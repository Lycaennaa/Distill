#![allow(
    clippy::panic_in_result_fn,
    reason = "test assertions are the intended failure mechanism"
)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

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

fn fake_cargo_fixture(name: &str) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let root = fixture(name)?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_cargo = bin.join("cargo");
    fs::write(
        &fake_cargo,
        r#"#!/bin/sh
case "$1" in
fmt)
  printf 'received args: %s\n' "$*" >&2
  printf 'Diff in src/lib.rs at line 1\n' >&2
  exit 1
  ;;
test)
  printf 'received args: %s\n' "$*" >&2
  case "$*" in
  *--diagnostic-error*)
    printf '%s\n' '{"reason":"compiler-message","message":{"message":"no method named missing found","level":"error","code":{"code":"E0599"},"spans":[{"file_name":"src/lib.rs","line_start":7,"column_start":3,"is_primary":true}],"children":[]}}'
    exit 1
    ;;
  esac
  printf 'test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.2s\n'
  exit 1
  ;;
package)
  printf 'received args: %s\n' "$*" >&2
  exit 1
  ;;
esac
exit 2
"#,
    )?;
    let mut permissions = fs::metadata(&fake_cargo)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_cargo, permissions)?;
    Ok((root, bin))
}

fn run_cargo(root: &Path, bin: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    let cwd = root
        .to_str()
        .ok_or_else(|| std::io::Error::other("fixture path is not UTF-8"))?;
    let mut command_arguments = vec!["--cwd", cwd];
    command_arguments.extend_from_slice(arguments);
    run_distill(&command_arguments, bin)
}

#[test]
fn cargo_fmt_forwards_options_and_reports_format_failures() -> TestResult {
    let (root, bin) = fake_cargo_fixture("cargo-fmt")?;
    let output = run_cargo(&root, &bin, &["cargo", "fmt", "--", "--check"])?;

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("received args: fmt --check"));
    assert!(stdout.contains("FALLBACK: stderr: Diff in src/lib.rs at line 1"));
    assert!(stdout.contains("class=lint owner=child"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_test_summarizes_results_and_forwards_options() -> TestResult {
    let (root, bin) = fake_cargo_fixture("cargo-test-summary")?;
    let output = run_cargo(&root, &bin, &["cargo", "test", "--", "--all-targets"])?;

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(
        "received args: test --quiet --message-format=json-diagnostic-rendered-ansi --all-targets"
    ));
    assert!(stdout.contains("TEST: passed=2 failed=1 skipped=1"));
    assert!(stdout.contains("class=test owner=child"));
    assert!(stdout.contains("FALLBACK: stdout: test result: FAILED."));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_test_diagnostics_preserve_rust_error_codes() -> TestResult {
    let (root, bin) = fake_cargo_fixture("cargo-test-diagnostic")?;
    let output = run_cargo(&root, &bin, &["cargo", "test", "--", "--diagnostic-error"])?;

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("DIAG:"));
    assert!(stdout.contains("E0599"));
    assert!(stdout.contains("no method named missing found"));
    assert!(!stdout.contains("FALLBACK:"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_package_forwards_common_flags() -> TestResult {
    let (root, bin) = fake_cargo_fixture("cargo-package")?;
    let output = run_cargo(
        &root,
        &bin,
        &["cargo", "package", "--", "--locked", "--offline"],
    )?;

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("received args: package --quiet --locked --offline"));
    assert!(stdout.contains("class=compile owner=child"));
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
