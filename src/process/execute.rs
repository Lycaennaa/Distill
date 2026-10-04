use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::launch::resolve_program;
use super::model::{
    CancellationToken, CaptureSinks, CommandSpec, Execution, ProcessError, RawLogSink,
};
use super::supervisor::{CaptureCompleteness, is_expired, supervise_child};
use super::termination::StopReason;
use crate::digest::Digest;
use crate::status::{Status, StatusClass, StatusReport};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

/// Execute one direct child with concurrent bounded capture and cancellation.
///
/// Captured output is reduced as it arrives. Only diagnostic groups and the
/// last 20 lines of each stream remain in memory.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
) -> Result<Execution, ProcessError> {
    let started = Instant::now();
    let deadline = timeout.and_then(|duration| started.checked_add(duration));
    if timeout.is_some() && deadline.is_none() {
        return Err(ProcessError::DeadlineOverflow);
    }
    execute_inner(spec, started, deadline, cancellation, CaptureSinks::new())
}

/// Execute one child against an invocation-wide absolute deadline.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute_until(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<Execution, ProcessError> {
    execute_inner(
        spec,
        Instant::now(),
        deadline,
        cancellation,
        CaptureSinks::new(),
    )
}

/// Execute a child and observe decoded output lines during supervision.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute_until_with_lines(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    sink: &mut dyn super::model::OutputLineSink,
) -> Result<Execution, ProcessError> {
    execute_inner(
        spec,
        Instant::now(),
        deadline,
        cancellation,
        CaptureSinks::new().with_lines(sink),
    )
}

/// Execute a child and forward raw output chunks to a supervised sink.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute_until_with_output(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    sink: &mut dyn super::model::RawOutputSink,
) -> Result<Execution, ProcessError> {
    execute_inner(
        spec,
        Instant::now(),
        deadline,
        cancellation,
        CaptureSinks::new().with_output(sink),
    )
}

/// Execute a child while teeing bounded raw chunks into a complete-log sink.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute_with_log(
    spec: &CommandSpec,
    timeout: Option<Duration>,
    cancellation: &CancellationToken,
    sink: &mut dyn RawLogSink,
) -> Result<Execution, ProcessError> {
    let started = Instant::now();
    let deadline = timeout.and_then(|duration| started.checked_add(duration));
    if timeout.is_some() && deadline.is_none() {
        return Err(ProcessError::DeadlineOverflow);
    }
    execute_inner(
        spec,
        started,
        deadline,
        cancellation,
        CaptureSinks::new().with_log(sink),
    )
}

/// Execute with log capture and an invocation-wide absolute deadline.
///
/// # Errors
/// Returns an error when the supervisor cannot construct a valid result.
pub fn execute_with_log_until(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    sink: &mut dyn RawLogSink,
) -> Result<Execution, ProcessError> {
    execute_inner(
        spec,
        Instant::now(),
        deadline,
        cancellation,
        CaptureSinks::new().with_log(sink),
    )
}

fn execute_inner(
    spec: &CommandSpec,
    started: Instant,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    sinks: CaptureSinks<'_>,
) -> Result<Execution, ProcessError> {
    let empty_digest = Digest::default();

    if is_expired(deadline) {
        return wrapper_execution(
            StatusClass::Timeout,
            "operation deadline exceeded",
            started,
            empty_digest,
        );
    }
    if cancellation.is_cancelled() {
        return wrapper_execution(
            StatusClass::Interrupt,
            "operation interrupted",
            started,
            empty_digest,
        );
    }

    let Some(program) = resolve_program(spec.program()) else {
        return wrapper_execution(
            StatusClass::ToolMissing,
            "tool not found",
            started,
            empty_digest,
        );
    };
    if is_expired(deadline) {
        return wrapper_execution(
            StatusClass::Timeout,
            "operation deadline exceeded",
            started,
            empty_digest,
        );
    }
    if cancellation.is_cancelled() {
        return wrapper_execution(
            StatusClass::Interrupt,
            "operation interrupted",
            started,
            empty_digest,
        );
    }

    let mut command = Command::new(program);
    command
        .args(spec.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in spec.environment() {
        command.env(key, value);
    }
    if let Some(cwd) = spec.cwd() {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(_error) => {
            return wrapper_execution(
                StatusClass::Launch,
                "child launch failed",
                started,
                empty_digest,
            );
        }
    };
    let supervised = supervise_child(child, deadline, cancellation, sinks)?;
    let mut statuses = Vec::new();
    if let Some(status) = supervised.status {
        statuses.push(status_from_exit(status, spec.status_class())?);
    }
    if supervised.wrapper_failure || supervised.cleanup_failure || statuses.is_empty() {
        statuses.push(Status::wrapper(StatusClass::GenericWrapper)?);
    }
    if supervised.log_failure && supervised.stop_reason != Some(StopReason::LogFailure) {
        statuses.push(Status::wrapper(StatusClass::LogWrite)?);
    }
    if let Some(reason) = supervised.stop_reason
        && let Some(class) = reason.status_class()
    {
        statuses.push(Status::wrapper(class)?);
    }
    let report = StatusReport::new(statuses)?;
    let message = report_message(&report);
    Ok(Execution::new(
        report,
        supervised.digest,
        started.elapsed(),
        message,
        matches!(
            supervised.capture_completeness,
            CaptureCompleteness::Complete
        ),
    ))
}

fn wrapper_execution(
    class: StatusClass,
    message: &str,
    started: Instant,
    digest: Digest,
) -> Result<Execution, ProcessError> {
    let status = Status::wrapper(class)?;
    Ok(Execution::new(
        StatusReport::single(status),
        digest,
        started.elapsed(),
        Some(message.to_owned()),
        true,
    ))
}

fn report_message(report: &StatusReport) -> Option<String> {
    match report.final_status().class() {
        StatusClass::ToolMissing => Some("tool not found".to_owned()),
        StatusClass::Launch => Some("child launch failed".to_owned()),
        StatusClass::Timeout => Some("operation deadline exceeded".to_owned()),
        StatusClass::Interrupt => Some("operation interrupted".to_owned()),
        StatusClass::LogWrite => Some("log capture failed".to_owned()),
        StatusClass::GenericWrapper => Some("output capture failed".to_owned()),
        _ => None,
    }
}

fn status_from_exit(status: ExitStatus, class: StatusClass) -> Result<Status, ProcessError> {
    if let Some(code) = status.code() {
        return Status::child(class, code).map_err(ProcessError::from);
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        let signal = u8::try_from(signal).map_err(|_| ProcessError::DeadlineOverflow)?;
        return Status::child_signaled(signal).map_err(ProcessError::from);
    }
    Err(ProcessError::DeadlineOverflow)
}
