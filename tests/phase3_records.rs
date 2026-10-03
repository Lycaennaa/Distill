#![allow(
    clippy::panic_in_result_fn,
    reason = "test assertions are the intended failure mechanism"
)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[path = "support/mod.rs"]
mod support;
use support::{fixture, run_distill};

type TestResult = Result<(), Box<dyn Error>>;
#[test]
fn swift_test_emits_case_and_summary_records_without_fallback() -> TestResult {
    let root = fixture("swift-test")?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swift = bin.join("swift");
    fs::write(
        &fake_swift,
        "#!/bin/sh\nprintf \"Test Case 'PackageTests.XTests/testPass()' passed (0.001 seconds).\\n\"\nprintf \"Test Case 'PackageTests.XTests/testFailure()' failed: assertion failed.\\n\"\nprintf 'Executed 2 tests, with 1 failure (0 unexpected) in 0.2 seconds\\n'\nexit 1\n",
    )?;
    let mut permissions = fs::metadata(&fake_swift)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swift, permissions)?;
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "swift",
            "test",
        ],
        &bin,
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("TEST: passed=1 failed=1 skipped=0"));
    assert!(stdout.contains("TEST: failed PackageTests.XTests/testFailure()"));
    assert!(stdout.contains("class=test owner=child"));
    assert!(!stdout.contains("FALLBACK:"));
    assert!(stdout.lines().all(|line| !line.starts_with("DISTILL ")));
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
fn xcode_list_emits_each_compact_section() -> TestResult {
    let root = fixture("xcode-list")?;
    fs::create_dir(root.join("Demo.xcodeproj"))?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_xcodebuild = bin.join("xcodebuild");
    fs::write(
        &fake_xcodebuild,
        "#!/bin/sh\nprintf 'Information about project \\\"Demo\\\":\\n'\nprintf '    Targets:\\n'\nprintf '        Demo\\n'\nprintf '    Build Configurations:\\n'\nprintf '        Debug\\n'\nprintf '    Schemes:\\n'\nprintf '        Demo\\n'\n",
    )?;
    let mut permissions = fs::metadata(&fake_xcodebuild)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_xcodebuild, permissions)?;
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "xcode",
            "list",
        ],
        &bin,
    )?;

    assert!(output.status.success());
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("LIST: project: Demo"));
    assert!(stdout.contains("LIST: targets: Demo"));
    assert!(stdout.contains("LIST: configurations: Debug"));
    assert!(stdout.contains("LIST: schemes: Demo"));
    assert!(stdout.lines().all(|line| !line.starts_with("DISTILL ")));
    fs::remove_dir_all(root)?;
    Ok(())
}
