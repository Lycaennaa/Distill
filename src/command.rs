use std::ffi::OsString;
use std::fmt;
use std::time::Instant;

use crate::cli::{Action, Invocation, Tool};
use crate::discovery::{
    Discovery, DiscoveryContext, RootKind, check_deadline, discover_with_context,
};
use crate::discovery_error::DiscoveryError;
use crate::gradle::{gradle_option_takes_value, is_gradle_test_action_component};
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
    let (status_class, mut spec) = if invocation.tool() == Tool::Gradle {
        (
            gradle_status_class(invocation),
            gradle_wrapper_command(&discovery),
        )
    } else {
        let (program, child_action, status_class) = command_mapping(invocation);
        (status_class, CommandSpec::new(program).arg(child_action))
    };
    spec = spec
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
        spec = spec.env("RUSTUP_TOOLCHAIN", "nightly");
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
        Tool::Gradle => &[][..],
        Tool::Cargo if invocation.action() == Action::Install => &["--path"][..],
        Tool::Cargo => &["--manifest-path"][..],
    };
    let explicit = collect_path_options(arguments, names);
    let Some(value) = explicit.first() else {
        return arguments.to_vec();
    };
    let Some(marker) = discovery.marker() else {
        return arguments.to_vec();
    };
    let path = if matches!(value.option.as_str(), "--package-path" | "--path") {
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
        (Tool::Cargo, Action::Install) => ("cargo", "install", StatusClass::Compile),
        (Tool::Cargo, Action::Clippy) => ("cargo", "clippy", StatusClass::Lint),
        (Tool::Cargo, Action::Xtask) => ("cargo", "xtask", StatusClass::Compile),
        _ => ("", "", StatusClass::Compile),
    }
}
fn gradle_status_class(invocation: &Invocation) -> StatusClass {
    if has_gradle_test_task(invocation.forwarded_args()) {
        StatusClass::Test
    } else {
        StatusClass::Compile
    }
}

fn has_gradle_test_task(arguments: &[OsString]) -> bool {
    let mut consume_value = false;
    for argument in arguments {
        if consume_value {
            consume_value = false;
            continue;
        }
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument == "--" {
            break;
        }
        if argument.starts_with('-') {
            consume_value = gradle_option_takes_value(argument);
            continue;
        }
        if argument
            .rsplit(':')
            .next()
            .is_some_and(is_gradle_test_action_component)
        {
            return true;
        }
    }
    false
}

#[cfg(windows)]
fn gradle_wrapper_command(discovery: &Discovery) -> CommandSpec {
    let java = std::env::var_os("JAVA_HOME")
        .filter(|home| !home.is_empty())
        .map_or_else(
            || OsString::from("java"),
            |home| gradle_java_executable(home),
        );
    let java_options = std::env::var_os("JAVA_OPTS")
        .map_or_else(String::new, |value| value.to_string_lossy().into_owned());
    let gradle_options = std::env::var_os("GRADLE_OPTS")
        .map_or_else(String::new, |value| value.to_string_lossy().into_owned());
    CommandSpec::new(java)
        .args(gradle_wrapper_jvm_options(&java_options, &gradle_options))
        .arg("-Dorg.gradle.appname=gradlew")
        .arg("-classpath")
        .arg(
            discovery
                .root()
                .join("gradle/wrapper/gradle-wrapper.jar")
                .into_os_string(),
        )
        .arg("org.gradle.wrapper.GradleWrapperMain")
}
#[cfg(any(windows, test))]
fn gradle_java_executable(home: OsString) -> OsString {
    let home = std::path::PathBuf::from(home);
    if home.to_str().is_some_and(|path| path.contains('"')) {
        std::path::PathBuf::from(home.to_string_lossy().replace('"', ""))
            .join("bin/java.exe")
            .into_os_string()
    } else {
        home.join("bin/java.exe").into_os_string()
    }
}
#[cfg(any(windows, test))]
const GRADLE_WINDOWS_DEFAULT_JVM_OPTIONS: [&str; 3] =
    ["-Dfile.encoding=UTF-8", "-Xmx64m", "-Xms64m"];

