use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use distill::artifact::Artifact;
use distill::log::{LogPublication, LogPublisher, PublishedLog};
use distill::process::{CancellationToken, Execution, execute_until, execute_with_log_until};
use distill::record::RecordKind;
use distill::{
    Action, DiagnosticGroup, GradleBuildFailure, RedactionPolicy, Severity, StatusClass, Tool,
    compact_text,
};

use super::{emit, exit_code, format_duration};
const LINT_COMPACTION_THRESHOLD: usize = 50;
const LINT_EXAMPLE_LIMIT: usize = 3;
const LINT_EXAMPLE_CHARACTER_LIMIT: usize = 180;

#[derive(Default)]
struct LintRuleSummary {
    count: usize,
    files: BTreeSet<String>,
    examples: Vec<String>,
}

#[derive(Clone, Copy)]
struct ExecutionMetadata<'a> {
    diagnostic_root: Option<&'a Path>,
    published_log: Option<&'a PublishedLog>,
    gradle_test_summary: Option<&'a crate::gradle_test_results::GradleTestSummary>,
}
fn emit_execution(
    label: &str,
    execution: &Execution,
    tool: Tool,
    action: Action,
    policy: RedactionPolicy,
    metadata: ExecutionMetadata<'_>,
) -> ExitCode {
    let ExecutionMetadata {
        diagnostic_root,
        published_log,
        gradle_test_summary,
    } = metadata;
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
    if action == Action::Test
        || gradle_test_summary.is_some()
        || (tool == Tool::Gradle && execution.digest().test_summary().has_tests())
    {
        emit_test_records(execution, gradle_test_summary, policy);
    }
    let diagnostics = execution.digest().diagnostics();
    if tool == Tool::Swift
        && action == Action::Lint
        && diagnostics
            .iter()
            .map(DiagnosticGroup::count)
            .fold(0_usize, usize::saturating_add)
            > LINT_COMPACTION_THRESHOLD
    {
        emit_lint_summary(&diagnostics, diagnostic_root, policy);
    } else {
        emit_diagnostic_records(&diagnostics, diagnostic_root, policy);
    }

    let final_status = execution.report().final_status();
    let has_gradle_failure = if tool == Tool::Gradle && !final_status.is_success() {
        emit_gradle_build_failure_record(execution.digest().gradle_build_failure(), policy)
    } else {
        false
    };
    let has_test_failures = !execution.digest().test_failures().is_empty()
        || gradle_test_summary.is_some_and(|summary| summary.failed > 0);
    if !final_status.is_success()
        && !execution.digest().has_diagnostics()
        && !has_test_failures
        && !has_gradle_failure
    {
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

fn emit_diagnostic_records(
    diagnostics: &[DiagnosticGroup],
    diagnostic_root: Option<&Path>,
    policy: RedactionPolicy,
) {
    for diagnostic in diagnostics {
        let rendered = diagnostic_root.map_or_else(
            || diagnostic.render(),
            |root| diagnostic.render_relative_to(root),
        );
        emit(RecordKind::Diagnostic, &rendered, policy);
    }
}

fn emit_lint_summary(
    diagnostics: &[DiagnosticGroup],
    diagnostic_root: Option<&Path>,
    policy: RedactionPolicy,
) {
    let mut summaries = BTreeMap::<(Severity, String), LintRuleSummary>::new();
    let mut severity_counts = BTreeMap::<Severity, usize>::new();
    let mut files = BTreeSet::new();
    let mut diagnostic_count = 0_usize;

    for diagnostic in diagnostics {
        let count = diagnostic.count();
        diagnostic_count = diagnostic_count.saturating_add(count);
        let severity_count = severity_counts.entry(diagnostic.severity()).or_default();
        *severity_count = severity_count.saturating_add(count);

        let source = diagnostic.source().unwrap_or("<unknown>");
        files.insert(source.to_owned());
        let summary = summaries
            .entry((
                diagnostic.severity(),
                diagnostic.rule().unwrap_or("unclassified").to_owned(),
            ))
            .or_default();
        summary.count = summary.count.saturating_add(count);
        if summary.files.insert(source.to_owned()) && summary.examples.len() < LINT_EXAMPLE_LIMIT {
            let rendered = diagnostic_root.map_or_else(
                || diagnostic.render(),
                |root| diagnostic.render_relative_to(root),
            );
            summary.examples.push(truncate_lint_example(&rendered));
        }
    }

    let by_severity = severity_counts
        .iter()
        .map(|(severity, count)| format!("{severity}:{count}"))
        .collect::<Vec<_>>()
        .join(",");
    emit(
        RecordKind::Diagnostic,
        &format!(
            "SwiftLint summary: diagnostics={diagnostic_count} files={} rules={} by-severity={by_severity}",
            files.len(),
            summaries.len(),
        ),
        policy,
    );

    for ((severity, rule), summary) in summaries {
        let mut payload = format!(
            "SwiftLint {severity} [rule={rule}]: count={} files={}",
            summary.count,
            summary.files.len(),
        );
        if !summary.examples.is_empty() {
            payload.push_str(" examples=");
            payload.push_str(&summary.examples.join(" | "));
        }
        let omitted_files = summary.files.len().saturating_sub(summary.examples.len());
        if omitted_files > 0 {
            payload.push_str(" (+");
            payload.push_str(&omitted_files.to_string());
            payload.push_str(" more files)");
        }
        emit(RecordKind::Diagnostic, &payload, policy);
    }
}

fn truncate_lint_example(value: &str) -> String {
    let mut characters = value.chars();
    let example = characters
        .by_ref()
        .take(LINT_EXAMPLE_CHARACTER_LIMIT)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{example}…")
    } else {
        example
    }
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

fn emit_test_records(
    execution: &Execution,
    gradle_test_summary: Option<&crate::gradle_test_results::GradleTestSummary>,
    policy: RedactionPolicy,
) {
    let summary = execution.digest().test_summary();
    if gradle_test_summary.is_none() && !summary.has_tests() {
        return;
    }
    let (executed, passed, failed, skipped) = gradle_test_summary.map_or_else(
        || {
            (
                summary.executed(),
                summary.passed(),
                summary.failed(),
                summary.skipped(),
            )
        },
        |summary| {
            (
                summary.executed,
                summary.passed,
                summary.failed,
                summary.skipped,
            )
        },
    );
    emit(
        RecordKind::Test,
        &format!("executed={executed} passed={passed} failed={failed} skipped={skipped}"),
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
    if let Some(summary) = gradle_test_summary {
        for failure in &summary.failures {
            emit(
                RecordKind::Test,
                &format!(
                    "failed {} {}: {}",
                    failure.task_path, failure.name, failure.reason
                ),
                policy,
            );
        }
        let details_shown = u32::try_from(summary.failures.len()).unwrap_or(u32::MAX);
        let details_omitted = summary.failed.saturating_sub(details_shown);
        if details_omitted > 0 {
            emit(
                RecordKind::Test,
                &format!("failure details omitted={details_omitted}"),
                policy,
            );
        }
    }
}
fn emit_gradle_build_failure_record(
    failure: Option<&GradleBuildFailure>,
    policy: RedactionPolicy,
) -> bool {
    let Some(failure) = failure else {
        return false;
    };
    let tasks = failure.failed_tasks();
    let Some(reason) = failure
        .reason()
        .or_else(|| (!tasks.is_empty()).then_some(""))
    else {
        return false;
    };
    let mut payload = if tasks.is_empty() {
        "Gradle build failed".to_owned()
    } else {
        let subject = if tasks.len() == 1 {
            "Gradle task"
        } else {
            "Gradle tasks"
        };
        format!("{subject} {} failed", tasks.join(", "))
    };
    if !reason.is_empty() {
        payload.push_str(": ");
        payload.push_str(reason);
    }
    emit(RecordKind::Diagnostic, &payload, policy);
    true
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
    emit_execution(
        label,
        &execution,
        tool,
        action,
        policy,
        ExecutionMetadata {
            diagnostic_root: None,
            published_log: None,
            gradle_test_summary: None,
        },
    )
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

    if let Err(exit_code) = run_post_action(
        label,
        invocation,
        spec,
        &mut execution,
        deadline,
        cancellation,
        policy,
    ) {
        return exit_code;
    }
    let gradle_test_summary = match collect_gradle_test_summary_or_emit(
        label,
        invocation,
        spec,
        &mut execution,
        deadline,
        cancellation,
        policy,
    ) {
        Ok(summary) => summary,
        Err(exit_code) => return exit_code,
    };
    execution.set_duration(operation_started.elapsed());
    emit_execution(
        label,
        &execution,
        invocation.tool(),
        invocation.action(),
        policy,
        execution_metadata(spec, published_log.as_ref(), gradle_test_summary.as_ref()),
    )
}
fn execution_metadata<'a>(
    spec: &'a distill::CommandSpec,
    published_log: Option<&'a PublishedLog>,
    gradle_test_summary: Option<&'a crate::gradle_test_results::GradleTestSummary>,
) -> ExecutionMetadata<'a> {
    ExecutionMetadata {
        diagnostic_root: spec.cwd(),
        published_log,
        gradle_test_summary,
    }
}
fn run_post_action(
    label: &str,
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
) -> Result<(), ExitCode> {
    match execute_post_action(invocation, spec, execution, deadline, cancellation, policy) {
        Ok(outcome) => {
            emit_post_action(outcome.as_ref(), policy);
            Ok(())
        }
        Err(error) => Err(emit_synthetic_failure(
            label,
            StatusClass::GenericWrapper,
            &error.to_string(),
            invocation.tool(),
            invocation.action(),
            policy,
        )),
    }
}
fn collect_gradle_test_summary(
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<Option<crate::gradle_test_results::GradleTestSummary>, distill::ProcessError> {
    let summary = if invocation.tool() != Tool::Gradle
        || !execution.capture_complete()
        || execution.report().final_status().owner() != distill::Owner::Child
        || cancellation.is_cancelled()
    {
        None
    } else {
        spec.cwd().and_then(|root| {
            let project_root = invocation.gradle_project_root(root)?;
            crate::gradle_test_results::summarize(
                &project_root,
                execution.digest().gradle_test_tasks(),
                deadline,
                cancellation,
            )
        })
    };
    if invocation.tool() == Tool::Gradle && execution.capture_complete() {
        add_gradle_summary_stop_status(execution, deadline, cancellation)?;
    }
    Ok(summary)
}
fn collect_gradle_test_summary_or_emit(
    label: &str,
    invocation: &distill::Invocation,
    spec: &distill::CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
) -> Result<Option<crate::gradle_test_results::GradleTestSummary>, ExitCode> {
    collect_gradle_test_summary(invocation, spec, execution, deadline, cancellation).map_err(
        |error| {
            emit_synthetic_failure(
                label,
                StatusClass::GenericWrapper,
                &error.to_string(),
                invocation.tool(),
                invocation.action(),
                policy,
            )
        },
    )
}
fn add_gradle_summary_stop_status(
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(), distill::ProcessError> {
    if execution.report().final_status().owner() != distill::Owner::Child {
        return Ok(());
    }
    let status = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Some((StatusClass::Timeout, "operation deadline exceeded"))
    } else if cancellation.is_cancelled() {
        Some((StatusClass::Interrupt, "operation interrupted"))
    } else {
        None
    };
    if let Some((class, message)) = status {
        execution.add_wrapper_status(class, message)?;
    }
    Ok(())
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
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::error::Error;

    fn child_success() -> Result<Execution, distill::ProcessError> {
        let spec = distill::CommandSpec::new("/bin/sh")
            .args(["-c", "printf '%s\\n' '> Task :test'"])
            .label("fake Gradle wrapper")
            .with_status_class(StatusClass::Test);
        execute_until(&spec, None, &CancellationToken::new())
    }

    fn verify_status(
        execution: &Execution,
        expected_class: StatusClass,
        expected_code: i32,
        expected_owner: distill::Owner,
    ) -> Result<(), Box<dyn Error>> {
        let status = execution.report().final_status();
        if status.class() != expected_class
            || status.code() != expected_code
            || status.owner() != expected_owner
        {
            return Err(std::io::Error::other("unexpected final status").into());
        }
        Ok(())
    }

    fn collect_gradle_summary(
        execution: &mut Execution,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<Option<crate::gradle_test_results::GradleTestSummary>, Box<dyn Error>> {
        let invocation = distill::parse(["distill", "gradle", "test"])?;
        let spec = distill::CommandSpec::new("gradlew").current_dir(std::env::current_dir()?);
        Ok(collect_gradle_test_summary(
            &invocation,
            &spec,
            execution,
            deadline,
            cancellation,
        )?)
    }
    #[test]
    fn report_deadline_changes_child_success_to_timeout() -> Result<(), Box<dyn Error>> {
        let mut execution = child_success()?;
        verify_status(&execution, StatusClass::Success, 0, distill::Owner::Child)?;
        let deadline = Some(Instant::now());
        let summary = collect_gradle_summary(&mut execution, deadline, &CancellationToken::new())?;
        if summary.is_some() {
            return Err(std::io::Error::other("expired report collection must be omitted").into());
        }
        verify_status(
            &execution,
            StatusClass::Timeout,
            124,
            distill::Owner::Wrapper,
        )
    }

    #[test]
    fn report_cancellation_changes_child_success_to_interrupt() -> Result<(), Box<dyn Error>> {
        let mut execution = child_success()?;
        verify_status(&execution, StatusClass::Success, 0, distill::Owner::Child)?;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let summary = collect_gradle_summary(&mut execution, None, &cancellation)?;
        if summary.is_some() {
            return Err(
                std::io::Error::other("cancelled report collection must be omitted").into(),
            );
        }
        verify_status(
            &execution,
            StatusClass::Interrupt,
            130,
            distill::Owner::Wrapper,
        )
    }

    #[test]
    fn report_deadline_takes_precedence_over_cancellation() -> Result<(), Box<dyn Error>> {
        let mut execution = child_success()?;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let summary = collect_gradle_summary(&mut execution, Some(Instant::now()), &cancellation)?;
        if summary.is_some() {
            return Err(std::io::Error::other("expired report collection must be omitted").into());
        }
        verify_status(
            &execution,
            StatusClass::Timeout,
            124,
            distill::Owner::Wrapper,
        )
    }
}
