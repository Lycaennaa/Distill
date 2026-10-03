use std::process::{Child, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use super::capture::{CaptureState, EVENT_CHANNEL_CAPACITY, ReaderEvent, spawn_reader};
use super::model::{CancellationToken, CaptureSinks, ProcessError};
use super::termination::{
    GroupSignal, GroupSignalResult, StopReason, begin_termination, force_child, send_group_signal,
};
use crate::digest::{Digest, Stream};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureCompleteness {
    Complete,
    Incomplete,
}

pub(super) struct SupervisedChild {
    pub(super) status: Option<ExitStatus>,
    pub(super) digest: Digest,
    pub(super) capture_completeness: CaptureCompleteness,
    pub(super) wrapper_failure: bool,
    pub(super) cleanup_failure: bool,
    pub(super) log_failure: bool,
    pub(super) stop_reason: Option<StopReason>,
}

pub(super) fn supervise_child(
    mut child: Child,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    sinks: CaptureSinks<'_>,
) -> Result<SupervisedChild, ProcessError> {
    let pid = child.id();
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child, pid);
        return Err(ProcessError::MissingPipe(Stream::Stdout));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child, pid);
        return Err(ProcessError::MissingPipe(Stream::Stderr));
    };

    let capture_raw = sinks.log.is_some() || sinks.output.is_some();
    let (sender, receiver) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    let capture_stop = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(
        Stream::Stdout,
        stdout,
        sender.clone(),
        Arc::clone(&capture_stop),
        capture_raw,
    );
    let stderr_reader = spawn_reader(
        Stream::Stderr,
        stderr,
        sender,
        Arc::clone(&capture_stop),
        capture_raw,
    );
    let mut sinks = sinks;
    let mut state = match supervise_loop(
        &mut child,
        pid,
        deadline,
        cancellation,
        &receiver,
        &mut sinks,
    ) {
        Ok(state) => state,
        Err(error) => {
            capture_stop.store(true, Ordering::Relaxed);
            drop(stdout_reader.join());
            drop(stderr_reader.join());
            return Err(error);
        }
    };
    if state.cleanup_failure {
        capture_stop.store(true, Ordering::Relaxed);
    }
    if stdout_reader.join().is_err() {
        state.capture.mark_wrapper_failure();
    }
    if stderr_reader.join().is_err() {
        state.capture.mark_wrapper_failure();
    }
    let capture_completeness = if state.capture.streams_done()
        && !state.cleanup_failure
        && !state.capture.wrapper_failure()
    {
        CaptureCompleteness::Complete
    } else {
        CaptureCompleteness::Incomplete
    };

    Ok(SupervisedChild {
        status: state.child_status,
        digest: state.capture.take_digest(),
        capture_completeness,
        wrapper_failure: state.capture.wrapper_failure(),
        cleanup_failure: state.cleanup_failure,
        log_failure: state.capture.log_failure(),
        stop_reason: state.stop_reason,
    })
}

struct SupervisorState {
    capture: CaptureState,
    cleanup_failure: bool,
    child_status: Option<ExitStatus>,
    stop_reason: Option<StopReason>,
    termination_deadline: Option<Instant>,
    cleanup_deadline: Option<Instant>,
    force_sent: bool,
}

fn supervise_loop(
    child: &mut Child,
    pid: u32,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    receiver: &Receiver<ReaderEvent>,
    sinks: &mut CaptureSinks<'_>,
) -> Result<SupervisorState, ProcessError> {
    let mut state = SupervisorState {
        capture: CaptureState::default(),
        cleanup_failure: false,
        child_status: None,
        stop_reason: None,
        termination_deadline: None,
        cleanup_deadline: None,
        force_sent: false,
    };

    loop {
        state.capture.drain(receiver, sinks);
        if state.child_status.is_none()
            && let Some(status) = poll_child(child, pid)?
        {
            state.child_status = Some(status);
        }
        request_stop_if_needed(child, pid, deadline, cancellation, &mut state);
        force_after_grace(child, pid, &mut state);

        let finished = operation_finished(pid, &state);
        if state.stop_reason.is_some()
            && state
                .cleanup_deadline
                .is_none_or(|value| Instant::now() >= value)
            && !finished
        {
            state.cleanup_failure = true;
            if state.child_status.is_none()
                && let Some(status) = reap_child_bounded(child, pid)
            {
                state.child_status = Some(status);
            }
            return Ok(state);
        }
        if finished {
            return Ok(state);
        }
        let wait = wait_interval(
            deadline,
            state.termination_deadline,
            state.cleanup_deadline,
            state.capture.channel_closed(),
        );
        if state.capture.channel_closed() {
            thread::sleep(wait);
            continue;
        }
        match receiver.recv_timeout(wait) {
            Ok(event) => state.capture.handle(event, sinks),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => state.capture.close_channel(),
        }
    }
}

