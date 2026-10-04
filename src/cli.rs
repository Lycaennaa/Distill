use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
mod post_action;
use post_action::normalize_post_action;
pub use post_action::{PostAction, PostActionKind, RuntimeMode};

/// Strict top-level command parser. Clap supplies generated help and version output.
#[derive(Debug, Parser)]
#[command(
    name = "distill",
    version,
    about = "Build digests",
    after_help = "Example: distill --plan swift build",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    options: CliOptions,

    #[command(subcommand)]
    command: ToolCommand,
}

#[derive(Debug, Clone, Args)]
struct ActionArgs {
    /// Forward args after `--`.
    #[arg(last = true, value_name = "ARG", allow_hyphen_values = true)]
    forwarded: Vec<OsString>,
}
#[derive(Debug, Clone, Args)]
struct CargoActionArgs {
    /// Use the nightly-aarch64-apple-darwin Rust toolchain.
    #[arg(long)]
    nightly: bool,

    #[command(flatten)]
    action: ActionArgs,
}

#[derive(Debug, Clone, Subcommand)]
enum ToolCommand {
    /// Run Xcode.
    #[command(
        subcommand,
        after_help = "Xcode build options:\n  --open    Launch the built app\n  --mv      Move to /Applications\n  --omv     Move to /Applications and launch\n  --wait    Wait for app exit; requires --open or --omv\n  --stream  Stream app output to stderr; implies --wait"
    )]
    Xcode(XcodeCommand),
    /// Run Swift.
    #[command(subcommand)]
    Swift(SwiftCommand),
    /// Run Cargo.
    #[command(subcommand)]
    Cargo(CargoCommand),
}

#[derive(Debug, Clone, Subcommand)]
enum XcodeCommand {
    #[command(
        after_help = "Xcode build options:\n  --open    Launch the built app\n  --mv      Move to /Applications\n  --omv     Move to /Applications and launch\n  --wait    Wait for app exit; requires --open or --omv\n  --stream  Stream app output to stderr; implies --wait"
    )]
    Build(ActionArgs),
    Test(ActionArgs),
    /// List Xcode metadata.
    List(ActionArgs),
}

#[derive(Debug, Clone, Subcommand)]
enum SwiftCommand {
    Build(ActionArgs),
    Test(ActionArgs),
    Lint(ActionArgs),
}

#[derive(Debug, Clone, Subcommand)]
enum CargoCommand {
    Build(CargoActionArgs),
    Test(CargoActionArgs),
    Fmt(CargoActionArgs),
    Package(CargoActionArgs),
    Clippy(CargoActionArgs),
}

/// Non-action wrapper settings retained by the validated invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapperOptions {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) save_log: Option<PathBuf>,
    pub(crate) plan: bool,
}

/// Raw wrapper flags parsed before they are normalized into `PostAction`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "Clap represents each independent flag before normalization"
)]
#[derive(Debug, Clone, Default, Args, PartialEq, Eq)]
struct CliOptions {
    /// Set discovery directory.
    #[arg(long, global = true, value_name = "PATH")]
    cwd: Option<PathBuf>,

    /// Deadline (ms/s/m/h).
    #[arg(long, global = true, value_name = "DUR", value_parser = parse_duration)]
    timeout: Option<Duration>,

    /// Xcode build only: launch the built app.
    #[arg(long, global = true, hide = true)]
    open: bool,

    /// Xcode-only: move to /Applications.
    #[arg(long, global = true, hide = true)]
    mv: bool,

    /// Xcode-only: move and launch.
    #[arg(long, global = true, hide = true)]
    omv: bool,

    /// Xcode build only: wait for app exit.
    #[arg(long, global = true, hide = true)]
    wait: bool,

    /// Xcode build only: stream app output to stderr; implies --wait.
    #[arg(long, global = true, hide = true)]
    stream: bool,

    /// Save sanitized log.
    #[arg(long, global = true, value_name = "PATH")]
    save_log: Option<PathBuf>,

    /// No tools; print plan.
    #[arg(long, global = true)]
    plan: bool,
}

/// Supported wrapped tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Xcode,
    Swift,
    Cargo,
}

impl std::fmt::Display for Tool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Xcode => "xcode",
            Self::Swift => "swift",
            Self::Cargo => "cargo",
        })
    }
}

/// Strict action set accepted by each tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Build,
    Test,
    List,
    Lint,
    Clippy,
    Fmt,
    Package,
}

impl std::fmt::Display for Action {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::List => "list",
            Self::Lint => "lint",
            Self::Clippy => "clippy",
            Self::Fmt => "fmt",
            Self::Package => "package",
        })
    }
}

/// Parsed invocation with filesystem-only options and one validated post-action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub(crate) tool: Tool,
    pub(crate) action: Action,
    pub(crate) options: WrapperOptions,
    pub(crate) post_action: Option<PostAction>,
    pub(crate) forwarded_args: Vec<OsString>,
    pub(crate) nightly: bool,
}

impl Invocation {
    #[must_use]
    pub const fn tool(&self) -> Tool {
        self.tool
    }

