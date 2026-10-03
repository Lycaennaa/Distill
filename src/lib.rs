//! Process execution, tool adapters, discovery, and deterministic digests for `distill`.

pub(crate) mod acl;
pub mod artifact;
pub mod cli;
pub mod command;
pub mod digest;
pub mod discovery;
pub(crate) mod discovery_error;
pub mod log;
pub mod post_action;
pub mod process;
pub mod record;
pub mod redaction;
pub mod status;
pub(crate) mod tool_args;

pub(crate) mod xcode_discovery;
pub use artifact::{Artifact, ArtifactKind, ArtifactState};
pub use cli::{
    Action, Invocation, PostAction, PostActionKind, RuntimeMode, Tool, WrapperOptions, parse,
};
pub use command::{
    CommandBuildError, command_for, command_for_with_cancellation, command_for_with_deadline,
};
pub use digest::{
    Diagnostic, DiagnosticGroup, DiagnosticLocation, Digest, FALLBACK_LINE_LIMIT, ListSection,
    ListSectionKind, OutputLine, Severity, Stream, TestFailure, TestSummary, XcodeDigest,
    compact_text, parse_diagnostic, parse_diagnostics,
};
pub use discovery::{Discovery, RootKind, discover};
pub use discovery_error::DiscoveryError;
pub use process::{
    CancellationToken, CommandSpec, DetachedLaunchError, Execution, OutputLineSink, ProcessError,
    RawLogSink, RawOutputSink, execute, execute_until, execute_until_with_lines,
    execute_until_with_output, execute_with_log, execute_with_log_until, install_interrupt_handler,
    spawn_detached_until,
};
pub use record::{Record, RecordKind, escape_field};
pub use redaction::{RedactionPolicy, redact};
pub use status::{Owner, Status, StatusClass, StatusReport};
