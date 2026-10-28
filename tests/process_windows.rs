#![cfg(windows)]

use std::time::{Duration, Instant};

use distill::{CancellationToken, CommandSpec, StatusClass, execute, execute_until_with_lines};

fn cmd(script: &str) -> CommandSpec {
    CommandSpec::new("cmd.exe")
        .args(["/D", "/C", script])
        .label("windows-process-test")
        .with_status_class(StatusClass::Compile)
}

#[test]
fn timeout_stops_a_windows_command() {
    let execution = execute(
        &cmd("ping -n 30 127.0.0.1 >NUL"),
        Some(Duration::from_millis(200)),
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
fn cancellation_stops_a_windows_command() {
    let cancellation = CancellationToken::new();
    let mut cancel_on_ready = |line: &distill::digest::OutputLine| {
        if line.text() == "ready" {
            cancellation.cancel();
        }
        Ok(())
    };
    let execution = execute_until_with_lines(
        &cmd("echo ready & ping -n 30 127.0.0.1 >NUL"),
        None,
        &cancellation,
        &mut cancel_on_ready,
    )
    .expect("execution should complete");

    assert_eq!(
        execution.report().final_status().class(),
        StatusClass::Interrupt
    );
}

#[test]
fn descendant_inheriting_output_pipes_does_not_delay_completion() {
    // The started ping inherits cmd.exe's stdout/stderr and would otherwise keep
    // the wrapper's output pipes open for roughly nine seconds.
    let started = Instant::now();
    let execution = execute(
        &cmd("start \"\" /B ping -n 10 127.0.0.1"),
        Some(Duration::from_secs(5)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");
    let elapsed = started.elapsed();

    assert!(execution.report().final_status().is_success());
    assert!(
        elapsed < Duration::from_secs(8),
        "execution took {elapsed:?} while a descendant held inherited output pipes"
    );
}
