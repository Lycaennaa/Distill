#![allow(
    clippy::panic_in_result_fn,
    reason = "test assertions are the intended failure mechanism"
)]

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};

#[path = "support/mod.rs"]
mod support;
use support::{fixture, run_distill};

type TestResult = Result<(), Box<dyn Error>>;
fn run_distill_with_restrictive_umask(
    arguments: &[&str],
    path: &std::path::Path,
) -> std::io::Result<Output> {
    Command::new("/bin/sh")
        .arg("-c")
        .arg("umask 0277; binary=$1; shift; exec \"$binary\" \"$@\"")
        .arg("distill-with-umask")
        .arg(env!("CARGO_BIN_EXE_distill"))
        .args(arguments)
        .env("PATH", path)
        .output()
}
#[test]
fn save_log_sanitizes_complete_capture_and_keeps_digest_stdout_clean() -> TestResult {
    let root = fixture("save")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(
        &fake_swiftlint,
        "#!/bin/sh\nprintf 'child stdout\\n'\n/bin/sleep 0.05\nprintf 'child stderr\\n' >&2\n/bin/sleep 0.05\nprintf 'password=hidden\\n'\n",
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;
    let log = root.join("complete.log");
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
            "--",
            "forwarded-private-value",
        ],
        &bin,
    )?;

    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("child stdout"));
    assert!(!stdout.contains("child stderr"));
    assert!(!stdout.contains("forwarded-private-value"));
    assert!(stdout.lines().all(|line| !line.starts_with("DISTILL ")));
    let log_record = stdout.find("LOG:").ok_or("missing LOG record")?;
    let result_record = stdout.find("RESULT:").ok_or("missing RESULT record")?;
    assert!(log_record < result_record);
    assert!(stdout.ends_with('\n'));
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("RESULT:"))
    );

    let content = fs::read(&log)?;
    let expected = b"child stdout\nchild stderr\npassword=[REDACTED]\n";
    assert_eq!(content, expected);
    let checksum = Sha256::digest(&content);
    let mut checksum_hex = String::with_capacity(64);
    for byte in &checksum {
        write!(&mut checksum_hex, "{byte:02x}").expect("writing to String cannot fail");
    }
    assert!(stdout.contains(&format!("bytes={} checksum={checksum_hex}", content.len())));
    assert_eq!(fs::metadata(&log)?.permissions().mode() & 0o777, 0o600);

    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn existing_log_destination_fails_before_launch() -> TestResult {
    let root = fixture("existing-log")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let sentinel = root.join("child-launched");
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(
        &fake_swiftlint,
        format!("#!/bin/sh\nprintf launched > '{}'\n", sentinel.display()),
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;
    let log = root.join("existing.log");
    fs::write(&log, "keep existing content")?;
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
        ],
        &bin,
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert!(!sentinel.exists());
    assert_eq!(fs::read_to_string(&log)?, "keep existing content");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("class=log_write owner=wrapper"),
        "stdout={stdout}"
    );
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("RESULT:"))
    );

    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn timeout_before_log_commit_removes_unpublished_file() -> TestResult {
    let root = fixture("timeout")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(
        &fake_swiftlint,
        "#!/bin/sh\nprintf 'partial log\\n'\n/bin/sleep 5\n",
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;
    let log = root.join("timed-out.log");
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--timeout",
            "100ms",
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
        ],
        &bin,
    )?;

    assert_eq!(output.status.code(), Some(124));
    assert!(!log.exists());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("class=timeout owner=wrapper"));
    assert!(!stdout.contains("LOG:"));
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("RESULT:"))
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn xcode_saved_log_uses_mandatory_typed_redaction() -> TestResult {
    let root = fixture("xcode-redaction")?;
    fs::create_dir(root.join("Demo.xcodeproj"))?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_xcodebuild = bin.join("xcodebuild");
    fs::write(
        &fake_xcodebuild,
        "#!/bin/sh\nprintf 'CODE_SIGN_IDENTITY = Secret Signing Name\\n'\nprintf 'DEVELOPMENT_TEAM = TEAM12345\\n'\nprintf 'PROVISIONING_PROFILE_SPECIFIER = Private Profile Name\\n'\nprintf 'contact developer@example.com\\n'\n",
    )?;
    let mut permissions = fs::metadata(&fake_xcodebuild)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_xcodebuild, permissions)?;
    let log = root.join("xcode.log");
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "xcode",
            "build",
        ],
        &bin,
    )?;

    assert!(output.status.success());
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("Secret Signing Name"));
    assert!(!stdout.contains("TEAM12345"));
    assert!(!stdout.contains("Private Profile Name"));
    assert!(!stdout.contains("developer@example.com"));
    let saved = fs::read_to_string(&log)?;
    assert!(saved.contains("[SIGNING_IDENTITY]"));
    assert!(saved.contains("[TEAM_ID]"));
    assert!(saved.contains("[PROFILE_ID]"));
    assert!(saved.contains("[EMAIL]"));
    assert!(!saved.contains("Secret Signing Name"));
    assert!(!saved.contains("TEAM12345"));
    assert!(!saved.contains("Private Profile Name"));
    assert!(!saved.contains("developer@example.com"));

    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn incomplete_detached_output_aborts_log_publication() -> TestResult {
    let root = fixture("incomplete-log")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let pid_file = root.join("writer.pid");
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(
        &fake_swiftlint,
        format!(
            "#!/bin/bash\nset -m\n/bin/sleep 30 &\nwriter_pid=$!\ndisown %1\nprintf '%s %s\\n' \"$$\" \"$writer_pid\" > '{}'\nprintf 'early-sentinel\\n'\n",
            pid_file.display()
        ),
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;
    let log = root.join("incomplete.log");
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--timeout",
            "20s",
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
        ],
        &bin,
    )?;
    let pids = fs::read_to_string(&pid_file)?;
    let mut pids = pids.split_whitespace();
    let shell_pid = pids.next().ok_or("missing shell pid")?.parse::<i32>()?;
    let writer_pid = pids.next().ok_or("missing writer pid")?.parse::<i32>()?;
    let writer_group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(writer_pid)))?;
    assert_ne!(writer_group.as_raw(), shell_pid);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(writer_pid),
        nix::sys::signal::Signal::SIGKILL,
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert!(!log.exists());
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("class=log_write owner=wrapper"));
    assert!(!stdout.contains("LOG:"));
    assert!(!stdout.contains("DIAG: late-sentinel"));
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("RESULT:"))
    );

    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn save_log_normalizes_permissions_under_restrictive_umask() -> TestResult {
    let root = fixture("umask")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(&fake_swiftlint, "#!/bin/sh\nprintf 'password=hidden\\n'\n")?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;
    let log = root.join("umask.log");
    let output = run_distill_with_restrictive_umask(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
        ],
        &bin,
    )?;

    assert!(output.status.success());
    assert_eq!(fs::read_to_string(&log)?, "password=[REDACTED]\n");
    assert_eq!(fs::metadata(&log)?.permissions().mode() & 0o777, 0o600);
    let lock = root.join(".umask.log.distill.lock");
    assert_eq!(fs::metadata(lock)?.permissions().mode() & 0o777, 0o600);
    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn fifo_lock_rejection_respects_cli_deadline() -> TestResult {
    let root = fixture("fifo-lock")?;
    let log = root.join("blocked.log");
    let lock = root.join(".blocked.log.distill.lock");
    let fifo_status = Command::new("/usr/bin/mkfifo").arg(&lock).status()?;
    assert!(fifo_status.success());

    let mut child = Command::new(env!("CARGO_BIN_EXE_distill"))
        .args([
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--timeout",
            "100ms",
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "lint",
        ])
        .env("PATH", "")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let child_pid = i32::try_from(child.id())?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let result = child.wait();
        let _send_result = sender.send(result);
    });

    let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
    drop(fs::remove_file(&lock));
    let status = match result {
        Ok(status) => status?,
        Err(error) => {
            let _kill_result = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child_pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            drop(waiter.join());
            fs::remove_dir_all(root)?;
            return Err(Box::new(error));
        }
    };
    waiter
        .join()
        .map_err(|_| std::io::Error::other("child waiter panicked"))?;

    assert_eq!(status.code(), Some(1));
    assert!(!log.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}
