use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use distill::artifact::Artifact;
use distill::log::{LogPublication, LogPublisher, PublishedLog};
use distill::process::{CancellationToken, Execution, execute_until, execute_with_log_until};
use distill::record::RecordKind;
use distill::{Action, RedactionPolicy, StatusClass, Tool, compact_text};

use super::{emit, exit_code, format_duration};
fn emit_execution(
    label: &str,
    execution: &Execution,
    tool: Tool,
    action: Action,
    policy: RedactionPolicy,
    diagnostic_root: Option<&Path>,
    published_log: Option<&PublishedLog>,
) -> ExitCode {
    if tool == Tool::Xcode && action == Action::List {
        for section in execution.digest().xcode().list_sections() {
            let values = section.values().join(", ");
            let payload = if values.is_empty() {
                section.kind().to_string()
            } else {
                format!("{}: {values}", section.kind())
            };
            emit(RecordKind::List, &payload, policy);
        }
    }
    if action == Action::Test {
        emit_test_records(execution, policy);
    }
    for diagnostic in execution.digest().diagnostics() {
        let rendered = diagnostic_root.map_or_else(
            || diagnostic.render(),
            |root| diagnostic.render_relative_to(root),
        );
        emit(RecordKind::Diagnostic, &rendered, policy);
    }

    let final_status = execution.report().final_status();
    let has_test_failures = !execution.digest().test_failures().is_empty();
    if !final_status.is_success() && !execution.digest().has_diagnostics() && !has_test_failures {
        emit(RecordKind::Fallback, "no recognized diagnostics", policy);
        for line in execution.digest().fallback_lines() {
            let payload = format!("{}: {}", line.stream(), compact_text(line.text()));
            emit(RecordKind::Fallback, &payload, policy);
        }
    }

    if let Some(log) = published_log {
        emit(
            RecordKind::Log,
            &format!(
                "path={} bytes={} checksum={} redaction={}",
                log.path().display(),
                log.bytes(),
                log.checksum(),
                log.policy(),
            ),
            policy,
        );
    }
    if let Some(message) = execution.message() {
        emit(RecordKind::Failure, message, policy);
    }

    emit(
        RecordKind::Result,
        &result_payload(label, execution),
        policy,
    );
    exit_code(final_status.code())
}

pub fn result_payload(label: &str, execution: &Execution) -> String {
    let final_status = execution.report().final_status();
    let status_metadata = execution
        .report()
        .statuses()
        .iter()
        .filter(|status| **status != final_status)
        .map(|status| format!("{}:{}:{}", status.class(), status.owner(), status.code()))
        .collect::<Vec<_>>();
    let lower_statuses = if status_metadata.is_empty() {
        String::new()
    } else {
        format!(" lower={}", status_metadata.join(","))
    };
    let outcome = if final_status.is_success() {
        "succeeded"
    } else {
        "failed"
    };
    let class = if final_status.is_success() {
        String::new()
    } else {
        format!(" class={}", final_status.class())
    };
    let label_prefix = if label.is_empty() {
        String::new()
    } else {
        format!("{label}: ")
    };
    format!(
        "{label_prefix}{outcome} (exit {}, {}){} owner={}{}",
        final_status.code(),
        format_duration(execution.duration()),
        class,
        final_status.owner(),
        lower_statuses
    )
}

fn emit_test_records(execution: &Execution, policy: RedactionPolicy) {
    let summary = execution.digest().test_summary();
    if !summary.has_tests() {
        return;
    }
    emit(
        RecordKind::Test,
        &format!(
            "passed={} failed={} skipped={}",
            summary.passed(),
            summary.failed(),
            summary.skipped()
        ),
        policy,
    );
    for failure in execution.digest().test_failures() {
        let source = failure.source().unwrap_or("<unknown>");
        let line = failure
            .line()
            .map_or_else(|| "<unknown>".to_owned(), |line| line.to_string());
        let reason = failure.reason().unwrap_or("unknown reason");
        emit(
            RecordKind::Test,
            &format!("failed {} ({source}:{line}): {reason}", failure.name()),
            policy,
        );
    }
}

