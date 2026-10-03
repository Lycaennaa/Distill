use std::fs;
use std::process::Command;
use std::time::Duration;

use distill::{CancellationToken, CommandSpec, RawLogSink, StatusClass, execute, execute_with_log};

fn shell(script: &str) -> CommandSpec {
    CommandSpec::new("/bin/sh")
        .args(["-c", script])
        .label("fake")
        .with_status_class(StatusClass::Compile)
}

#[test]
fn captures_both_streams_and_keeps_success_with_warning() {
    let execution = execute(
        &shell("printf 'src.swift:9: warning: warning\\n'; printf 'stderr line\\n' >&2"),
        None,
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert!(execution.report().final_status().is_success());
    assert_eq!(execution.digest().diagnostics().len(), 1);
    assert_eq!(execution.digest().fallback_lines().len(), 2);
}

#[test]
fn failing_child_uses_signal_tail_fallback() {
    let execution = execute(
        &shell("printf 'unknown failure\\n'; exit 7"),
        None,
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Compile
    );
    assert_eq!(
        execution.report().final_status().owner(),
        distill::Owner::Child
    );
    assert_eq!(execution.report().final_status().code(), 7);
    assert!(!execution.digest().has_diagnostics());
    assert_eq!(execution.digest().fallback_lines().len(), 1);
}

#[test]
fn timeout_terminates_process_group() {
    let execution = execute(
        &shell("sleep 30"),
        Some(Duration::from_millis(100)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Timeout
    );
    assert_eq!(execution.report().final_status().code(), 124);
}

#[test]
fn timeout_keeps_a_non_newline_terminated_output_fragment() {
    let execution = execute(
        &shell("printf 'partial-without-newline'; sleep 30"),
        Some(Duration::from_millis(100)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Timeout
    );
    assert!(
        execution
            .digest()
            .fallback_lines()
            .iter()
            .any(|line| line.text().contains("partial-without-newline"))
    );
}

#[test]
fn deadline_covers_descendant_holding_output_pipe() {
    let execution = execute(
        &shell("trap '' TERM; sleep 30 >/dev/null 2>&1 & wait"),
        Some(Duration::from_millis(100)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Timeout
    );
    assert_eq!(execution.report().final_status().code(), 124);
}

#[test]
fn pre_cancelled_operation_never_launches() {
    let token = CancellationToken::new();
    token.cancel();
    let execution = execute(&shell("exit 9"), None, &token).expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Interrupt
    );
    assert_eq!(
        execution.report().final_status().owner(),
        distill::Owner::Wrapper
    );
}

#[test]
fn signaled_child_preserves_signal_class_and_mapped_code() {
    let execution = execute(&shell("kill -TERM $$"), None, &CancellationToken::new())
        .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Signaled
    );
    assert_eq!(execution.report().final_status().code(), 143);
    assert_eq!(
        execution.report().final_status().owner(),
        distill::Owner::Child
    );
}

#[test]
fn bounds_an_oversized_capture_line() {
    let execution = execute(
        &shell("printf '%100000s\\n' x; exit 1"),
        None,
        &CancellationToken::new(),
    )
    .expect("execution should complete");
    let lines = execution.digest().fallback_lines();
    let line = lines
        .first()
        .expect("fallback should retain the output line");

    assert!(line.text().len() < 66_000);
}

#[test]
fn normal_completion_cleans_owned_descendants() {
    let pid_file = format!("target/distill-normal-cleanup-{}.pid", std::process::id());
    if let Err(error) = fs::remove_file(&pid_file) {
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
    let spec = CommandSpec::new("/bin/sh")
        .args([
            "-c",
            "trap '' TERM; sleep 30 >/dev/null 2>&1 & printf '%s\\n' \"$!\" > \"$1\"; exit 0",
            "distill",
            &pid_file,
        ])
        .label("fake")
        .with_status_class(StatusClass::Compile);
    let execution =
        execute(&spec, None, &CancellationToken::new()).expect("execution should complete");
    let pid = fs::read_to_string(&pid_file).expect("child should write its PID");
    let status = Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .status()
        .expect("kill probe should execute");
    let _ = fs::remove_file(&pid_file);

    assert!(execution.report().final_status().is_success());
    assert!(!status.success(), "owned descendant remained alive");
}

#[test]
fn missing_program_is_distinct_from_launch_failure() {
    let execution = execute(
        &CommandSpec::new("distill-definitely-missing-tool"),
        None,
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::ToolMissing
    );
    assert_eq!(execution.report().final_status().code(), 127);
}

#[test]
fn closes_child_stdin() {
    let execution = execute(
        &shell("if read value; then exit 1; else printf 'stdin closed\\n'; fi"),
        None,
        &CancellationToken::new(),
    )
    .expect("execution should complete");

    assert!(execution.report().final_status().is_success());
    assert_eq!(execution.digest().fallback_lines().len(), 1);
}

#[test]
fn structured_diagnostics_have_a_separate_line_bound() {
    let message = "x".repeat(70_000);
    let json = format!(
        r#"{{"reason":"compiler-message","message":{{"message":"{message}","level":"error","spans":[]}}}}"#
    );
    let leading_whitespace = " ".repeat(70_000);
    let script = format!("printf '%s\\n' '{leading_whitespace}{json}'");
    let execution = execute(&shell(&script), None, &CancellationToken::new())
        .expect("execution should complete");

    assert_eq!(execution.digest().diagnostics().len(), 1);
    assert!(
        execution
            .digest()
            .diagnostics()
            .first()
            .is_some_and(|diagnostic| diagnostic.message().len() >= 70_000)
    );
}
#[test]
fn timeout_precedes_preexisting_interrupt() {
    let token = CancellationToken::new();
    token.cancel();
    let execution =
        execute(&shell("exit 9"), Some(Duration::ZERO), &token).expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Timeout
    );
    assert_eq!(execution.report().final_status().code(), 124);
}
#[derive(Default)]
struct BufferSink(Vec<u8>);

impl RawLogSink for BufferSink {
    fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

#[test]
fn complete_log_sink_preserves_merged_stream_arrival() {
    let mut sink = BufferSink::default();
    let execution = execute_with_log(
        &shell(
            "printf 'stdout one\\n'; /bin/sleep 0.05; printf 'stderr one\\n' >&2; /bin/sleep 0.05; printf 'stdout two\\n'",
        ),
        None,
        &CancellationToken::new(),
        &mut sink,
    )
    .expect("execution should complete");

    assert!(execution.report().final_status().is_success());
    assert_eq!(sink.0, b"stdout one\nstderr one\nstdout two\n");
}

struct FailingSink;

impl RawLogSink for FailingSink {
    fn write_chunk(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("sink unavailable"))
    }
}

#[test]
fn capture_sink_failure_is_wrapper_owned_log_write() {
    let mut sink = FailingSink;
    let execution = execute_with_log(
        &shell("printf 'output\\n'; sleep 30"),
        None,
        &CancellationToken::new(),
        &mut sink,
    )
    .expect("execution should report sink failure");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::LogWrite
    );
    assert_eq!(
        execution.report().final_status().owner(),
        distill::Owner::Wrapper
    );
    assert_eq!(execution.report().final_status().code(), 1);
}
