mod sanitize;
mod storage;
#[cfg(test)]
mod tests;

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::process::{CancellationToken, RawLogSink};
use crate::redaction::RedactionPolicy;
use crate::status::StatusClass;

use self::sanitize::{PublishFailure, checksum_text, sanitize_file};
use self::storage::{
    TempWorkspace, cleanup_incomplete, ensure_absent, open_directory, reserve_lock,
    validate_parent_ancestry,
};

/// Error raised while reserving or publishing a local log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogError {
    message: String,
}

impl LogError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LogError {}

/// Metadata for a successfully committed sanitized log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedLog {
    path: PathBuf,
    bytes: u64,
    checksum: String,
    policy: RedactionPolicy,
}

impl PublishedLog {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    #[must_use]
    pub const fn policy(&self) -> RedactionPolicy {
        self.policy
    }
}

/// Result of the log publication commit attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogPublication {
    Published(PublishedLog),
    Failed {
        class: StatusClass,
        message: String,
        published: Option<PublishedLog>,
        lower_failure: Option<(StatusClass, String)>,
    },
}

impl LogPublication {
    #[must_use]
    pub const fn published(&self) -> Option<&PublishedLog> {
        match self {
            Self::Published(log) => Some(log),
            Self::Failed { published, .. } => published.as_ref(),
        }
    }

    #[must_use]
    pub fn failure(&self) -> Option<(StatusClass, &str)> {
        match self {
            Self::Published(_) => None,
            Self::Failed { class, message, .. } => Some((*class, message)),
        }
    }
    #[must_use]
    pub fn lower_failure(&self) -> Option<(StatusClass, &str)> {
        match self {
            Self::Published(_) => None,
            Self::Failed { lower_failure, .. } => lower_failure
                .as_ref()
                .map(|(class, message)| (*class, message.as_str())),
        }
    }
}

/// Exclusive writer for one sanitized destination path.
#[derive(Debug)]
pub struct LogPublisher {
    destination: PathBuf,
    destination_name: String,
    workspace: TempWorkspace,
    lock: Option<nix::fcntl::Flock<File>>,
    raw: Option<BufWriter<File>>,
    capture_failed: bool,
    finished: bool,
}

impl LogPublisher {
    /// Reserve a new destination and create a restrictive raw capture file.
    ///
    /// Existing destinations, symlinks, non-regular paths, and concurrent
    /// reservations are rejected before a child is launched.
    ///
    /// # Errors
    /// Returns an error when the destination cannot be exclusively reserved.
    pub fn prepare(path: impl Into<PathBuf>) -> Result<Self, LogError> {
        let requested_destination = path.into();
        let parent = requested_destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let Some(name) = requested_destination.file_name().and_then(OsStr::to_str) else {
            return Err(LogError::new("log path must name a file"));
        };
        let destination_name = name.to_owned();
        let parent = fs::canonicalize(&parent)
            .map_err(|error| LogError::new(format!("cannot resolve log directory: {error}")))?;
        let destination = parent.join(&destination_name);

        let parent_directory = open_directory(&parent)?;
        validate_parent_ancestry(&parent, &parent_directory)?;
        let lock = reserve_lock(&parent_directory, &destination_name)?;
        cleanup_incomplete(&parent, &parent_directory, &destination_name)?;
        ensure_absent(&destination)?;
        let workspace = TempWorkspace::create(&parent_directory, &destination_name)?;
        let raw_file = workspace.create_capture()?;

        Ok(Self {
            destination,
            destination_name,
            workspace,
            lock: Some(lock),
            raw: Some(BufWriter::new(raw_file)),
            capture_failed: false,
            finished: false,
        })
    }

    #[must_use]
    pub const fn capture_failed(&self) -> bool {
        self.capture_failed
    }
    #[cfg(test)]
    pub(super) fn capture_path_for_test(&self) -> PathBuf {
        let parent = match self.destination.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        parent
            .join(self.workspace.directory_name_for_test())
            .join("capture.tmp")
    }
    #[cfg(test)]
    pub(super) fn workspace_path_for_test(&self) -> PathBuf {
        let parent = match self.destination.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        parent.join(self.workspace.directory_name_for_test())
    }

