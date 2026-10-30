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
    let ready_seen = std::cell::Cell::new(false);
    let mut cancel_on_ready = |line: &distill::digest::OutputLine| {
        if line.text().trim() == "ready" {
            ready_seen.set(true);
            cancellation.cancel();
        }
        Ok(())
    };
    let execution = execute_until_with_lines(
        &cmd("echo ready & powershell.exe -NoProfile -NonInteractive -Command Start-Sleep -Seconds 30"),
        None,
        &cancellation,
        &mut cancel_on_ready,
    )
    .expect("execution should complete");

    assert!(
        ready_seen.get(),
        "command did not emit its readiness marker"
    );
    let final_status = execution.report().final_status();
    assert_eq!(
        final_status.class(),
        StatusClass::Interrupt,
        "unexpected final status: {final_status:?}"
    );
}

#[test]
fn descendant_inheriting_output_pipes_does_not_delay_completion() {
    let executable = std::env::current_exe().expect("locate test executable");
    let command = CommandSpec::new(executable.into_os_string())
        .args([
            "--ignored",
            "--exact",
            "descendant_pipe_holder",
            "--nocapture",
        ])
        .env("DISTILL_PROCESS_PIPE_DESCENDANT", "1")
        .label("windows-process-test")
        .with_status_class(StatusClass::Compile);
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_secs(5))
        .expect("compute process deadline");
    let descendant_ready = std::cell::Cell::new(false);
    let mut capture_descendant_output = |line: &distill::digest::OutputLine| {
        if line.text().trim() == "descendant-ready" {
            descendant_ready.set(true);
        }
        Ok(())
    };
    let execution = execute_until_with_lines(
        &command,
        Some(deadline),
        &CancellationToken::new(),
        &mut capture_descendant_output,
    )
    .expect("execution should complete");
    let elapsed = started.elapsed();

    assert!(
        descendant_ready.get(),
        "descendant did not emit output through the inherited pipe"
    );
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

#[test]
#[ignore = "spawned by the descendant pipe test"]
fn descendant_pipe_holder() {
    if std::env::var_os("DISTILL_PROCESS_PIPE_DESCENDANT").is_none() {
        return;
    }
    let _descendant = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output descendant-ready; Start-Sleep -Seconds 10",
        ])
        .spawn()
        .expect("spawn descendant PowerShell process");
    std::thread::sleep(Duration::from_secs(3));
}