#[cfg(any(windows, test))]
fn gradle_wrapper_jvm_options(java_options: &str, gradle_options: &str) -> Vec<OsString> {
    let mut options = GRADLE_WINDOWS_DEFAULT_JVM_OPTIONS
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    options.extend(split_jvm_options(java_options));
    options.extend(split_jvm_options(gradle_options));
    options
}

#[cfg(any(windows, test))]
fn split_jvm_options(value: &str) -> Vec<OsString> {
    let mut options = Vec::new();
    let mut current = String::new();
    let mut characters = value.chars().peekable();
    let mut quoted = false;
    let mut argument_started = false;

    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                let mut slashes = 1_usize;
                while characters.peek() == Some(&'\\') {
                    characters.next();
                    slashes = slashes.saturating_add(1);
                }
                if characters.peek() == Some(&'"') {
                    characters.next();
                    current.extend(std::iter::repeat_n('\\', slashes / 2));
                    if slashes.is_multiple_of(2) {
                        quoted = !quoted;
                    } else {
                        current.push('"');
                    }
                } else {
                    current.extend(std::iter::repeat_n('\\', slashes));
                }
                argument_started = true;
            }
            '"' => {
                quoted = !quoted;
                argument_started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if argument_started {
                    options.push(OsString::from(std::mem::take(&mut current)));
                    argument_started = false;
                }
            }
            character => {
                current.push(character);
                argument_started = true;
            }
        }
    }
    if argument_started {
        options.push(current.into());
    }
    options
}

#[cfg(not(windows))]
fn gradle_wrapper_command(discovery: &Discovery) -> CommandSpec {
    CommandSpec::new(discovery.root().join("gradlew").into_os_string())
}