    #[must_use]
    pub const fn action(&self) -> Action {
        self.action
    }

    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.options.timeout
    }

    #[must_use]
    pub const fn is_plan(&self) -> bool {
        self.options.plan
    }

    #[must_use]
    pub(crate) const fn options(&self) -> &WrapperOptions {
        &self.options
    }

    #[must_use]
    pub const fn post_action(&self) -> Option<&PostAction> {
        self.post_action.as_ref()
    }

    #[must_use]
    pub fn forwarded_args(&self) -> &[OsString] {
        &self.forwarded_args
    }

    #[must_use]
    pub fn waits_for_runtime(&self) -> bool {
        self.post_action
            .as_ref()
            .is_some_and(PostAction::waits_for_runtime)
    }

    #[must_use]
    pub fn save_log_path(&self) -> Option<&std::path::Path> {
        self.options.save_log.as_deref()
    }

    /// Return action label without exposing runtime argument values.
    #[must_use]
    pub const fn planned_post_action(&self) -> Option<&'static str> {
        match self.post_action.as_ref() {
            Some(action) => Some(action.label()),
            None => None,
        }
    }
}

/// Parse and semantically validate one command line.
///
/// # Errors
/// Returns a Clap error for invalid syntax or incompatible wrapper flags.
pub fn parse<I, T>(arguments: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(arguments)?;
    let (tool, action, forwarded_args, nightly) = match cli.command {
        ToolCommand::Xcode(command) => match command {
            XcodeCommand::Build(args) => (Tool::Xcode, Action::Build, args.forwarded, false),
            XcodeCommand::Test(args) => (Tool::Xcode, Action::Test, args.forwarded, false),
            XcodeCommand::List(args) => (Tool::Xcode, Action::List, args.forwarded, false),
        },
        ToolCommand::Swift(command) => match command {
            SwiftCommand::Build(args) => (Tool::Swift, Action::Build, args.forwarded, false),
            SwiftCommand::Test(args) => (Tool::Swift, Action::Test, args.forwarded, false),
            SwiftCommand::Lint(args) => (Tool::Swift, Action::Lint, args.forwarded, false),
        },
        ToolCommand::Cargo(command) => match command {
            CargoCommand::Build(args) => (
                Tool::Cargo,
                Action::Build,
                args.action.forwarded,
                args.nightly,
            ),
            CargoCommand::Test(args) => (
                Tool::Cargo,
                Action::Test,
                args.action.forwarded,
                args.nightly,
            ),
            CargoCommand::Fmt(args) => (
                Tool::Cargo,
                Action::Fmt,
                args.action.forwarded,
                args.nightly,
            ),
            CargoCommand::Package(args) => (
                Tool::Cargo,
                Action::Package,
                args.action.forwarded,
                args.nightly,
            ),
            CargoCommand::Clippy(args) => (
                Tool::Cargo,
                Action::Clippy,
                args.action.forwarded,
                args.nightly,
            ),
        },
    };

    let post_action = normalize_post_action(tool, action, &cli.options)
        .map_err(|message| Cli::command().error(ErrorKind::ValueValidation, message))?;
    let options = WrapperOptions {
        cwd: cli.options.cwd,
        timeout: cli.options.timeout,
        save_log: cli.options.save_log,
        plan: cli.options.plan,
    };
    Ok(Invocation {
        tool,
        action,
        options,
        post_action,
        forwarded_args,
        nightly,
    })
}

