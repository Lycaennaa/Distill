use clap::error::ErrorKind;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use distill::artifact::Artifact;
use distill::cli::parse;
use distill::command::{CommandBuildError, command_for_with_cancellation, has_user_verbosity};
use distill::process::{CancellationToken, Execution, install_interrupt_handler};
use distill::record::{Record, RecordKind};
use distill::{Action, DiscoveryError, RedactionPolicy, StatusClass, Tool, redact};
mod cli_execution;

fn main() -> ExitCode {
    let invocation = match parse(std::env::args_os()) {
        Ok(invocation) => invocation,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                error.exit();
            }
            return emit_usage_failure();
        }
    };
    let operation_started = Instant::now();
    let deadline = invocation.timeout().map(|duration| {
        let now = operation_started;
        now.checked_add(duration).unwrap_or(now)
    });
    let label = format!("{} {}", invocation.tool(), invocation.action());
    let policy = redaction_policy(invocation.tool());
    let cancellation = CancellationToken::new();
    if install_interrupt_handler(&cancellation).is_err() {
        return cli_execution::emit_synthetic_failure(
            &label,
            StatusClass::GenericWrapper,
            "interrupt handler unavailable",
            invocation.tool(),
            invocation.action(),
            policy,
        );
    }
    let spec = match command_for_with_cancellation(&invocation, deadline, &cancellation) {
        Ok(spec) => spec,
        Err(error) => {
            let class = match error {
                CommandBuildError::DeferredPostAction => StatusClass::PostAction,
                CommandBuildError::Discovery(DiscoveryError::Timeout { .. }) => {
                    StatusClass::Timeout
                }
                CommandBuildError::Discovery(DiscoveryError::Interrupted { .. }) => {
                    StatusClass::Interrupt
                }
                CommandBuildError::Discovery(_) => StatusClass::Discovery,
            };
            return cli_execution::emit_synthetic_failure(
                &label,
                class,
                &error.to_string(),
                invocation.tool(),
                invocation.action(),
                policy,
            );
        }
    };

    if invocation.is_plan() {
        let injected = plan_injected_flags(&invocation, &spec);
        emit(
            RecordKind::Plan,
            &format!(
                "{label}: action={} program={} injected={injected}",
                invocation.action(),
                spec.program().to_string_lossy()
            ),
            policy,
        );
        if let Some(root) = spec.cwd()
            && let Some(artifact) = Artifact::for_command(
                invocation.tool(),
                invocation.action(),
                root,
                true,
                invocation.planned_post_action(),
            )
        {
            emit(RecordKind::Artifact, &artifact.render(), policy);
        }
        return ExitCode::SUCCESS;
    }

    cli_execution::run_execution(
        &label,
        &invocation,
        &spec,
        deadline,
        &cancellation,
        policy,
        operation_started,
    )
}

fn plan_injected_flags(invocation: &distill::Invocation, spec: &distill::CommandSpec) -> String {
    let has_flag = |names: &[&str]| -> bool {
        invocation
            .forwarded_args()
            .iter()
            .take_while(|argument| argument.to_str() != Some("--"))
            .any(|argument| argument.to_str().is_some_and(|text| names.contains(&text)))
    };
    let has_verbosity = has_user_verbosity(invocation.forwarded_args());
    let has_message_format = invocation
        .forwarded_args()
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
        .any(|argument| {
            argument.to_str().is_some_and(|text| {
                text == "--message-format"
                    || text
                        .strip_prefix("--message-format")
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
        });
    let has_spec_argument = |name: &str| spec.arguments().iter().any(|argument| argument == name);
    let mut flags = Vec::new();
    if invocation.tool() == Tool::Xcode {
        for option in ["-workspace", "-project"] {
            if has_spec_argument(option) && !has_flag(&[option]) {
                flags.push(option);
            }
        }
    }
    match (invocation.tool(), invocation.action()) {
        (Tool::Xcode, Action::Build | Action::Test)
            if has_spec_argument("-quiet") && !has_verbosity =>
        {
            flags.push("-quiet");
        }
        (Tool::Swift, Action::Build | Action::Test)
            if has_spec_argument("--quiet") && !has_verbosity =>
        {
            flags.push("--quiet");
        }
        (Tool::Cargo, Action::Build | Action::Clippy) => {
            if has_spec_argument("--quiet") && !has_verbosity {
                flags.push("--quiet");
            }
            if has_spec_argument("--message-format=json-diagnostic-rendered-ansi")
                && !has_message_format
            {
                flags.push("--message-format=json-diagnostic-rendered-ansi");
            }
        }
        _ => {}
    }
    if flags.is_empty() {
        "none".to_owned()
    } else {
        flags.join(",")
    }
}

fn emit(kind: RecordKind, payload: &str, policy: RedactionPolicy) {
    println!("{}", Record::new(kind, redact(payload, policy)).render());
}

fn redaction_policy(tool: Tool) -> RedactionPolicy {
    if tool == Tool::Xcode {
        RedactionPolicy::XcodeMandatory
    } else {
        RedactionPolicy::BestEffort
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() == 0 {
        format!("{}ms", duration.subsec_millis())
    } else {
        format!("{}.{:03}s", duration.as_secs(), duration.subsec_millis())
    }
}

fn exit_code(code: i32) -> ExitCode {
    match u8::try_from(code) {
        Ok(code) => ExitCode::from(code),
        Err(_error) => ExitCode::from(1),
    }
}
fn emit_usage_failure() -> ExitCode {
    let policy = RedactionPolicy::BestEffort;
    let execution = match Execution::wrapper_failure(
        StatusClass::Usage,
        "invalid command line",
        Duration::ZERO,
    ) {
        Ok(execution) => execution,
        Err(_error) => return ExitCode::from(1),
    };
    if let Some(message) = execution.message() {
        emit(RecordKind::Failure, message, policy);
    }
    let status = execution.report().final_status();
    emit(
        RecordKind::Result,
        &cli_execution::result_payload("", &execution),
        policy,
    );
    exit_code(status.code())
}
