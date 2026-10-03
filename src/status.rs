use std::fmt;

/// Identifies whether a result belongs to the wrapped child or the wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    Child,
    Wrapper,
}

impl fmt::Display for Owner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Child => "child",
            Self::Wrapper => "wrapper",
        })
    }
}

/// Contract classifications for final results and retained lower-precedence results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StatusClass {
    Success,
    Compile,
    Test,
    Lint,
    Runtime,
    ToolMissing,
    Launch,
    Signaled,
    Timeout,
    Interrupt,
    Usage,
    Discovery,
    Artifact,
    PostAction,
    LogWrite,
    GenericWrapper,
}

impl StatusClass {
    const fn is_child(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Compile | Self::Test | Self::Lint | Self::Runtime
        )
    }

    const fn is_wrapper(self) -> bool {
        matches!(
            self,
            Self::ToolMissing
                | Self::Launch
                | Self::Timeout
                | Self::Interrupt
                | Self::Usage
                | Self::Discovery
                | Self::Artifact
                | Self::PostAction
                | Self::LogWrite
                | Self::GenericWrapper
        )
    }
}

impl fmt::Display for StatusClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Success => "success",
            Self::Compile => "compile",
            Self::Test => "test",
            Self::Lint => "lint",
            Self::Runtime => "runtime",
            Self::ToolMissing => "tool_missing",
            Self::Launch => "launch",
            Self::Signaled => "signaled",
            Self::Timeout => "timeout",
            Self::Interrupt => "interrupt",
            Self::Usage => "usage",
            Self::Discovery => "discovery",
            Self::Artifact => "artifact",
            Self::PostAction => "post_action",
            Self::LogWrite => "log_write",
            Self::GenericWrapper => "generic_wrapper",
        })
    }
}

/// Invalid status construction, kept explicit so owner/class collisions cannot be silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusError {
    ChildClassUsedForWrapper,
    WrapperClassUsedForChild,
    SignaledRequiresSignal,
    SuccessWithFailureCode,
    ExitCodeOutOfRange,
    InvalidSignal,
    EmptyReport,
}

impl fmt::Display for StatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ChildClassUsedForWrapper => "child-owned status class used as wrapper status",
            Self::WrapperClassUsedForChild => "wrapper-owned status class used as child status",
            Self::SignaledRequiresSignal => "signaled status requires a signal constructor",
            Self::SuccessWithFailureCode => "success status cannot carry a nonzero exit code",
            Self::ExitCodeOutOfRange => "child exit code must be between 0 and 255",
            Self::InvalidSignal => "signal must be between 1 and 127",
            Self::EmptyReport => "status report must contain at least one status",
        })
    }
}

impl std::error::Error for StatusError {}

/// Canonical owner/class/exit-code tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    owner: Owner,
    class: StatusClass,
    code: i32,
}

impl Status {
    /// Build a normal child result. Exit code zero always becomes `success`.
    ///
    /// # Errors
    /// Returns an error when a wrapper-owned class, signaled class, or invalid exit code is supplied.
    pub fn child(class: StatusClass, code: i32) -> Result<Self, StatusError> {
        if class == StatusClass::Signaled {
            return Err(StatusError::SignaledRequiresSignal);
        }
        if !class.is_child() {
            return Err(StatusError::WrapperClassUsedForChild);
        }
        if !(0..=255).contains(&code) {
            return Err(StatusError::ExitCodeOutOfRange);
        }
        if class == StatusClass::Success && code != 0 {
            return Err(StatusError::SuccessWithFailureCode);
        }

        Ok(Self {
            owner: Owner::Child,
            class: if code == 0 {
                StatusClass::Success
            } else {
                class
            },
            code,
        })
    }

    /// Build a child result terminated by a Unix signal.
    ///
    /// # Errors
    /// Returns an error when the signal is outside the Unix range 1 through 127.
    pub fn child_signaled(signal: u8) -> Result<Self, StatusError> {
        if !(1..=127).contains(&signal) {
            return Err(StatusError::InvalidSignal);
        }
        let Some(code) = 128_i32.checked_add(i32::from(signal)) else {
            return Err(StatusError::ExitCodeOutOfRange);
        };
        Ok(Self {
            owner: Owner::Child,
            class: StatusClass::Signaled,
            code,
        })
    }

    /// Build a wrapper-owned result using the locked status-code mapping.
    ///
    /// # Errors
    /// Returns an error when a child-owned class is supplied.
    pub const fn wrapper(class: StatusClass) -> Result<Self, StatusError> {
        if !class.is_wrapper() {
            return Err(StatusError::ChildClassUsedForWrapper);
        }
        let code = match class {
            StatusClass::Timeout => 124,
            StatusClass::Interrupt => 130,
            StatusClass::ToolMissing | StatusClass::Launch => 127,
            StatusClass::Usage => 2,
            StatusClass::Discovery
            | StatusClass::Artifact
            | StatusClass::PostAction
            | StatusClass::LogWrite
            | StatusClass::GenericWrapper => 1,
            _ => return Err(StatusError::ChildClassUsedForWrapper),
        };
        Ok(Self {
            owner: Owner::Wrapper,
            class,
            code,
        })
    }

    #[must_use]
    pub const fn owner(self) -> Owner {
        self.owner
    }

