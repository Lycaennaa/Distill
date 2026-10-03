use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::artifact::ArtifactKind;
use crate::digest::OutputLine;
use crate::process::{
    CancellationToken, CommandSpec, Execution, OutputLineSink, ProcessError,
    execute_until_with_lines,
};
use crate::status::StatusClass;

use super::wrapper_failure;

const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum BuiltProduct {
    XcodeApp {
        name: String,
        scheme: Option<String>,
        path: PathBuf,
        executable: PathBuf,
    },
    SwiftExecutable {
        name: String,
        path: PathBuf,
    },
    CargoBinary {
        package: String,
        name: String,
        path: PathBuf,
    },
}

impl BuiltProduct {
    pub(super) const fn kind(&self) -> ArtifactKind {
        match self {
            Self::XcodeApp { .. } => ArtifactKind::App,
            Self::SwiftExecutable { .. } | Self::CargoBinary { .. } => ArtifactKind::Binary,
        }
    }

    pub(super) fn name(&self) -> &str {
        match self {
            Self::XcodeApp { name, .. }
            | Self::SwiftExecutable { name, .. }
            | Self::CargoBinary { name, .. } => name,
        }
    }

    pub(super) fn scheme(&self) -> Option<&str> {
        match self {
            Self::XcodeApp { scheme, .. } => scheme.as_deref(),
            Self::SwiftExecutable { .. } | Self::CargoBinary { .. } => None,
        }
    }

    pub(super) fn package(&self) -> Option<&str> {
        match self {
            Self::CargoBinary { package, .. } => Some(package),
            Self::XcodeApp { .. } | Self::SwiftExecutable { .. } => None,
        }
    }

    pub(super) fn path(&self) -> &Path {
        match self {
            Self::XcodeApp { path, .. }
            | Self::SwiftExecutable { path, .. }
            | Self::CargoBinary { path, .. } => path,
        }
    }

    pub(super) fn executable(&self) -> &Path {
        match self {
            Self::XcodeApp { executable, .. } => executable,
            Self::SwiftExecutable { path, .. } | Self::CargoBinary { path, .. } => path,
        }
    }

    pub(super) fn relocate_app(&mut self, destination: PathBuf) {
        if let Self::XcodeApp {
            path, executable, ..
        } = self
        {
            let relative = executable
                .strip_prefix(path.as_path())
                .unwrap_or(executable)
                .to_owned();
            *path = destination;
            *executable = path.join(relative);
        }
    }
}

#[derive(Debug)]
pub(super) struct Failure {
    pub(super) class: StatusClass,
    pub(super) message: String,
}

pub(super) fn artifact_failure(message: impl Into<String>) -> Failure {
    wrapper_failure(StatusClass::Artifact, message)
}

fn capture_text(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(Execution, String), ProcessError> {
    let mut capture = TextCapture::default();
    let execution = execute_until_with_lines(spec, deadline, cancellation, &mut capture)?;
    if capture.invalid {
        return Ok((execution, String::new()));
    }
    Ok((execution, capture.text))
}

#[must_use]
pub(super) fn metadata_spec(
    program: impl Into<OsString>,
    arguments: impl IntoIterator<Item = OsString>,
    root: &Path,
) -> CommandSpec {
    CommandSpec::new(program)
        .args(arguments)
        .label("post-action metadata")
        .with_status_class(StatusClass::Runtime)
        .current_dir(root.to_owned())
}

pub(super) fn query_metadata(
    destination: &mut Execution,
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    failure_message: &str,
) -> Result<String, Failure> {
    let (metadata, text) = capture_text(spec, deadline, cancellation)
        .map_err(|_error| artifact_failure(failure_message))?;
    if metadata.report().final_status().is_success() && metadata.capture_complete() {
        return Ok(text);
    }
    destination
        .absorb_statuses(&metadata)
        .map_err(|_error| artifact_failure("status aggregation failed"))?;
    Err(artifact_failure(failure_message))
}

pub(super) fn parse_option_values(
    arguments: &[OsString],
    long: &str,
    short: Option<&str>,
) -> Result<Vec<String>, Failure> {
    let mut values = Vec::new();
    let mut index = 0_usize;
    while index < arguments.len() {
        let Some(argument) = arguments.get(index).and_then(|value| value.to_str()) else {
            index = index.saturating_add(1);
            continue;
        };
        if argument == "--" {
            break;
        }
        if argument == long || short.is_some_and(|name| argument == name) {
            let Some(value) = arguments
                .get(index.saturating_add(1))
                .and_then(|value| value.to_str())
            else {
                return Err(artifact_failure("product selector is missing a value"));
            };
            values.push(value.to_owned());
            index = index.saturating_add(2);
            continue;
        }
        if let Some(value) = argument.strip_prefix(&format!("{long}=")) {
            values.push(value.to_owned());
        } else if let Some(short) = short
            && let Some(value) = argument.strip_prefix(short)
            && !value.is_empty()
        {
            values.push(value.to_owned());
        }
        index = index.saturating_add(1);
    }
    Ok(values)
}

pub(super) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn single_selector(values: &[String]) -> Result<Option<&str>, Failure> {
    if values.len() > 1 {
        return Err(artifact_failure("only one product selector is supported"));
    }
    Ok(values.first().map(String::as_str))
}

#[derive(Default)]
struct TextCapture {
    text: String,
    invalid: bool,
}

impl OutputLineSink for TextCapture {
    fn write_line(&mut self, line: &OutputLine) -> io::Result<()> {
        if line.stream() != crate::digest::Stream::Stdout {
            return Ok(());
        }
        if line.text().contains("[structured diagnostic truncated]")
            || self
                .text
                .len()
                .saturating_add(line.text().len())
                .saturating_add(1)
                > MAX_METADATA_BYTES
        {
            self.invalid = true;
            return Err(io::Error::other("metadata output exceeds capture limit"));
        }
        self.text.push_str(line.text());
        self.text.push('\n');
        Ok(())
    }
}

pub(super) fn parse_json(text: &str, message: &str) -> Result<Value, Failure> {
    serde_json::from_str(text).map_err(|_error| artifact_failure(message))
}