fn reap_child_bounded(child: &mut Child, pid: u32) -> Option<ExitStatus> {
    let _group_result = force_child(child, pid);
    let deadline = Instant::now().checked_add(POLL_INTERVAL)?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) | Err(_) => return None,
        }
    }
}

fn poll_child(child: &mut Child, pid: u32) -> Result<Option<ExitStatus>, ProcessError> {
    match child.try_wait() {
        Ok(status) => Ok(status),
        Err(_error) => {
            terminate_child(child, pid);
            Err(ProcessError::SupervisorFailure)
        }
    }
}

fn terminate_child(child: &mut Child, pid: u32) {
    let _result = reap_child_bounded(child, pid);
}

fn request_stop_if_needed(
    child: &mut Child,
    pid: u32,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    state: &mut SupervisorState,
) {
    let reason = if is_expired(deadline) {
        Some(StopReason::Timeout)
    } else if cancellation.is_cancelled() {
        Some(StopReason::Interrupt)
    } else if state.capture.log_failure() {
        Some(StopReason::LogFailure)
    } else if state.capture.wrapper_failure() {
        Some(StopReason::CaptureFailure)
    } else if state.child_status.is_some() && state.stop_reason.is_none() {
        Some(StopReason::NormalCleanup)
    } else {
        None
    };

    let Some(reason) = reason else {
        return;
    };
    if state
        .stop_reason
        .is_some_and(|current| reason.priority() >= current.priority())
    {
        return;
    }

    let (grace_deadline, cleanup_deadline, forced, signal_failed) =
        begin_termination(child, pid, reason);
    state.stop_reason = Some(reason);
    state.termination_deadline = grace_deadline;
    state.cleanup_deadline = cleanup_deadline;
    state.force_sent = forced;
    if signal_failed {
        state.capture.mark_wrapper_failure();
    }
}

fn force_after_grace(child: &mut Child, pid: u32, state: &mut SupervisorState) {
    if state.stop_reason.is_some()
        && !state.force_sent
        && state
            .termination_deadline
            .is_some_and(|value| Instant::now() >= value)
    {
        if matches!(force_child(child, pid), GroupSignalResult::Failed) {
            state.capture.mark_wrapper_failure();
        }
        state.force_sent = true;
        state.termination_deadline = None;
    }
}

const fn capture_finished(state: &SupervisorState) -> bool {
    state.capture.is_finished()
}

fn operation_finished(pid: u32, state: &SupervisorState) -> bool {
    if state.child_status.is_none() || !capture_finished(state) {
        return false;
    }
    let Some(reason) = state.stop_reason else {
        return false;
    };
    let signal = if state.force_sent {
        GroupSignal::Kill
    } else {
        reason.signal()
    };
    matches!(send_group_signal(pid, signal), GroupSignalResult::Gone)
}

fn wait_interval(
    deadline: Option<Instant>,
    termination_deadline: Option<Instant>,
    cleanup_deadline: Option<Instant>,
    channel_closed: bool,
) -> Duration {
    let mut interval = POLL_INTERVAL;
    if termination_deadline.is_none()
        && cleanup_deadline.is_none()
        && let Some(deadline) = deadline
    {
        interval = interval.min(deadline.saturating_duration_since(Instant::now()));
    }
    if let Some(deadline) = termination_deadline {
        interval = interval.min(deadline.saturating_duration_since(Instant::now()));
    }
    if let Some(deadline) = cleanup_deadline {
        interval = interval.min(deadline.saturating_duration_since(Instant::now()));
    }
    if channel_closed && interval.is_zero() {
        Duration::from_millis(1)
    } else {
        interval
    }
}

pub(super) fn is_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|value| Instant::now() >= value)
}
