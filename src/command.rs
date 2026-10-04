use std::ffi::OsString;
use std::fmt;
use std::time::Instant;

use crate::cli::{Action, Invocation, Tool};
use crate::discovery::{
    Discovery, DiscoveryContext, RootKind, check_deadline, discover_with_context,
};
use crate::discovery_error::DiscoveryError;
use crate::process::{CancellationToken, CommandSpec};
use crate::status::StatusClass;
use crate::tool_args::{
    arguments_before_child_separator, collect_path_options, rewrite_path_argument,
};

/// Command construction failures owned by the wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandBuildError {
    DeferredPostAction,
    Discovery(DiscoveryError),
}

impl fmt::Display for CommandBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeferredPostAction => formatter.write_str("post-actions are not available yet"),
            Self::Discovery(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandBuildError {}

/// Construct the direct child command represented by a parsed invocation.
///
/// Construction performs filesystem-only root discovery. It never launches a
/// tool or runs metadata commands, so it is also safe for `--plan`.
///
/// # Errors
/// Returns an error for an unresolved root.
pub fn command_for(invocation: &Invocation) -> Result<CommandSpec, CommandBuildError> {
    command_for_with_context(invocation, None, None)
}

/// Construct a command while enforcing one invocation-wide deadline.
///
/// # Errors
/// Returns an error when root discovery fails.
pub fn command_for_with_deadline(
    invocation: &Invocation,
    deadline: Option<Instant>,
) -> Result<CommandSpec, CommandBuildError> {
    command_for_with_context(invocation, deadline, None)
}

/// Construct a command with deadline and cooperative cancellation checks.
///
/// # Errors
/// Returns an error when discovery or cancellation fails.
pub fn command_for_with_cancellation(
    invocation: &Invocation,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<CommandSpec, CommandBuildError> {
    command_for_with_context(invocation, deadline, Some(cancellation))
}

fn command_for_with_context(
    invocation: &Invocation,
    deadline: Option<Instant>,
    cancellation: Option<&CancellationToken>,
) -> Result<CommandSpec, CommandBuildError> {
    let context = DiscoveryContext::new(deadline, cancellation);
    let discovery =
        discover_with_context(invocation, context).map_err(CommandBuildError::Discovery)?;
    check_deadline(context, discovery.root()).map_err(CommandBuildError::Discovery)?;
    let label = format!("{} {}", invocation.tool(), invocation.action());
    let (program, child_action, status_class) = command_mapping(invocation);
    let mut spec = CommandSpec::new(program)
        .arg(child_action)
        .label(label)
        .with_status_class(status_class)
        .current_dir(discovery.root().to_owned());

    if invocation.tool() == Tool::Xcode
        && !discovery.is_explicit()
        && let Some(marker) = discovery.marker()
    {
        let option = match discovery.kind() {
            RootKind::XcodeWorkspace => "-workspace",
            RootKind::XcodeProject => "-project",
            _ => "",
        };
        if !option.is_empty() {
            spec = spec.injected_arg(option);
            spec = spec.arg(marker.as_os_str().to_owned());
        }
    }

    let forwarded = normalized_forwarded_args(invocation, &discovery);
    for option in automatic_flags(invocation.tool(), invocation.action(), &forwarded) {
        spec = spec.injected_arg(option);
    }
    spec = spec.args(forwarded);

    if invocation.nightly {
        spec = spec.env("RUSTUP_TOOLCHAIN", "nightly-aarch64-apple-darwin");
    }
    Ok(spec)
}

fn normalized_forwarded_args(invocation: &Invocation, discovery: &Discovery) -> Vec<OsString> {
    let arguments = invocation.forwarded_args();
    if !discovery.is_explicit() {
        return arguments.to_vec();
    }
    let names = match invocation.tool() {
        Tool::Xcode => &["-workspace", "-project"][..],
        Tool::Swift => &["--package-path", "--manifest-path"][..],
        Tool::Cargo => &["--manifest-path"][..],
    };
    let explicit = collect_path_options(arguments, names);
    let Some(value) = explicit.first() else {
        return arguments.to_vec();
    };
    let Some(marker) = discovery.marker() else {
        return arguments.to_vec();
    };
    let path = if invocation.tool() == Tool::Swift && value.option == "--package-path" {
        discovery.root()
    } else {
        marker
    };
    rewrite_path_argument(arguments, &value.option, path)
}

const fn command_mapping(invocation: &Invocation) -> (&'static str, &'static str, StatusClass) {
    match (invocation.tool(), invocation.action()) {
        (Tool::Xcode, Action::Build) => ("xcodebuild", "build", StatusClass::Compile),
        (Tool::Xcode, Action::Test) => ("xcodebuild", "test", StatusClass::Test),
        (Tool::Xcode, Action::List) => ("xcodebuild", "-list", StatusClass::Compile),
        (Tool::Swift, Action::Build) => ("swift", "build", StatusClass::Compile),
        (Tool::Swift, Action::Test) => ("swift", "test", StatusClass::Test),
        (Tool::Swift, Action::Lint) => ("swiftlint", "lint", StatusClass::Lint),
        (Tool::Cargo, Action::Build) => ("cargo", "build", StatusClass::Compile),
        (Tool::Cargo, Action::Test) => ("cargo", "test", StatusClass::Test),
        (Tool::Cargo, Action::Fmt) => ("cargo", "fmt", StatusClass::Lint),
        (Tool::Cargo, Action::Package) => ("cargo", "package", StatusClass::Compile),
        (Tool::Cargo, Action::Clippy) => ("cargo", "clippy", StatusClass::Lint),
        _ => ("", "", StatusClass::Compile),
    }
}

fn automatic_flags(tool: Tool, action: Action, forwarded: &[OsString]) -> Vec<OsString> {
    match (tool, action) {
        (Tool::Xcode, Action::Build | Action::Test) if !has_user_verbosity(forwarded) => {
            vec![OsString::from("-quiet")]
        }
        (Tool::Swift, Action::Build | Action::Test) if !has_user_verbosity(forwarded) => {
            vec![OsString::from("--quiet")]
        }
        (Tool::Cargo, Action::Build | Action::Test | Action::Package | Action::Clippy) => {
            let mut flags = Vec::new();
            if !has_user_verbosity(forwarded) {
                flags.push(OsString::from("--quiet"));
            }
            if action != Action::Package && !has_long_option(forwarded, "--message-format") {
                flags.push(OsString::from(
                    "--message-format=json-diagnostic-rendered-ansi",
                ));
            }
            flags
        }
        _ => Vec::new(),
    }
}

/// Whether forwarded tool arguments request quiet or verbose output.
#[must_use]
pub fn has_user_verbosity(arguments: &[OsString]) -> bool {
    arguments_before_child_separator(arguments).any(|argument| {
        argument.to_str().is_some_and(|text| {
            ["--quiet", "-q", "--verbose", "-v"].contains(&text) || is_bundled_verbose(text)
        })
    })
}

fn is_bundled_verbose(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("-v") else {
        return false;
    };
    !suffix.is_empty() && suffix.chars().all(|character| character == 'v')
}

fn has_long_option(arguments: &[OsString], name: &str) -> bool {
    arguments_before_child_separator(arguments).any(|argument| {
        argument.to_str().is_some_and(|text| {
            text == name
                || text
                    .strip_prefix(name)
                    .is_some_and(|suffix| suffix.starts_with('='))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::cli::parse;
    fn invocation(args: &[&str]) -> Invocation {
        parse(std::iter::once("distill").chain(args.iter().copied())).expect("valid invocation")
    }

    #[test]
    fn maps_actions_and_injects_cargo_diagnostics_flags() {
        let invocation = invocation(&["cargo", "clippy", "--", "--all-targets"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(spec.program(), "cargo");
        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("clippy"),
                OsString::from("--quiet"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi"),
                OsString::from("--all-targets")
            ]
        );
        assert_eq!(spec.status_class(), StatusClass::Lint);
    }

    #[test]
    fn cargo_test_injects_diagnostic_flags_and_uses_test_status() {
        let invocation = invocation(&["cargo", "test", "--", "--all-targets"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("test"),
                OsString::from("--quiet"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi"),
                OsString::from("--all-targets")
            ]
        );
        assert_eq!(spec.status_class(), StatusClass::Test);
    }

    #[test]
    fn cargo_fmt_forwards_options_without_diagnostic_flags() {
        let invocation = invocation(&["cargo", "fmt", "--", "--check", "--all"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("fmt"),
                OsString::from("--check"),
                OsString::from("--all")
            ]
        );
        assert_eq!(spec.status_class(), StatusClass::Lint);
    }

    #[test]
    fn cargo_test_respects_user_verbosity_and_message_format() {
        let invocation = invocation(&["cargo", "test", "--", "--verbose", "--message-format=json"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("test"),
                OsString::from("--verbose"),
                OsString::from("--message-format=json")
            ]
        );
    }

    #[test]
    fn cargo_package_forwards_common_flags_without_json_injection() {
        let invocation = invocation(&["cargo", "package", "--", "--locked", "--offline"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("package"),
                OsString::from("--quiet"),
                OsString::from("--locked"),
                OsString::from("--offline")
            ]
        );
        assert_eq!(spec.status_class(), StatusClass::Compile);
    }

    #[test]
    fn cargo_nightly_sets_toolchain_environment_without_forwarding_flag() {
        let invocation = invocation(&["cargo", "build", "--nightly"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.environment(),
            &[(
                OsString::from("RUSTUP_TOOLCHAIN"),
                OsString::from("nightly-aarch64-apple-darwin")
            )]
        );
        assert!(!spec.arguments().contains(&OsString::from("--nightly")));
        assert_eq!(
            spec.injected_options(),
            &[
                OsString::from("--quiet"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi")
            ]
        );
    }

    #[test]
    fn forwards_resolved_cwd_and_user_verbosity_wins() {
        let invocation = invocation(&["--cwd", ".", "cargo", "build", "--", "--verbose"]);
        let spec = command_for(&invocation).expect("command should build");

        assert!(spec.cwd().is_some_and(std::path::Path::is_absolute));
        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("build"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi"),
                OsString::from("--verbose")
            ]
        );
    }

    #[test]
    fn does_not_duplicate_user_diagnostic_flags() {
        let invocation = invocation(&["cargo", "build", "--", "--quiet", "--message-format=json"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("build"),
                OsString::from("--quiet"),
                OsString::from("--message-format=json")
            ]
        );
    }

    #[test]
    fn swift_build_constructs_command_without_post_action() {
        let root = PathBuf::from("target").join(format!(
            "distill-command-post-action-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create Swift fixture");
        fs::write(root.join("Package.swift"), "// fixture\n").expect("write Swift manifest");
        let root_argument = root.to_string_lossy().into_owned();
        let invocation = invocation(&["--cwd", &root_argument, "swift", "build"]);
        let spec = command_for(&invocation).expect("build command should be constructed");
        assert_eq!(
            spec.arguments(),
            &[OsString::from("build"), OsString::from("--quiet")]
        );
        fs::remove_dir_all(root).expect("remove Swift fixture");
    }
    #[test]
    fn expired_deadline_stops_before_discovery() {
        let invocation = invocation(&["cargo", "build"]);
        let result = command_for_with_deadline(&invocation, Some(Instant::now()));
        assert!(matches!(
            result,
            Err(CommandBuildError::Discovery(DiscoveryError::Timeout { .. }))
        ));
    }

    #[test]
    fn bundled_verbosity_prevents_automatic_quiet() {
        let invocation = invocation(&["cargo", "build", "--", "-vv"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("build"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi"),
                OsString::from("-vv")
            ]
        );
    }

    #[test]
    fn cancellation_stops_before_discovery() {
        let invocation = invocation(&["cargo", "build"]);
        let cancellation = crate::process::CancellationToken::new();
        cancellation.cancel();
        let result = command_for_with_cancellation(&invocation, None, &cancellation);
        assert!(matches!(
            result,
            Err(CommandBuildError::Discovery(
                DiscoveryError::Interrupted { .. }
            ))
        ));
    }
}