/// Parse one whole-operation deadline.
///
/// # Errors
/// Returns an error when the value has an unsupported unit, is zero, or overflows.
pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = raw.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = raw.strip_suffix('s') {
        (number, 1_000_u64)
    } else if let Some(number) = raw.strip_suffix('m') {
        (number, 60_000_u64)
    } else if let Some(number) = raw.strip_suffix('h') {
        (number, 3_600_000_u64)
    } else {
        return Err("duration must end in ms, s, m, or h".to_owned());
    };

    let value = number
        .parse::<u64>()
        .map_err(|_| "duration value must be a positive integer".to_owned())?;
    if value == 0 {
        return Err("duration value must be a positive integer".to_owned());
    }

    let milliseconds = value
        .checked_mul(multiplier)
        .ok_or_else(|| "duration is too large".to_owned())?;
    Ok(Duration::from_millis(milliseconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, clap::Error> {
        parse(std::iter::once("distill").chain(args.iter().copied()))
    }

    #[test]
    fn accepts_every_strict_action() {
        for args in [
            ["xcode", "build"].as_slice(),
            ["xcode", "test"].as_slice(),
            ["xcode", "list"].as_slice(),
            ["swift", "build"].as_slice(),
            ["swift", "test"].as_slice(),
            ["swift", "lint"].as_slice(),
            ["cargo", "build"].as_slice(),
            ["cargo", "test"].as_slice(),
            ["cargo", "fmt"].as_slice(),
            ["cargo", "package"].as_slice(),
            ["cargo", "clippy"].as_slice(),
        ] {
            assert!(parse_args(args).is_ok(), "args: {args:?}");
        }
    }

    #[test]
    fn cargo_nightly_selects_toolchain_and_is_cargo_only() {
        let invocation = parse_args(&["cargo", "build", "--nightly"]).expect("valid invocation");

        assert!(invocation.nightly);
        assert!(
            !parse_args(&["cargo", "build"])
                .expect("valid invocation")
                .nightly
        );
        assert!(parse_args(&["swift", "build", "--nightly"]).is_err());
    }

    #[test]
    fn rejects_unknown_tools_and_actions() {
        assert!(parse_args(&["node", "build"]).is_err());
        assert!(parse_args(&["xcode", "deploy"]).is_err());
        assert!(parse_args(&["cargo", "deploy"]).is_err());
    }

    #[test]
    fn accepts_wrapper_flags_before_and_after_action() {
        let invocation = parse_args(&[
            "--cwd",
            "/tmp/project",
            "xcode",
            "build",
            "--timeout",
            "500ms",
            "--plan",
            "--",
            "-scheme",
            "Demo",
            "--cwd",
        ])
        .expect("valid invocation");

        assert_eq!(invocation.options.cwd, Some(PathBuf::from("/tmp/project")));
        assert_eq!(invocation.options.timeout, Some(Duration::from_millis(500)));
        assert!(invocation.options.plan);
        assert_eq!(invocation.forwarded_args.len(), 3);
        assert_eq!(invocation.forwarded_args[0], OsString::from("-scheme"));
        assert_eq!(invocation.forwarded_args[2], OsString::from("--cwd"));
    }

    #[test]
    fn rejects_invalid_post_action_combinations() {
        assert!(parse_args(&["xcode", "test", "--open"]).is_err());
        assert!(parse_args(&["swift", "build", "--mv"]).is_err());
        assert!(parse_args(&["swift", "build", "--open"]).is_err());
        assert!(parse_args(&["cargo", "build", "--open"]).is_err());
        assert!(parse_args(&["xcode", "build", "--wait"]).is_err());
        assert!(parse_args(&["xcode", "build", "--open"]).is_ok());
    }
    #[test]
    fn stream_implies_wait() {
        let invocation =
            parse_args(&["xcode", "build", "--open", "--stream"]).expect("valid invocation");
        assert!(invocation.waits_for_runtime());
    }

    #[test]
    fn duration_parser_accepts_documented_units() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3_600)));
    }

    #[test]
    fn duration_parser_rejects_zero_unknown_and_overflow() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("1d").is_err());
        assert!(parse_duration("18446744073709551615h").is_err());
    }

    #[test]
    fn generated_help_is_compact_and_has_example() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("Example: distill --plan swift build"));
        assert!(!help.contains("--open"));
        assert!(!help.contains("--mv"));
        assert!(!help.contains("--omv"));
        assert!(!help.contains("--run-args"));
        for args in [
            [
                "--plan",
                "swift",
                "build",
                "--",
                "--configuration",
                "release",
            ]
            .as_slice(),
            [
                "--timeout",
                "5m",
                "cargo",
                "clippy",
                "--",
                "--all-targets",
                "--all-features",
            ]
            .as_slice(),
            [
                "--save-log",
                "./build.log",
                "xcode",
                "build",
                "--",
                "--scheme",
                "Demo",
            ]
            .as_slice(),
            ["--open", "--stream", "xcode", "build"].as_slice(),
        ] {
            assert!(parse_args(args).is_ok(), "args: {args:?}");
        }
    }

    #[test]
    fn post_action_help_is_shown_at_xcode_and_build_levels() {
        let mut command = Cli::command();
        let xcode = command.find_subcommand_mut("xcode").expect("xcode help");
        let xcode_help = xcode.render_help().to_string();
        let xcode_build_help = xcode
            .find_subcommand_mut("build")
            .expect("xcode build help")
            .render_help()
            .to_string();
        let xcode_test_help = xcode
            .find_subcommand_mut("test")
            .expect("xcode test help")
            .render_help()
            .to_string();
        let swift_build_help = command
            .find_subcommand_mut("swift")
            .expect("swift help")
            .find_subcommand_mut("build")
            .expect("swift build help")
            .render_help()
            .to_string();
        let cargo_build_help = command
            .find_subcommand_mut("cargo")
            .expect("cargo help")
            .find_subcommand_mut("build")
            .expect("cargo build help")
            .render_help()
            .to_string();

        for option in ["--open", "--mv", "--omv", "--wait", "--stream"] {
            assert!(xcode_help.contains(option));
            assert!(xcode_build_help.contains(option));
            assert!(!xcode_test_help.contains(option));
            assert!(!swift_build_help.contains(option));
            assert!(!cargo_build_help.contains(option));
        }
    }

    #[test]
    fn nested_help_pages_stay_under_word_budget() {
        let mut command = Cli::command();
        for tool in command.get_subcommands_mut() {
            let tool_words = tool.render_help().to_string().split_whitespace().count();
            assert!(
                tool_words <= 70,
                "{} help has {tool_words} words",
                tool.get_name()
            );
            for action in tool.get_subcommands_mut() {
                let action_words = action.render_help().to_string().split_whitespace().count();
                assert!(
                    action_words <= 70,
                    "{} help has {action_words} words",
                    action.get_name()
                );
            }
        }
    }
}
