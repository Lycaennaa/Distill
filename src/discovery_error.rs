use std::fmt;
use std::path::PathBuf;

use crate::cli::{Action, Tool};

/// Root resolution failures that happen before any child launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryError {
    StartDirectory {
        path: PathBuf,
        reason: String,
    },
    Timeout {
        path: PathBuf,
    },
    Interrupted {
        path: PathBuf,
    },
    Io {
        path: PathBuf,
        reason: String,
    },
    Missing {
        tool: Tool,
        action: Action,
        start: PathBuf,
    },
    Ambiguous {
        tool: Tool,
        action: Action,
        choices: Vec<PathBuf>,
    },
    InvalidExplicit {
        tool: Tool,
        action: Action,
        option: String,
        path: PathBuf,
        reason: String,
    },
    ConflictingExplicit {
        tool: Tool,
        action: Action,
        options: Vec<String>,
    },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartDirectory { path, reason } => {
                write!(
                    formatter,
                    "cannot use discovery directory {}: {reason}",
                    path.display()
                )
            }
            Self::Timeout { path } => {
                write!(formatter, "discovery timed out from {}", path.display())
            }
            Self::Interrupted { path } => {
                write!(formatter, "discovery interrupted from {}", path.display())
            }
            Self::Io { path, reason } => {
                write!(formatter, "cannot inspect {}: {reason}", path.display())
            }
            Self::Missing {
                tool,
                action,
                start,
            } => write!(
                formatter,
                "no {tool} {action} root found from {}",
                start.display()
            ),
            Self::Ambiguous {
                tool,
                action,
                choices,
            } => {
                let choices = choices
                    .iter()
                    .map(|choice| choice.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(formatter, "multiple {tool} {action} roots found: {choices}")
            }
            Self::InvalidExplicit {
                tool,
                action,
                option,
                path,
                reason,
            } => write!(
                formatter,
                "invalid {tool} {action} {option} path {}: {reason}",
                path.display()
            ),
            Self::ConflictingExplicit {
                tool,
                action,
                options,
            } => write!(
                formatter,
                "conflicting explicit {tool} {action} paths: {}",
                options.join(", ")
            ),
        }
    }
}

impl std::error::Error for DiscoveryError {}
