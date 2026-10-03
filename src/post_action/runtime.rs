use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::{Invocation, PostAction, RuntimeMode, Tool};
use crate::digest::Stream;
use crate::process::{
    CancellationToken, CommandSpec, DetachedLaunchError, Execution, RawOutputSink, execute_until,
    execute_until_with_output, spawn_detached_until,
};
use crate::redaction::{RedactionPolicy, redact};
use crate::status::StatusClass;

use super::cargo;
use super::products::{BuiltProduct, Failure};
use super::wrapper_failure;

const MAX_STREAM_LINE_BYTES: usize = 4_194_304;
const STDERR_QUEUE_CAPACITY: usize = 16;
const STDERR_POLL_INTERVAL: Duration = Duration::from_millis(50);

enum StderrMessage {
    Write {
        bytes: Vec<u8>,
        completion: SyncSender<io::Result<()>>,
        deadline: Option<Instant>,
        cancellation: CancellationToken,
    },
}

static STDERR_WRITER: OnceLock<Option<SyncSender<StderrMessage>>> = OnceLock::new();

fn stderr_writer() -> io::Result<&'static SyncSender<StderrMessage>> {
    STDERR_WRITER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(STDERR_QUEUE_CAPACITY);
            let worker = thread::Builder::new()
                .name("distill-stderr".to_owned())
                .spawn(move || {
                    while let Ok(StderrMessage::Write {
                        bytes,
                        completion,
                        deadline,
                        cancellation,
                    }) = receiver.recv()
                    {
                        let result = if deadline.is_some_and(|value| Instant::now() >= value) {
                            Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "stderr forwarding deadline exceeded",
                            ))
                        } else if cancellation.is_cancelled() {
                            Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "stderr forwarding interrupted",
                            ))
                        } else {
                            let mut stderr = io::stderr().lock();
                            stderr.write_all(&bytes).and_then(|()| stderr.flush())
                        };
                        let _completion_result = completion.send(result);
                    }
                });
            worker.ok().map(|_worker| sender)
        })
        .as_ref()
        .ok_or_else(|| io::Error::other("stderr forwarding worker could not start"))
}

pub(super) fn run(
    invocation: &Invocation,
    post_action: &PostAction,
    build: &CommandSpec,
    product: &BuiltProduct,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
) -> Result<Option<Execution>, Failure> {
    let Some(mode) = post_action.runtime_mode() else {
        return Err(wrapper_failure(
            StatusClass::Artifact,
            "runtime action is unavailable",
        ));
    };
    let spec = runtime_spec(invocation, build, product)?;
    if mode == RuntimeMode::Detached {
        return spawn_detached(invocation, build, product, &spec, deadline, cancellation);
    }
    let mut execution = if mode == RuntimeMode::Stream {
        let mut sink = SanitizedStderr::new(policy, deadline, cancellation);
        execute_until_with_output(&spec, deadline, cancellation, &mut sink).map_err(|_error| {
            wrapper_failure(
                StatusClass::PostAction,
                "runtime output could not be streamed",
            )
        })?
    } else {
        execute_until(&spec, deadline, cancellation).map_err(|_error| {
            wrapper_failure(
                StatusClass::PostAction,
                "runtime process could not be supervised",
            )
        })?
    };
    if mode == RuntimeMode::Stream && !execution.capture_complete() {
        execution
            .add_wrapper_status(
                StatusClass::PostAction,
                "runtime output could not be streamed",
            )
            .map_err(|_error| {
                wrapper_failure(
                    StatusClass::GenericWrapper,
                    "runtime result aggregation failed",
                )
            })?;
    }
    Ok(Some(execution))
}

fn runtime_spec(
    invocation: &Invocation,
    build: &CommandSpec,
    product: &BuiltProduct,
) -> Result<CommandSpec, Failure> {
    if invocation.tool() == Tool::Cargo {
        let package = product.package().ok_or_else(|| {
            wrapper_failure(
                StatusClass::Artifact,
                "Cargo package selection was unavailable",
            )
        })?;
        return cargo::run_spec(invocation, build, package, product.name());
    }
    let mut spec = CommandSpec::new(product.executable().as_os_str().to_owned())
        .label("product runtime")
        .with_status_class(StatusClass::Runtime);
    if let Some(root) = build.cwd() {
        spec = spec.current_dir(root.to_owned());
    }
    Ok(spec)
}