pub fn emit_synthetic_failure(
    label: &str,
    class: StatusClass,
    message: &str,
    tool: Tool,
    action: Action,
    policy: RedactionPolicy,
) -> ExitCode {
    let Ok(execution) = Execution::wrapper_failure(class, message, Duration::ZERO) else {
        return ExitCode::from(1);
    };
    emit_execution(label, &execution, tool, action, policy, None, None)
}
pub fn run_execution(
    label: &str,
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
    operation_started: Instant,
) -> ExitCode {
    emit_artifact(invocation, spec, policy);
    let mut log_publisher = match invocation.save_log_path() {
        Some(path) => match LogPublisher::prepare(path.to_owned()) {
            Ok(publisher) => Some(publisher),
            Err(error) => {
                return emit_synthetic_failure(
                    label,
                    StatusClass::LogWrite,
                    &error.to_string(),
                    invocation.tool(),
                    invocation.action(),
                    policy,
                );
            }
        },
        None => None,
    };
    let mut execution = match execute_primary(spec, deadline, cancellation, &mut log_publisher) {
        Ok(execution) => execution,
        Err(error) => {
            return emit_synthetic_failure(
                label,
                StatusClass::GenericWrapper,
                &error.to_string(),
                invocation.tool(),
                invocation.action(),
                policy,
            );
        }
    };

    let published_log = match log_publisher.as_mut() {
        Some(publisher) => {
            let capture_failed = publisher.capture_failed();
            if capture_failed || !execution.capture_complete() {
                publisher.abort();
                if !capture_failed
                    && let Err(error) = execution
                        .add_wrapper_status(StatusClass::LogWrite, "log capture incomplete")
                {
                    return emit_synthetic_failure(
                        label,
                        StatusClass::GenericWrapper,
                        &error.to_string(),
                        invocation.tool(),
                        invocation.action(),
                        policy,
                    );
                }
                None
            } else {
                let publication = publisher.publish(policy, deadline, cancellation);
                let log_metadata = publication.published().cloned();
                if let Err(error) = add_publication_statuses(&mut execution, &publication) {
                    return emit_synthetic_failure(
                        label,
                        StatusClass::GenericWrapper,
                        &error.to_string(),
                        invocation.tool(),
                        invocation.action(),
                        policy,
                    );
                }
                log_metadata
            }
        }
        None => None,
    };

    let post_action = match execute_post_action(
        invocation,
        spec,
        &mut execution,
        deadline,
        cancellation,
        policy,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            return emit_synthetic_failure(
                label,
                StatusClass::GenericWrapper,
                &error.to_string(),
                invocation.tool(),
                invocation.action(),
                policy,
            );
        }
    };
    emit_post_action(post_action.as_ref(), policy);
    execution.set_duration(operation_started.elapsed());
    emit_execution(
        label,
        &execution,
        invocation.tool(),
        invocation.action(),
        policy,
        spec.cwd(),
        published_log.as_ref(),
    )
}

fn emit_artifact(
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    policy: RedactionPolicy,
) {
    if let Some(artifact) = spec.cwd().and_then(|root| {
        Artifact::for_command(
            invocation.tool(),
            invocation.action(),
            root,
            false,
            invocation.planned_post_action(),
        )
    }) {
        emit(RecordKind::Artifact, &artifact.render(), policy);
    }
}

fn execute_primary(
    spec: &distill::CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    log_publisher: &mut Option<LogPublisher>,
) -> Result<Execution, distill::ProcessError> {
    let execution = log_publisher.as_mut().map_or_else(
        || execute_until(spec, deadline, cancellation),
        |sink| execute_with_log_until(spec, deadline, cancellation, sink),
    );
    if execution.is_err()
        && let Some(publisher) = log_publisher.as_mut()
    {
        publisher.abort();
    }
    execution
}

fn execute_post_action(
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
) -> Result<Option<distill::post_action::PostActionOutcome>, distill::ProcessError> {
    if !execution.report().final_status().is_success() {
        return Ok(None);
    }
    let Some(post_action) = invocation.post_action() else {
        return Ok(None);
    };
    match distill::post_action::run(
        invocation,
        post_action,
        spec,
        execution,
        deadline,
        cancellation,
        policy,
    ) {
        Ok(outcome) => Ok(Some(outcome)),
        Err(error) => {
            execution.add_wrapper_status(StatusClass::GenericWrapper, error.to_string())?;
            Ok(None)
        }
    }
}

fn emit_post_action(
    outcome: Option<&distill::post_action::PostActionOutcome>,
    policy: RedactionPolicy,
) {
    let Some(outcome) = outcome else {
        return;
    };
    match outcome {
        distill::post_action::PostActionOutcome::ProductNotResolved { action } => {
            emit(
                RecordKind::Action,
                &format!("{} state=failed", action.label()),
                policy,
            );
        }
        distill::post_action::PostActionOutcome::ProductResolved { artifact, actions } => {
            emit(RecordKind::Artifact, &artifact.render(), policy);
            for action in actions {
                emit(RecordKind::Action, &action.render(), policy);
            }
        }
    }
}
fn add_publication_statuses(
    execution: &mut Execution,
    publication: &LogPublication,
) -> Result<(), distill::ProcessError> {
    if let Some((class, message)) = publication.lower_failure() {
        execution.add_wrapper_status(class, message)?;
    }
    if let Some((class, message)) = publication.failure() {
        execution.add_wrapper_status(class, message)?;
    }
    Ok(())
}
