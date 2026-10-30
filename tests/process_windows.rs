#![cfg(windows)]

use std::time::{Duration, Instant};

use distill::{CancellationToken, CommandSpec, StatusClass, execute, execute_until_with_lines};

fn powershell(script: &str) -> CommandSpec {
    CommandSpec::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .label("windows-process-test")
        .with_status_class(StatusClass::Compile)
}
fn child_test_command(mode: &str) -> CommandSpec {
    let executable = std::env::current_exe().expect("locate test executable");
    CommandSpec::new(executable.into_os_string())
        .args([
            "--ignored",
            "--exact",
            "process_child_fixture",
            "--nocapture",
        ])
        .env("DISTILL_PROCESS_TEST_MODE", mode)
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
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .expect("compute process deadline");
    let execution = execute_until_with_lines(
        &child_test_command("cancel"),
        Some(deadline),
        &cancellation,
        &mut cancel_on_ready,
    )
    .expect("execution should complete");

    assert!(ready_seen.get(), "child did not emit its readiness marker");
    let final_status = execution.report().final_status();
    assert_eq!(
        final_status.class(),
        StatusClass::Interrupt,
        "unexpected final status: {final_status:?}"
    );
}

#[test]
fn descendant_inheriting_output_pipes_does_not_delay_completion() {
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_secs(12))
        .expect("compute process deadline");
    let descendant_ready = std::cell::Cell::new(false);
    let mut capture_descendant_output = |line: &distill::digest::OutputLine| {
        if line.text().trim() == "descendant-ready" {
            descendant_ready.set(true);
        }
        Ok(())
    };
    let execution = execute_until_with_lines(
        &child_test_command("descendant"),
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
#[ignore = "spawned by Windows process tests"]
fn process_child_fixture() {
    match std::env::var("DISTILL_PROCESS_TEST_MODE").as_deref() {
        Ok("cancel") => {
            use std::io::Write as _;

            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "ready").expect("write readiness marker");
            stdout.flush().expect("flush readiness marker");
            std::thread::sleep(Duration::from_secs(30));
        }
        Ok("descendant") => {
            let _child = std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Write-Output descendant-ready; Start-Sleep -Seconds 10",
                ])
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .expect("spawn descendant PowerShell process");
            std::thread::sleep(Duration::from_secs(2));
        }
        _ => {}
    }
}
