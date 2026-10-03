use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::digest::{Digest, Stream};
use crate::status::{Status, StatusClass, StatusError, StatusReport};

/// A direct child command. No shell is involved when it is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    label: String,
    status_class: StatusClass,
}

impl CommandSpec {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            label: "child".to_owned(),
            status_class: StatusClass::Compile,
        }
    }

    #[must_use]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.args.push(argument.into());
        self
    }

    #[must_use]
    pub fn args<I, T>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        self.args.extend(arguments.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub const fn with_status_class(mut self, status_class: StatusClass) -> Self {
        self.status_class = status_class;
        self
    }

    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    #[must_use]
    pub fn label_text(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn status_class(&self) -> StatusClass {
        self.status_class
    }
}
/// Receives bounded raw child-output chunks for complete-log capture.
pub trait RawLogSink {
    /// Store one chunk without retaining the complete child log in memory.
    ///
    /// # Errors
    /// Returns an I/O error when the chunk cannot be stored.
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()>;
}
/// Receives one bounded output line while a child is supervised.
pub trait OutputLineSink {
    /// Observe a decoded line without retaining the complete child output.
    ///
    /// # Errors
    /// Returns an I/O error when observation cannot continue.
    fn write_line(&mut self, line: &crate::digest::OutputLine) -> io::Result<()>;
}

impl<F> OutputLineSink for F
where
    F: FnMut(&crate::digest::OutputLine) -> io::Result<()>,
{
    fn write_line(&mut self, line: &crate::digest::OutputLine) -> io::Result<()> {
        self(line)
    }
}

/// Receives raw output chunks for sanitized live forwarding.
pub trait RawOutputSink {
    /// Forward one chunk from a child stream.
    ///
    /// # Errors
    /// Returns an I/O error when forwarding cannot continue.
    fn write_chunk(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()>;

    /// Flush one stream after its child pipe reaches EOF.
    ///
    /// # Errors
    /// Returns an I/O error when the final buffered output cannot be written.
    fn finish_stream(&mut self, _stream: Stream) -> io::Result<()> {
        Ok(())
    }
}

impl<F> RawOutputSink for F
where
    F: FnMut(Stream, &[u8]) -> io::Result<()>,
{
    fn write_chunk(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        self(stream, bytes)
    }
}

pub(super) struct CaptureSinks<'a> {
    pub(super) log: Option<&'a mut (dyn RawLogSink + 'a)>,
    pub(super) line: Option<&'a mut (dyn OutputLineSink + 'a)>,
    pub(super) output: Option<&'a mut (dyn RawOutputSink + 'a)>,
}

impl<'a> CaptureSinks<'a> {
    pub(super) const fn new() -> Self {
        Self {
            log: None,
            line: None,
            output: None,
        }
    }

    pub(super) fn with_log(mut self, sink: &'a mut (dyn RawLogSink + 'a)) -> Self {
        self.log = Some(sink);
        self
    }

    pub(super) fn with_lines(mut self, sink: &'a mut (dyn OutputLineSink + 'a)) -> Self {
        self.line = Some(sink);
        self
    }

    pub(super) fn with_output(mut self, sink: &'a mut (dyn RawOutputSink + 'a)) -> Self {
        self.output = Some(sink);
        self
    }
}

/// Classification of detached process creation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachedLaunchError {
    /// The operation deadline had already elapsed.
    Deadline,
    /// Cancellation was requested before launch.
    Interrupted,
    /// Executable could not be resolved through `PATH`.
    ToolMissing,
    /// A resolved executable could not be spawned.
    SpawnFailed,
}
/// Cooperative cancellation flag checked by the process supervisor.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Install a process-wide SIGINT handler that sets the supplied token.
///
/// # Errors
/// Returns an error when the operating system rejects signal registration.
pub fn install_interrupt_handler(token: &CancellationToken) -> io::Result<()> {
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&token.flag)).map(|_| ())
}
/// Captured child result, digest, completeness, and elapsed whole-operation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execution {
    report: StatusReport,
    digest: Digest,
    duration: Duration,
    message: Option<String>,
    capture_complete: bool,
}

impl Execution {
    pub(crate) const fn new(
        report: StatusReport,
        digest: Digest,
        duration: Duration,
        message: Option<String>,
        capture_complete: bool,
    ) -> Self {
        Self {
            report,
            digest,
            duration,
            message,
            capture_complete,
        }
    }

    /// Build a wrapper-owned execution when supervision fails before capture.
    ///
    /// # Errors
    /// Returns an error when the supplied class is not wrapper-owned.
    pub fn wrapper_failure(
        class: StatusClass,
        message: impl Into<String>,
        duration: Duration,
    ) -> Result<Self, ProcessError> {
        let status = Status::wrapper(class)?;
        Ok(Self::new(
            StatusReport::single(status),
            Digest::default(),
            duration,
            Some(message.into()),
            true,
        ))
    }

    #[must_use]
    pub const fn report(&self) -> &StatusReport {
        &self.report
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }
    /// Whether both child output streams reached EOF and were fully drained.
    #[must_use]
    pub const fn capture_complete(&self) -> bool {
        self.capture_complete
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    /// Add a wrapper-owned result while preserving all lower-precedence results.
    ///
    /// # Errors
    /// Returns an error when `class` is not a wrapper-owned classification.
    pub fn add_wrapper_status(
        &mut self,
        class: StatusClass,
        message: impl Into<String>,
    ) -> Result<(), ProcessError> {
        let status = Status::wrapper(class)?;
        let mut statuses = self.report.statuses().to_vec();
        statuses.push(status);
        let report = StatusReport::new(statuses)?;
        if report.final_status() == status {
            self.message = Some(message.into());
        }
        self.report = report;
        Ok(())
    }
    pub(crate) fn absorb_statuses(&mut self, other: &Self) -> Result<(), ProcessError> {
        self.merge_status_report(other)
    }

    pub(crate) fn absorb_output(&mut self, other: &Self) -> Result<(), ProcessError> {
        self.merge_status_report(other)?;
        self.digest.merge(&other.digest);
        Ok(())
    }

    fn merge_status_report(&mut self, other: &Self) -> Result<(), ProcessError> {
        let mut statuses = self.report.statuses().to_vec();
        statuses.extend_from_slice(other.report.statuses());
        let report = StatusReport::new(statuses)?;
        if report.final_status() != self.report.final_status() {
            self.message.clone_from(&other.message);
        }
        self.capture_complete &= other.capture_complete;
        self.report = report;
        Ok(())
    }

    /// Override elapsed whole-operation time after wrapper-owned phases finish.
    pub const fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
    }
}
/// Internal supervisor failures that prevent a complete execution result.
#[derive(Debug)]
pub enum ProcessError {
    Status(StatusError),
    MissingPipe(Stream),
    ReaderPanicked(Stream),
    SupervisorFailure,
    DeadlineOverflow,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(error) => error.fmt(formatter),
            Self::MissingPipe(stream) => write!(formatter, "{stream} pipe was unavailable"),
            Self::ReaderPanicked(stream) => write!(formatter, "{stream} reader panicked"),
            Self::SupervisorFailure => formatter.write_str("process supervisor failed"),
            Self::DeadlineOverflow => formatter.write_str("operation deadline overflowed"),
        }
    }
}

impl std::error::Error for ProcessError {}

impl From<StatusError> for ProcessError {
    fn from(error: StatusError) -> Self {
        Self::Status(error)
    }
}
