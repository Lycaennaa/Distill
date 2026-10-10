#![allow(
    clippy::panic_in_result_fn,
    reason = "fixture tests use assertions for readable failure output"
)]
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use distill::{CommandBuildError, DiscoveryError, RootKind, command_for, parse};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> TestResult<PathBuf> {
    let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root =
        PathBuf::from("target").join(format!("distill-phase2-{name}-{}-{id}", std::process::id()));
    drop(fs::remove_dir_all(&root));
    fs::create_dir_all(&root)?;
    Ok(fs::canonicalize(root)?)
}

fn invocation(cwd: &Path, args: &[&str]) -> TestResult<distill::Invocation> {
    let mut values = vec![
        OsString::from("distill"),
        OsString::from("--cwd"),
        cwd.as_os_str().to_owned(),
    ];
    values.extend(args.iter().map(OsString::from));
    parse(values).map_err(|error| io::Error::other(error.to_string()).into())
}

fn run_distill(arguments: &[&str], path: &Path) -> TestResult<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_distill"))
        .args(arguments)
        .env("PATH", path)
        .output()?)
}

#[test]
fn explicit_cargo_manifest_selects_its_root() -> TestResult {
    let root = fixture("cargo")?;
    let nested = root.join("src").join("nested");
    fs::create_dir_all(&nested)?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;

    let spec = command_for(&invocation(
        &nested,
        &[
            "cargo",
            "build",
            "--",
            "--manifest-path",
            "../../Cargo.toml",
            "--all-features",
        ],
    )?)?;
    assert_eq!(spec.cwd(), Some(root.as_path()));
    assert_eq!(spec.arguments().first(), Some(&OsString::from("build")));
    assert_eq!(spec.arguments().get(1), Some(&OsString::from("--quiet")));
    assert_eq!(
        spec.arguments().last(),
        Some(&OsString::from("--all-features"))
    );

    let Some(manifest_index) = spec
        .arguments()
        .iter()
        .position(|argument| argument == "--manifest-path")
    else {
        return Err(io::Error::other("manifest argument should be forwarded").into());
    };
    assert_eq!(
        spec.arguments().get(manifest_index.saturating_add(1)),
        Some(&root.join("Cargo.toml").into_os_string())
    );
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn cargo_install_resolves_local_path_and_propagates_child_status() -> TestResult {
    let root = fixture("cargo-install")?;
    let package = root.join("local-package");
    fs::create_dir(&package)?;
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    let fake_cargo = bin.join("cargo");
    fs::write(
        &fake_cargo,
        r#"#!/bin/sh
case "$*" in
  *--fail-install*)
    printf '%s\n' '{"reason":"compiler-message","message":{"message":"missing function","level":"error","code":{"code":"E0599"},"spans":[{"file_name":"src/lib.rs","line_start":7,"column_start":3,"is_primary":true}],"children":[]}}'
    exit 7
    ;;
esac
"#,
    )?;
    let mut permissions = fs::metadata(&fake_cargo)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&fake_cargo, permissions)?;
    let cwd = root.to_str().ok_or("fixture path is not UTF-8")?;

    let spec = command_for(&invocation(
        &root,
        &[
            "cargo",
            "install",
            "--",
            "--path",
            "local-package",
            "--locked",
        ],
    )?)?;
    assert_eq!(spec.cwd(), Some(package.as_path()));
    assert_eq!(spec.arguments().len(), 6);
    assert_eq!(
        spec.arguments(),
        &[
            OsString::from("install"),
            OsString::from("--quiet"),
            OsString::from("--message-format=json-diagnostic-rendered-ansi"),
            OsString::from("--path"),
            package.as_os_str().to_os_string(),
            OsString::from("--locked")
        ]
    );

    let output = run_distill(
        &[
            "--cwd",
            cwd,
            "cargo",
            "install",
            "--",
            "--path",
            "local-package",
            "--locked",
        ],
        &bin,
    )?;
    assert!(output.status.success());

    let failed = run_distill(
        &[
            "--cwd",
            cwd,
            "cargo",
            "install",
            "--",
            "--path",
            "local-package",
            "--locked",
            "--fail-install",
        ],
        &bin,
    )?;
    assert_eq!(failed.status.code(), Some(7));
    let stdout = String::from_utf8(failed.stdout)?;
    assert!(stdout.contains("class=compile owner=child"));
    assert!(stdout.contains("E0599"));
    assert!(stdout.contains("missing function"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_install_without_local_path_needs_no_manifest() -> TestResult {
    let root = fixture("cargo-install-registry")?;
    let spec = command_for(&invocation(&root, &["cargo", "install", "ripgrep"])?)?;

    assert_eq!(spec.cwd(), Some(root.as_path()));
    assert_eq!(spec.arguments().len(), 4);
    assert_eq!(
        spec.arguments(),
        &[
            OsString::from("install"),
            OsString::from("--quiet"),
            OsString::from("--message-format=json-diagnostic-rendered-ansi"),
            OsString::from("ripgrep")
        ]
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_install_rejects_directory_without_manifest() -> TestResult {
    let root = fixture("cargo-install-invalid-path")?;
    let empty = root.join("empty");
    fs::create_dir(&empty)?;
    let error = match command_for(&invocation(
        &root,
        &["cargo", "install", "--", "--path", "empty"],
    )?) {
        Ok(_spec) => return Err(io::Error::other("invalid Cargo install path should fail").into()),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CommandBuildError::Discovery(DiscoveryError::InvalidExplicit {
            tool: distill::Tool::Cargo,
            action: distill::Action::Install,
            ..
        })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cargo_install_rejects_empty_local_path() -> TestResult {
    let root = fixture("cargo-install-empty-path")?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    let error = match command_for(&invocation(&root, &["cargo", "install", "--", "--path="])?) {
        Ok(_spec) => return Err(io::Error::other("empty Cargo install path should fail").into()),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CommandBuildError::Discovery(DiscoveryError::InvalidExplicit {
            tool: distill::Tool::Cargo,
            action: distill::Action::Install,
            ..
        })
    ));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn explicit_xcode_project_suppresses_auto_selection() -> TestResult {
    let root = fixture("xcode")?;
    let project = root.join("Demo.xcodeproj");
    fs::create_dir(&project)?;

    let spec = command_for(&invocation(
        &root,
        &["xcode", "build", "--", "-project", "Demo.xcodeproj"],
    )?)?;
    assert_eq!(spec.cwd(), Some(root.as_path()));
    assert_eq!(spec.arguments().first(), Some(&OsString::from("build")));
    assert!(spec.arguments().contains(&OsString::from("-project")));
    assert!(spec.arguments().contains(&OsString::from("-quiet")));
    assert_eq!(
        spec.arguments()
            .iter()
            .filter(|argument| *argument == &OsString::from("-project"))
            .count(),
        1
    );
    assert!(spec.arguments().contains(&project.as_os_str().to_owned()));
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn inferred_xcode_project_is_injected_and_sets_project_root() -> TestResult {
    let root = fixture("xcode-inferred")?;
    let project = root.join("Demo.xcodeproj");
    let nested = root.join("Sources");
    fs::create_dir(&project)?;
    fs::create_dir(&nested)?;

    let spec = command_for(&invocation(&nested, &["xcode", "build"])?)?;
    assert_eq!(spec.cwd(), Some(root.as_path()));
    assert_eq!(spec.arguments().first(), Some(&OsString::from("build")));
    assert!(spec.arguments().contains(&OsString::from("-project")));
    assert!(spec.arguments().contains(&project.as_os_str().to_owned()));
    assert!(spec.arguments().contains(&OsString::from("-quiet")));
    assert_eq!(
        spec.injected_options(),
        &[OsString::from("-project"), OsString::from("-quiet")]
    );
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn swiftlint_without_a_package_uses_selected_directory() -> TestResult {
    let root = fixture("swiftlint")?;
    let spec = command_for(&invocation(
        &root,
        &["swift", "lint", "--", "--config", ".swiftlint.yml"],
    )?)?;
    assert_eq!(spec.cwd(), Some(root.as_path()));
    assert_eq!(
        spec.arguments(),
        &[
            OsString::from("lint"),
            OsString::from("--config"),
            OsString::from(".swiftlint.yml")
        ]
    );
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn multiple_ancestor_manifests_are_rejected() -> TestResult {
    let root = fixture("ambiguous")?;
    let nested = root.join("nested");
    fs::create_dir(&nested)?;
    for directory in [&root, &nested] {
        fs::write(
            directory.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )?;
    }

    let error = match command_for(&invocation(&nested, &["cargo", "build"])?) {
        Ok(_spec) => return Err(io::Error::other("ambiguous roots should fail").into()),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        CommandBuildError::Discovery(DiscoveryError::Ambiguous {
            tool: distill::Tool::Cargo,
            action: distill::Action::Build,
            ..
        })
    ));
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn plan_resolves_filesystem_intent_without_child_metadata() -> TestResult {
    let root = fixture("plan")?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )?;
    let invocation = invocation(
        &root,
        &[
            "--plan",
            "cargo",
            "clippy",
            "--",
            "--manifest-path",
            "Cargo.toml",
        ],
    )?;
    assert_eq!(invocation.tool(), distill::Tool::Cargo);
    assert_eq!(invocation.action(), distill::Action::Clippy);
    let spec = command_for(&invocation)?;
    assert_eq!(spec.cwd(), Some(root.as_path()));
    drop(fs::remove_dir_all(root));
    Ok(())
}

#[test]
fn root_kind_is_exposed_for_explicit_xcode_projects() -> TestResult {
    let root = fixture("kind")?;
    let project = root.join("Demo.xcodeproj");
    fs::create_dir(&project)?;
    let invocation = invocation(
        &root,
        &["xcode", "list", "--", "-project", "Demo.xcodeproj"],
    )?;
    let discovery = distill::discover(&invocation)?;
    assert_eq!(discovery.kind(), RootKind::XcodeProject);
    assert!(discovery.is_explicit());
    drop(fs::remove_dir_all(root));
    Ok(())
}
#[test]
fn rejects_invalid_swift_explicit_path_shapes() -> TestResult {
    let root = fixture("swift-paths")?;
    fs::write(root.join("Package.swift"), "// fixture package\n")?;
    for args in [
        ["swift", "build", "--", "--package-path", "Package.swift"],
        ["swift", "build", "--", "--manifest-path", "."],
    ] {
        let error = match command_for(&invocation(&root, &args)?) {
            Ok(_spec) => return Err(io::Error::other("invalid Swift path should fail").into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            CommandBuildError::Discovery(DiscoveryError::InvalidExplicit {
                tool: distill::Tool::Swift,
                action: distill::Action::Build,
                ..
            })
        ));
    }
    drop(fs::remove_dir_all(root));
    Ok(())
}