fn automatic_flags(tool: Tool, action: Action, forwarded: &[OsString]) -> Vec<OsString> {
    match (tool, action) {
        (Tool::Xcode, Action::Build | Action::Test) if !has_user_verbosity(forwarded) => {
            vec![OsString::from("-quiet")]
        }
        (Tool::Swift, Action::Build | Action::Test) if !has_user_verbosity(forwarded) => {
            vec![OsString::from("--quiet")]
        }
        (
            Tool::Cargo,
            Action::Build | Action::Test | Action::Package | Action::Clippy | Action::Install,
        ) => {
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
    fn cargo_xtask_forwards_task_arguments_without_injected_flags() {
        let invocation = invocation(&["cargo", "xtask", "ci", "--all"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(spec.program(), "cargo");
        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("xtask"),
                OsString::from("ci"),
                OsString::from("--all")
            ]
        );
        assert_eq!(spec.injected_options(), Vec::<OsString>::new());
        assert_eq!(spec.status_class(), StatusClass::Compile);
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
    fn cargo_install_adds_diagnostic_flags_and_forwards_path() {
        let root =
            PathBuf::from("target").join(format!("distill-command-install-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).expect("create Cargo fixture");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .expect("write Cargo manifest");
        let root = fs::canonicalize(root).expect("canonicalize Cargo fixture");
        let root_argument = root.to_string_lossy().into_owned();
        let cargo_invocation = invocation(&[
            "--cwd",
            &root_argument,
            "cargo",
            "install",
            "--",
            "--path",
            ".",
            "--locked",
        ]);
        let spec = command_for(&cargo_invocation).expect("install command should build");

        assert_eq!(spec.cwd(), Some(root.as_path()));
        assert_eq!(
            spec.arguments(),
            &[
                OsString::from("install"),
                OsString::from("--quiet"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi"),
                OsString::from("--path"),
                root.as_os_str().to_os_string(),
                OsString::from("--locked")
            ]
        );
        assert_eq!(
            spec.injected_options(),
            &[
                OsString::from("--quiet"),
                OsString::from("--message-format=json-diagnostic-rendered-ansi")
            ]
        );
        assert_eq!(spec.status_class(), StatusClass::Compile);
        let custom_invocation = invocation(&[
            "--cwd",
            &root_argument,
            "cargo",
            "install",
            "--",
            "--path",
            ".",
            "--message-format=json",
        ]);
        let custom_spec = command_for(&custom_invocation).expect("custom message format");
        assert!(
            custom_spec
                .arguments()
                .contains(&OsString::from("--message-format=json"))
        );
        assert!(!custom_spec.arguments().contains(&OsString::from(
            "--message-format=json-diagnostic-rendered-ansi"
        )));
        fs::remove_dir_all(root).expect("remove Cargo fixture");
    }

    #[test]
    fn cargo_nightly_sets_toolchain_environment_without_forwarding_flag() {
        let invocation = invocation(&["cargo", "build", "--nightly"]);
        let spec = command_for(&invocation).expect("command should build");

        assert_eq!(
            spec.environment(),
            &[(
                OsString::from("RUSTUP_TOOLCHAIN"),
                OsString::from("nightly")
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
    #[test]
    fn gradle_test_action_detects_later_tasks_and_ignores_non_tasks() {
        let clean_then_test = invocation(&["gradle", "clean", "test"]);
        assert_eq!(gradle_status_class(&clean_then_test), StatusClass::Test);

        let build_with_test_filter =
            invocation(&["gradle", "assembleDebug", "--tests", "example.FooTest"]);
        assert_eq!(
            gradle_status_class(&build_with_test_filter),
            StatusClass::Compile
        );

        let test_named_project = invocation(&["gradle", ":test:assemble"]);
        assert_eq!(
            gradle_status_class(&test_named_project),
            StatusClass::Compile
        );

        let excluded_test = invocation(&["gradle", "build", "-x", "test"]);
        assert_eq!(gradle_status_class(&excluded_test), StatusClass::Compile);

        let project_cache_named_test =
            invocation(&["gradle", "build", "--project-cache-dir", "test"]);
        assert_eq!(
            gradle_status_class(&project_cache_named_test),
            StatusClass::Compile
        );
        let help_task_detail = invocation(&["gradle", "help", "--task", "test"]);
        assert_eq!(gradle_status_class(&help_task_detail), StatusClass::Compile);
        let build_with_short_gradle_home = invocation(&["gradle", "build", "-g", "test"]);
        assert_eq!(
            gradle_status_class(&build_with_short_gradle_home),
            StatusClass::Compile
        );

        let build_with_gradle_home = invocation(&["gradle", "build", "--gradle-user-home", "test"]);
        assert_eq!(
            gradle_status_class(&build_with_gradle_home),
            StatusClass::Compile
        );

        let build_with_lock_update = invocation(&["gradle", "build", "--update-locks", "test"]);
        assert_eq!(
            gradle_status_class(&build_with_lock_update),
            StatusClass::Compile
        );
    }
    #[cfg(unix)]
    #[test]
    fn gradle_classifier_consumes_non_utf8_option_operand() {
        use std::os::unix::ffi::OsStringExt;

        let arguments = vec![
            OsString::from("clean"),
            OsString::from("--exclude-task"),
            OsString::from_vec(vec![0xff]),
            OsString::from("test"),
        ];
        assert!(has_gradle_test_task(&arguments));
    }
    #[test]
    fn gradle_wrapper_options_preserve_defaults_and_quoted_arguments() {
        let options = gradle_wrapper_jvm_options(
            r#"-Xmx2g -Dmessage="hello world" -Dpath=C:\Gradle\cache"#,
            r#"-Dflag=\"yes\""#,
        );
        assert_eq!(
            options,
            vec![
                OsString::from("-Dfile.encoding=UTF-8"),
                OsString::from("-Xmx64m"),
                OsString::from("-Xms64m"),
                OsString::from("-Xmx2g"),
                OsString::from("-Dmessage=hello world"),
                OsString::from(r"-Dpath=C:\Gradle\cache"),
                OsString::from(r#"-Dflag="yes""#),
            ]
        );
    }
    #[test]
    fn gradle_java_executable_strips_batch_script_quotes_from_java_home() {
        let java_home = OsString::from(r#""C:\Program Files\Java\jdk-21""#);
        assert_eq!(
            gradle_java_executable(java_home),
            std::path::PathBuf::from(r"C:\Program Files\Java\jdk-21")
                .join("bin/java.exe")
                .into_os_string()
        );
    }
}