    /// Publish the complete capture after sanitizing it through the policy.
    pub fn publish(
        &mut self,
        policy: RedactionPolicy,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> LogPublication {
        if self.finished {
            return self.failed(
                StatusClass::LogWrite,
                "log publisher is no longer active",
                None,
            );
        }
        if self.capture_failed {
            return self.failed(StatusClass::LogWrite, "log capture failed", None);
        }
        let raw_file = match self.finish_capture() {
            Ok(raw_file) => raw_file,
            Err(error) => {
                return self.failed(
                    StatusClass::LogWrite,
                    &format!("cannot finish temporary log: {error}"),
                    None,
                );
            }
        };
        if let Some(class) = stop_class(deadline, cancellation) {
            return self.failed(class, stop_message(class), None);
        }

        let sanitized_file = match self.workspace.create_publish() {
            Ok(file) => file,
            Err(error) => return self.failed(StatusClass::LogWrite, &error.to_string(), None),
        };

        let (bytes, checksum) =
            match sanitize_file(raw_file, sanitized_file, policy, deadline, cancellation) {
                Ok(result) => result,
                Err(PublishFailure::Stopped(class)) => {
                    return self.failed(class, stop_message(class), None);
                }
                Err(PublishFailure::Io(message)) => {
                    return self.failed(StatusClass::LogWrite, &message, None);
                }
            };
        if let Some(class) = stop_class(deadline, cancellation) {
            return self.failed(class, stop_message(class), None);
        }

        let published = PublishedLog {
            path: self.destination.clone(),
            bytes,
            checksum: checksum_text(checksum),
            policy,
        };
        if let Err(error) = self.workspace.publish(&self.destination_name) {
            return self.failed(
                StatusClass::LogWrite,
                &format!("cannot commit log: {error}"),
                None,
            );
        }

        let stop = stop_class(deadline, cancellation);
        let cleanup_error = self.cleanup();
        self.finished = true;
        if let Some(class) = stop {
            LogPublication::Failed {
                class,
                message: stop_message(class).to_owned(),
                published: Some(published),
                lower_failure: cleanup_error.map(|error| {
                    (
                        StatusClass::LogWrite,
                        format!("log committed but cleanup failed: {error}"),
                    )
                }),
            }
        } else if let Some(error) = cleanup_error {
            LogPublication::Failed {
                class: StatusClass::LogWrite,
                message: format!("log committed but cleanup failed: {error}"),
                published: Some(published),
                lower_failure: None,
            }
        } else {
            LogPublication::Published(published)
        }
    }

    /// Remove an incomplete capture and release the destination reservation.
    pub fn abort(&mut self) {
        if self.finished {
            return;
        }
        drop(self.raw.take());
        drop(self.cleanup());
        self.finished = true;
    }

    fn finish_capture(&mut self) -> io::Result<File> {
        let Some(mut raw) = self.raw.take() else {
            return Err(io::Error::other("temporary log is closed"));
        };
        raw.flush()?;
        raw.get_ref().sync_all()?;
        let mut raw_file = raw.into_inner().map_err(io::IntoInnerError::into_error)?;
        raw_file.seek(SeekFrom::Start(0))?;
        Ok(raw_file)
    }

    fn failed(
        &mut self,
        class: StatusClass,
        message: &str,
        published: Option<PublishedLog>,
    ) -> LogPublication {
        let cleanup_error = self.cleanup();
        self.finished = true;
        let lower_failure = if matches!(class, StatusClass::Timeout | StatusClass::Interrupt) {
            cleanup_error.as_ref().map(|error| {
                (
                    StatusClass::LogWrite,
                    format!("temporary log cleanup failed: {error}"),
                )
            })
        } else {
            None
        };
        let message = cleanup_error.map_or_else(
            || message.to_owned(),
            |error| format!("{message}; cleanup failed: {error}"),
        );
        LogPublication::Failed {
            class,
            message,
            published,
            lower_failure,
        }
    }

    fn cleanup(&mut self) -> Option<String> {
        let cleanup_error = self.workspace.cleanup();
        drop(self.lock.take());
        cleanup_error
    }
}

impl RawLogSink for LogPublisher {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        let result = self.raw.as_mut().map_or_else(
            || Err(io::Error::other("temporary log is closed")),
            |raw| raw.write_all(bytes),
        );
        if result.is_err() {
            self.capture_failed = true;
        }
        result
    }
}

impl Drop for LogPublisher {
    fn drop(&mut self) {
        if !self.finished {
            drop(self.raw.take());
            drop(self.cleanup());
        }
    }
}

fn stop_class(deadline: Option<Instant>, cancellation: &CancellationToken) -> Option<StatusClass> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Some(StatusClass::Timeout)
    } else if cancellation.is_cancelled() {
        Some(StatusClass::Interrupt)
    } else {
        None
    }
}

const fn stop_message(class: StatusClass) -> &'static str {
    match class {
        StatusClass::Timeout => "operation deadline exceeded",
        StatusClass::Interrupt => "operation interrupted",
        _ => "operation stopped",
    }
}
