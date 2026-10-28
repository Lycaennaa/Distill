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
fn plan_reports_unresolved_artifact_without_creating_log_or_exposing_args() -> TestResult {
    let root = fixture("plan")?;
    let log = root.join("planned.log");
    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "--plan",
            "--save-log",
            log.to_str().ok_or("log path is not UTF-8")?,
            "swift",
            "build",
            "--",
            "--private-child-value",
        ],
        std::path::Path::new(""),
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
    assert!(stdout.contains("PLAN:"));
    assert!(stdout.contains("INFO: kind=binary state=planned/unresolved"));
    assert!(!stdout.contains("root="));
    assert!(!stdout.contains("--private-child-value"));
    assert!(!stdout.contains("COMMAND:"));
    assert!(!stdout.contains("RESULT:"));
    assert!(!log.exists());
    assert_eq!(fs::read_dir(&root)?.count(), 2);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn swiftlint_diagnostic_paths_are_relative_to_selected_root() -> TestResult {
    let fixture_root = fixture("swiftlint-paths")?;
    fs::remove_file(fixture_root.join("Package.swift"))?;
    let root = fixture_root.join("quoted ' ; printf INJECTED >&2; #");
    fs::create_dir(&root)?;
    fs::write(root.join("Package.swift"), "// fixture package\n")?;
    let sources = root.join("Sources");
    fs::create_dir(&sources)?;
    let bin = fixture_root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swiftlint = bin.join("swiftlint");
    let diagnostic_path = sources.join("LibraryScanner.swift");
    fs::write(&diagnostic_path, "")?;
    fs::create_dir(root.join("Other"))?;
    let outside = fixture_root.join("outside");
    fs::create_dir_all(outside.join("sub"))?;
    fs::write(outside.join("sub/Dependency.swift"), "")?;
    fs::write(outside.join("Other.swift"), "")?;
    let escaped_relative_path = outside.join("Relative.swift");
    fs::write(&escaped_relative_path, "")?;
    std::os::unix::fs::symlink(outside.join("sub"), root.join("link"))?;
    let traversal_path = root.join("../outside/Other.swift");
    let symlink_external_path = root.join("link/Dependency.swift");
    let symlink_parent_path = root.join("link/../Other.swift");
    let in_root_parent_path = root.join("Other/../Sources/LibraryScanner.swift");
    fs::write(
        &fake_swiftlint,
        "#!/bin/sh\nshift\nprintf '%s\\n' \"$1:348: warning: Function Body Length Violation (function_body_length)\"\nprintf '%s\\n' '/opt/external/Dependency.swift:12: warning: avoid TODO (todo)'\nprintf '%s\\n' \"$2:14: warning: avoid TODO (todo)\"\nprintf '%s\\n' '../outside/Relative.swift:15: warning: avoid TODO (todo)'\nprintf '%s\\n' \"$3:16: warning: avoid TODO (todo)\"\nprintf '%s\\n' \"$4:17: warning: avoid TODO (todo)\"\nprintf '%s\\n' \"$5:18: warning: avoid TODO (todo)\"\n",
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;

    let output = run_distill(
        &[
            "--cwd",
            root.to_str().ok_or("fixture path is not UTF-8")?,
            "swift",
            "lint",
            "--",
            diagnostic_path
                .to_str()
                .ok_or("diagnostic path is not UTF-8")?,
            traversal_path
                .to_str()
                .ok_or("traversal path is not UTF-8")?,
            symlink_external_path
                .to_str()
                .ok_or("symlink path is not UTF-8")?,
            symlink_parent_path
                .to_str()
                .ok_or("symlink traversal path is not UTF-8")?,
            in_root_parent_path
                .to_str()
                .ok_or("in-root traversal path is not UTF-8")?,
        ],
        &bin,
    )?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(
        "DIAG: Sources/LibraryScanner.swift: warning:348 [rule=function_body_length]: Function Body Length Violation"
    ));
    assert!(
        stdout.contains("DIAG: /opt/external/Dependency.swift: warning:12 [rule=todo]: avoid TODO")
    );
    assert!(stdout.contains(&format!(
        "DIAG: {}: warning:14 [rule=todo]: avoid TODO",
        traversal_path.display()
    )));
    assert!(stdout.contains(&format!(
        "DIAG: {}: warning:15 [rule=todo]: avoid TODO",
        escaped_relative_path.display()
    )));
    assert!(stdout.contains(&format!(
        "DIAG: {}: warning:16 [rule=todo]: avoid TODO",
        symlink_external_path.display()
    )));
    assert!(stdout.contains(&format!(
        "DIAG: {}: warning:17 [rule=todo]: avoid TODO",
        symlink_parent_path.display()
    )));
    assert!(
        stdout.contains("DIAG: Sources/LibraryScanner.swift: warning:18 [rule=todo]: avoid TODO")
    );
    fs::remove_dir_all(fixture_root)?;
    Ok(())
}

#[test]
fn large_swiftlint_output_is_summarized_by_dynamic_rule_and_severity() -> TestResult {
    let root = fixture("swiftlint-compaction")?;
    let sources = root.join("Sources");
    fs::create_dir(&sources)?;
    fs::write(sources.join("First.swift"), "")?;
    fs::write(sources.join("Second.swift"), "")?;

    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_swiftlint = bin.join("swiftlint");
    fs::write(
        &fake_swiftlint,
        r#"#!/bin/sh
index=1
while [ "$index" -le 51 ]; do
  if [ "$index" -eq 1 ]; then
    printf '%s\n' 'Sources/First.swift:1:1: error: Example violation (future_error_rule)'
  elif [ "$index" -le 26 ]; then
    printf 'Sources/First.swift:%s:1: warning: Example violation (future_rule)\n' "$index"
  else
    printf 'Sources/Second.swift:%s:1: warning: Example violation (future_rule)\n' "$index"
  fi
  index=$((index + 1))
done
exit 2
"#,
    )?;
    let mut permissions = fs::metadata(&fake_swiftlint)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_swiftlint, permissions)?;

    let cwd = root.to_str().ok_or("fixture path is not UTF-8")?;
    let output = run_distill(&["--cwd", cwd, "swift", "lint"], &bin)?;

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(
        "SwiftLint summary: diagnostics=51 files=2 rules=2 by-severity=error:1,warning:50"
    ));
    assert!(stdout.contains("SwiftLint error [rule=future_error_rule]: count=1 files=1"));
    assert!(stdout.contains("SwiftLint warning [rule=future_rule]: count=50 files=2"));
    assert!(stdout.contains("Sources/First.swift"));
    assert!(stdout.contains("Sources/Second.swift"));
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("DIAG:"))
            .count(),
        3
    );
    assert!(stdout.contains("class=lint owner=child"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn bare_invocation_displays_help() -> TestResult {
    let output = run_distill(&[], std::path::Path::new(""))?;

    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("Usage: distill"));
    assert!(stderr.contains("Example: distill --plan swift build"));
    assert!(!stderr.contains("FAILURE:"));
    assert_eq!(output.stdout, Vec::<u8>::new());
    Ok(())
}

#[test]
fn invalid_invocation_emits_wrapper_owned_usage_result() -> TestResult {
    let output = run_distill(&["cargo", "build", "--open"], std::path::Path::new(""))?;

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("distill"));
    assert!(!stdout.contains("COMMAND:"));
    assert!(stdout.contains("FAILURE: invalid command line"));
    assert!(stdout.contains("RESULT: failed (exit 2"));
    assert!(stdout.contains("class=usage owner=wrapper"));
    assert_eq!(stdout.lines().count(), 2);
    Ok(())
}
