#![cfg(windows)]

use std::time::{Duration, Instant};

use distill::{CancellationToken, CommandSpec, StatusClass, execute, execute_until_with_lines};

fn cmd(script: &str) -> CommandSpec {
    CommandSpec::new("cmd.exe")
        .args(["/D", "/C", script])
        .label("windows-process-test")
        .with_status_class(StatusClass::Compile)
}
fn powershell(script: &str) -> CommandSpec {
    CommandSpec::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .label("windows-process-test")
        .with_status_class(StatusClass::Compile)
}

#[test]
fn timeout_stops_a_windows_command() {
    let execution = execute(
        &powershell("Start-Sleep -Seconds 30"),
        Some(Duration::from_secs(1)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");
    let final_status = execution.report().final_status();
    assert_eq!(
        final_status.class(),
        StatusClass::Timeout,
        "unexpected final status: {final_status:?}"
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
        &powershell("Write-Output ready; Start-Sleep -Seconds 30"),
        None,
        &cancellation,
        &mut cancel_on_ready,
    )
    .expect("execution should complete");
    let final_status = execution.report().final_status();
    assert_eq!(
        final_status.class(),
        StatusClass::Interrupt,
        "unexpected final status: {final_status:?}"
    );
}

#[test]
fn descendant_inheriting_output_pipes_does_not_delay_completion() {
    // The started PowerShell process inherits cmd.exe's stdout/stderr and would
    // otherwise keep the wrapper's output pipes open for ten seconds.
    let started = Instant::now();
    let execution = execute(
        &cmd("start /B powershell.exe -NoProfile -NonInteractive -Command Start-Sleep -Seconds 10"),
        Some(Duration::from_secs(5)),
        &CancellationToken::new(),
    )
    .expect("execution should complete");
    let elapsed = started.elapsed();

    assert!(
        execution.report().final_status().is_success(),
        "unexpected final status: {:?}",
        execution.report().final_status()
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "execution took {elapsed:?} while a descendant held inherited output pipes"
    );
}