    #[must_use]
    pub const fn class(self) -> StatusClass {
        self.class
    }

    #[must_use]
    pub const fn code(self) -> i32 {
        self.code
    }

    #[must_use]
    pub fn is_success(self) -> bool {
        self.class == StatusClass::Success && self.code == 0
    }
}

/// All statuses observed during one operation, with precedence-resolved final status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    statuses: Vec<Status>,
    final_status: Status,
}

impl StatusReport {
    /// Build a report and resolve timeout/interrupt, wrapper, then child precedence.
    ///
    /// # Errors
    /// Returns an error when no statuses were observed.
    pub fn new(statuses: Vec<Status>) -> Result<Self, StatusError> {
        let Some(final_status) = statuses.iter().copied().min_by_key(|status| {
            (
                status_precedence(status.class()),
                status.class(),
                status.owner(),
            )
        }) else {
            return Err(StatusError::EmptyReport);
        };
        Ok(Self {
            statuses,
            final_status,
        })
    }

    #[must_use]
    pub fn single(status: Status) -> Self {
        Self {
            statuses: vec![status],
            final_status: status,
        }
    }

    #[must_use]
    pub const fn final_status(&self) -> Status {
        self.final_status
    }

    #[must_use]
    pub fn statuses(&self) -> &[Status] {
        &self.statuses
    }
}

const fn status_precedence(class: StatusClass) -> u8 {
    match class {
        StatusClass::Timeout => 0,
        StatusClass::Interrupt => 1,
        StatusClass::ToolMissing
        | StatusClass::Launch
        | StatusClass::Usage
        | StatusClass::Discovery
        | StatusClass::Artifact
        | StatusClass::PostAction
        | StatusClass::LogWrite
        | StatusClass::GenericWrapper => 2,
        StatusClass::Compile
        | StatusClass::Test
        | StatusClass::Lint
        | StatusClass::Runtime
        | StatusClass::Signaled => 3,
        StatusClass::Success => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_status_preserves_child_exit_code_and_owner() {
        let status = Status::child(StatusClass::Compile, 2).expect("valid child status");
        assert_eq!(status.owner(), Owner::Child);
        assert_eq!(status.class(), StatusClass::Compile);
        assert_eq!(status.code(), 2);
    }

    #[test]
    fn zero_child_exit_is_success_even_when_warnings_exist() {
        let status = Status::child(StatusClass::Compile, 0).expect("valid child status");
        assert_eq!(status.owner(), Owner::Child);
        assert_eq!(status.class(), StatusClass::Success);
        assert!(status.is_success());
    }

    #[test]
    fn wrapper_mapping_is_explicit() {
        assert_eq!(Status::wrapper(StatusClass::Usage).unwrap().code(), 2);
        assert_eq!(Status::wrapper(StatusClass::Timeout).unwrap().code(), 124);
        assert_eq!(Status::wrapper(StatusClass::Interrupt).unwrap().code(), 130);
        assert_eq!(
            Status::wrapper(StatusClass::ToolMissing).unwrap().code(),
            127
        );
        assert_eq!(Status::wrapper(StatusClass::Launch).unwrap().code(), 127);
        assert_eq!(Status::wrapper(StatusClass::Discovery).unwrap().code(), 1);
        assert_eq!(Status::wrapper(StatusClass::LogWrite).unwrap().code(), 1);
    }

    #[test]
    fn signaled_status_uses_signal_exit_mapping() {
        let status = Status::child_signaled(9).expect("valid signal");
        assert_eq!(status.owner(), Owner::Child);
        assert_eq!(status.class(), StatusClass::Signaled);
        assert_eq!(status.code(), 137);
    }

    #[test]
    fn status_constructors_reject_owner_collisions() {
        assert_eq!(
            Status::child(StatusClass::Usage, 2),
            Err(StatusError::WrapperClassUsedForChild)
        );
        assert_eq!(
            Status::child(StatusClass::Signaled, 137),
            Err(StatusError::SignaledRequiresSignal)
        );
        assert_eq!(
            Status::wrapper(StatusClass::Compile),
            Err(StatusError::ChildClassUsedForWrapper)
        );
    }

    #[test]
    fn report_preserves_lower_precedence_statuses() {
        let child = Status::child(StatusClass::Test, 2).expect("valid child status");
        let wrapper = Status::wrapper(StatusClass::LogWrite).expect("valid wrapper status");
        let timeout = Status::wrapper(StatusClass::Timeout).expect("valid timeout status");
        let report = StatusReport::new(vec![child, wrapper, timeout]).expect("valid report");

        assert_eq!(report.final_status(), timeout);
        assert_eq!(report.statuses(), &[child, wrapper, timeout]);
    }

    #[test]
    fn child_runtime_failure_beats_successful_build_status() {
        let build = Status::child(StatusClass::Success, 0).expect("valid build status");
        let runtime = Status::child(StatusClass::Runtime, 7).expect("valid runtime status");
        let report = StatusReport::new(vec![build, runtime]).expect("valid report");

        assert_eq!(report.final_status(), runtime);
        assert_eq!(report.statuses(), &[build, runtime]);
    }

    #[test]
    fn report_rejects_empty_status_lists() {
        assert_eq!(StatusReport::new(Vec::new()), Err(StatusError::EmptyReport));
    }
}