fn spawn_detached(
    invocation: &Invocation,
    build: &CommandSpec,
    product: &BuiltProduct,
    runtime: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<Option<Execution>, Failure> {
    let spec = if invocation.tool() == Tool::Xcode {
        let mut open = CommandSpec::new("open")
            .args([
                std::ffi::OsString::from("-a"),
                product.path().as_os_str().to_owned(),
            ])
            .label("open app")
            .with_status_class(StatusClass::Runtime);
        if let Some(root) = build.cwd() {
            open = open.current_dir(root.to_owned());
        }
        open
    } else {
        runtime.clone()
    };
    match spawn_detached_until(&spec, deadline, cancellation) {
        Ok(()) => Ok(None),
        Err(error) => Err(detached_launch_failure(invocation.tool(), error)),
    }
}

fn detached_launch_failure(tool: Tool, error: DetachedLaunchError) -> Failure {
    let class = match error {
        DetachedLaunchError::Deadline => StatusClass::Timeout,
        DetachedLaunchError::Interrupted => StatusClass::Interrupt,
        DetachedLaunchError::ToolMissing if tool != Tool::Xcode => StatusClass::ToolMissing,
        DetachedLaunchError::SpawnFailed if tool != Tool::Xcode => StatusClass::Launch,
        DetachedLaunchError::ToolMissing | DetachedLaunchError::SpawnFailed => {
            StatusClass::PostAction
        }
    };
    let message = match error {
        DetachedLaunchError::Deadline => "operation deadline exceeded",
        DetachedLaunchError::Interrupted => "operation interrupted",
        DetachedLaunchError::ToolMissing => "runtime executable was not found",
        DetachedLaunchError::SpawnFailed => "runtime process could not be launched",
    };
    wrapper_failure(class, message)
}

struct SanitizedStderr<'a> {
    policy: RedactionPolicy,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    deadline: Option<Instant>,
    cancellation: &'a CancellationToken,
}

impl<'a> SanitizedStderr<'a> {
    const fn new(
        policy: RedactionPolicy,
        deadline: Option<Instant>,
        cancellation: &'a CancellationToken,
    ) -> Self {
        Self {
            policy,
            stdout: Vec::new(),
            stderr: Vec::new(),
            deadline,
            cancellation,
        }
    }

    const fn pending(&mut self, stream: Stream) -> &mut Vec<u8> {
        match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        }
    }

    fn write_sanitized(&self, bytes: &[u8], newline: bool) -> io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        let sanitized = redact(text.trim_end_matches('\n'), self.policy);
        let mut output = sanitized.into_bytes();
        if newline {
            output.push(b'\n');
        }
        if self.deadline.is_some_and(|value| Instant::now() >= value) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "stderr forwarding deadline exceeded",
            ));
        }
        if self.cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stderr forwarding interrupted",
            ));
        }
        let (completion, result) = mpsc::sync_channel(1);
        stderr_writer()?
            .try_send(StderrMessage::Write {
                bytes: output,
                completion,
                deadline: self.deadline,
                cancellation: self.cancellation.clone(),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "stderr forwarding queue is full")
                }
                TrySendError::Disconnected(_) => {
                    io::Error::other("stderr forwarding worker stopped")
                }
            })?;
        loop {
            if self.cancellation.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stderr forwarding interrupted",
                ));
            }
            let wait = if let Some(deadline) = self.deadline {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .unwrap_or_default();
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "stderr forwarding deadline exceeded",
                    ));
                }
                remaining.min(STDERR_POLL_INTERVAL)
            } else {
                STDERR_POLL_INTERVAL
            };
            match result.recv_timeout(wait) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("stderr forwarding worker stopped"));
                }
            }
        }
    }
}

impl RawOutputSink for SanitizedStderr<'_> {
    fn write_chunk(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        let lines = {
            let pending = self.pending(stream);
            pending.extend_from_slice(bytes);
            let mut lines = Vec::new();
            while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                let count = newline.saturating_add(1);
                lines.push(pending.drain(..count).collect::<Vec<_>>());
            }
            lines
        };
        for line in lines {
            self.write_sanitized(&line, true)?;
        }
        if self.pending(stream).len() > MAX_STREAM_LINE_BYTES {
            return Err(io::Error::other(
                "runtime output line exceeds sanitizer limit",
            ));
        }
        Ok(())
    }

    fn finish_stream(&mut self, stream: Stream) -> io::Result<()> {
        let pending = std::mem::take(self.pending(stream));
        if !pending.is_empty() {
            self.write_sanitized(&pending, false)?;
        }
        Ok(())
    }
}
