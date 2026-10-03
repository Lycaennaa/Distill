#![allow(
    clippy::panic_in_result_fn,
    reason = "integration assertions are the intended failure mechanism"
)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[path = "support/mod.rs"]
mod support;

use support::{fixture, run_distill};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn xcode_stream_uses_app_bundle_and_applies_mandatory_email_redaction() -> TestResult {
    let root = xcode_fixture("xcode-stream", "macosx", "MacOSX")?;
    let tools = root.join("tools");
    let output = run_distill(
        &[
            "--cwd",
            path_arg(&root)?,
            "xcode",
            "build",
            "--open",
            "--stream",
        ],
        &tools,
    )?;

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("contact [EMAIL]"));
    assert!(!stdout.contains("person@example.com"));
    assert!(stdout.contains("kind=app"));
    assert!(!stdout.contains("class=success"));
    assert!(stdout.contains("RESULT: xcode build: succeeded"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn xcode_rejects_device_only_app_products_before_open() -> TestResult {
    let root = xcode_fixture("xcode-device", "iphoneos", "iPhoneOS")?;
    let tools = root.join("tools");
    let output = run_distill(
        &[
            "--cwd",
            path_arg(&root)?,
            "xcode",
            "build",
            "--open",
            "--wait",
        ],
        &tools,
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, Vec::<u8>::new());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("class=artifact owner=wrapper"));
    assert!(stdout.contains("not a macOS or Mac Catalyst app"));
    assert!(!stdout.contains("ACTION: runtime"));
    fs::remove_dir_all(root)?;
    Ok(())
}
#[test]
fn xcode_accepts_catalyst_products_and_rejects_multiple_apps() -> TestResult {
    let catalyst_root = xcode_fixture("xcode-catalyst", "maccatalyst", "MacOSX")?;
    let catalyst_tools = catalyst_root.join("tools");
    let catalyst = run_distill(
        &[
            "--cwd",
            path_arg(&catalyst_root)?,
            "xcode",
            "build",
            "--open",
            "--wait",
        ],
        &catalyst_tools,
    )?;
    assert!(catalyst.status.success());
    assert!(String::from_utf8(catalyst.stdout)?.contains("kind=app"));
    fs::remove_dir_all(catalyst_root)?;

    let root = xcode_fixture("xcode-multiple", "macosx", "MacOSX")?;
    let tools = root.join("tools");
    let products = root.join("products");
    create_test_app(&products.join("Other.app"), "Other")?;
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = -showBuildSettings ]; then\n  printf '%s\\n' 'Build settings for action build and target Demo:' '    FULL_PRODUCT_NAME = Demo.app' '    TARGET_BUILD_DIR = {}' '    PLATFORM_NAME = macosx' '    SDKROOT = macosx.sdk' 'Build settings for action build and target Other:' '    FULL_PRODUCT_NAME = Other.app' '    TARGET_BUILD_DIR = {}' '    PLATFORM_NAME = macosx' '    SDKROOT = macosx.sdk'\nfi\n",
        products.display(),
        products.display()
    );
    write_executable(&tools.join("xcodebuild"), &script)?;
    let multiple = run_distill(
        &["--cwd", path_arg(&root)?, "xcode", "build", "--open"],
        &tools,
    )?;
    assert_eq!(multiple.status.code(), Some(1));
    let stdout = String::from_utf8(multiple.stdout)?;
    assert!(stdout.contains("multiple macOS app products"));
    assert!(stdout.contains("Demo.app, Other.app"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn xcode_detached_open_reports_started_without_a_pid() -> TestResult {
    let root = xcode_fixture("xcode-detached", "macosx", "MacOSX")?;
    let tools = root.join("tools");
    let output = run_distill(
        &["--cwd", path_arg(&root)?, "xcode", "build", "--open"],
        &tools,
    )?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("ACTION: open state=started"));
    assert!(!stdout.contains("pid="));
    fs::remove_dir_all(root)?;
    Ok(())
}

fn create_test_app(app: &Path, executable_name: &str) -> std::io::Result<()> {
    let executable = app.join("Contents/MacOS").join(executable_name);
    fs::create_dir_all(
        executable
            .parent()
            .ok_or_else(|| std::io::Error::other("test app executable has no parent"))?,
    )?;
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>{executable_name}</string><key>CFBundleSupportedPlatforms</key><array><string>MacOSX</string></array><key>DTPlatformName</key><string>macosx</string></dict></plist>"
        ),
    )?;
    write_executable(&executable, "#!/bin/sh\nexit 0\n")
}
fn xcode_fixture(name: &str, platform: &str, supported: &str) -> Result<PathBuf, Box<dyn Error>> {
    let root = fixture(name)?;
    let project = root.join("Demo.xcodeproj");
    let products = root.join("products");
    let app = products.join("Demo.app");
    let executable = app.join("Contents/MacOS/Demo");
    fs::create_dir_all(project)?;
    fs::create_dir_all(executable.parent().ok_or("app executable has no parent")?)?;
    fs::create_dir_all(&products)?;
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>Demo</string><key>CFBundleSupportedPlatforms</key><array><string>{supported}</string></array><key>DTPlatformName</key><string>{platform}</string></dict></plist>"
        ),
    )?;
    write_executable(
        &executable,
        "#!/bin/sh\nprintf 'contact person@example.com\\n'\n",
    )?;
    let tools = root.join("tools");
    fs::create_dir_all(&tools)?;
    let xcode_script = format!(
        "#!/bin/sh\nif [ \"$1\" = -showBuildSettings ]; then\n  printf '%s\\n' 'Build settings for action build and target Demo:'\n  printf '%s\\n' '    FULL_PRODUCT_NAME = Demo.app'\n  printf '%s\\n' '    TARGET_BUILD_DIR = {}'\n  printf '%s\\n' '    PLATFORM_NAME = {}'\n  printf '%s\\n' '    SDKROOT = {}.sdk'\nfi\n",
        products.display(),
        platform,
        platform
    );
    write_executable(&tools.join("xcodebuild"), &xcode_script)?;
    write_executable(&tools.join("open"), "#!/bin/sh\nexit 0\n")?;
    Ok(root)
}

fn write_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

fn path_arg(path: &Path) -> Result<&str, Box<dyn Error>> {
    path.to_str()
        .ok_or_else(|| "fixture path is not UTF-8".into())
}
