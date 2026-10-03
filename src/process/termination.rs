use std::process::Child;
use std::time::{Duration, Instant};

use crate::status::StatusClass;
#[cfg(unix)]
use nix::sys::signal::{Signal, killpg};
#[cfg(unix)]
use nix::unistd::Pid;
const TERMINATION_GRACE: Duration = Duration::from_secs(5);

const CLEANUP_LIMIT: Duration = Duration::from_secs(10);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StopReason {
    Timeout,
    Interrupt,
    CaptureFailure,
    LogFailure,
    NormalCleanup,
}

impl StopReason {
    pub(super) const fn status_class(self) -> Option<StatusClass> {
        match self {
            Self::Timeout => Some(StatusClass::Timeout),
            Self::Interrupt => Some(StatusClass::Interrupt),
            Self::CaptureFailure => Some(StatusClass::GenericWrapper),
            Self::LogFailure => Some(StatusClass::LogWrite),
            Self::NormalCleanup => None,
        }
    }

    pub(super) const fn signal(self) -> GroupSignal {
        match self {
            Self::Timeout | Self::CaptureFailure | Self::LogFailure | Self::NormalCleanup => {
                GroupSignal::Terminate
            }
            Self::Interrupt => GroupSignal::Interrupt,
        }
    }

    pub(super) const fn priority(self) -> u8 {
        match self {
            Self::Timeout => 0,
            Self::Interrupt => 1,
            Self::CaptureFailure | Self::LogFailure => 2,
            Self::NormalCleanup => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupSignalResult {
    Sent,
    Gone,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum GroupSignal {
    Terminate,
    Interrupt,
    Kill,
}
pub(super) fn begin_termination(
    child: &mut Child,
    pid: u32,
    reason: StopReason,
) -> (Option<Instant>, Option<Instant>, bool, bool) {
    let cleanup_deadline = Instant::now().checked_add(CLEANUP_LIMIT);
    match send_group_signal(pid, reason.signal()) {
        GroupSignalResult::Gone => (None, cleanup_deadline, true, false),
        GroupSignalResult::Sent => (
            Instant::now().checked_add(TERMINATION_GRACE),
            cleanup_deadline,
            false,
            false,
        ),
        GroupSignalResult::Failed => {
            drop(child.kill());
            (
                Instant::now().checked_add(TERMINATION_GRACE),
                cleanup_deadline,
                false,
                true,
            )
        }
    }
}

pub(super) fn force_child(child: &mut Child, pid: u32) -> GroupSignalResult {
    let result = send_group_signal(pid, GroupSignal::Kill);
    if result == GroupSignalResult::Failed {
        drop(child.kill());
    }
    result
}

#[cfg(unix)]
pub(super) fn send_group_signal(pid: u32, signal: GroupSignal) -> GroupSignalResult {
    let Ok(pid) = i32::try_from(pid) else {
        return GroupSignalResult::Failed;
    };
    let signal = match signal {
        GroupSignal::Terminate => Signal::SIGTERM,
        GroupSignal::Interrupt => Signal::SIGINT,
        GroupSignal::Kill => Signal::SIGKILL,
    };
    match killpg(Pid::from_raw(pid), signal) {
        Ok(()) => GroupSignalResult::Sent,
        Err(nix::errno::Errno::ESRCH) => GroupSignalResult::Gone,
        Err(_error) => GroupSignalResult::Failed,
    }
}

#[cfg(not(unix))]
pub(super) fn send_group_signal(_pid: u32, _signal: GroupSignal) -> GroupSignalResult {
    GroupSignalResult::Failed
}
