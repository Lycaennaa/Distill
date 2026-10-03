use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::cli::Invocation;
use crate::process::{CancellationToken, CommandSpec, Execution};

use super::products::{
    BuiltProduct, artifact_failure, is_executable_file, metadata_spec, parse_json,
    parse_option_values, query_metadata, single_selector,
};

pub(super) fn resolve(
    invocation: &Invocation,
    build: &CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<BuiltProduct, super::products::Failure> {
    let root = build
        .cwd()
        .ok_or_else(|| artifact_failure("build root is unavailable"))?;
    let packages = parse_option_values(invocation.forwarded_args(), "--package", Some("-p"))?;
    let package_selector = single_selector(&packages)?;
    let binaries = parse_option_values(invocation.forwarded_args(), "--bin", None)?;
    let bin_selector = single_selector(&binaries)?;

    let spec = metadata_spec(
        "cargo",
        [
            OsString::from("metadata"),
            OsString::from("--format-version"),
            OsString::from("1"),
            OsString::from("--no-deps"),
        ],
        root,
    );
    let text = query_metadata(
        execution,
        &spec,
        deadline,
        cancellation,
        "Cargo product metadata failed",
    )?;
    let json = parse_json(&text, "Cargo product metadata was invalid")?;
    let packages_json = json
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| artifact_failure("Cargo product metadata was incomplete"))?;
    let (package, binary) = choose_binary(packages_json, &json, package_selector, bin_selector)?;

    let target_root = json
        .get("target_directory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| artifact_failure("Cargo target directory was unavailable"))?;
    let path = binary_path(root, &target_root, invocation.forwarded_args(), &binary);
    if !is_executable_file(&path) {
        return Err(artifact_failure("selected Cargo binary is missing"));
    }
    Ok(BuiltProduct::CargoBinary {
        package,
        name: binary,
        path,
    })
}

fn choose_binary(
    packages_json: &[Value],
    metadata: &Value,
    package_selector: Option<&str>,
    bin_selector: Option<&str>,
) -> Result<(String, String), super::products::Failure> {
    let default_members = metadata
        .get("workspace_default_members")
        .and_then(Value::as_array)
        .map(|members| members.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut available = Vec::new();
    for package in packages_json {
        let Some(id) = package.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let version = package.get("version").and_then(Value::as_str).unwrap_or("");
        if let Some(selector) = package_selector
            && selector != name
            && selector != id
            && selector != format!("{name}@{version}")
        {
            continue;
        }
        if package_selector.is_none()
            && !default_members.is_empty()
            && !default_members.contains(&id)
        {
            continue;
        }
        if let Some(targets) = package.get("targets").and_then(Value::as_array) {
            for target in targets {
                if is_binary_target(target)
                    && let Some(binary) = target.get("name").and_then(Value::as_str)
                {
                    available.push((name.to_owned(), binary.to_owned()));
                }
            }
        }
    }
    available.sort_unstable();
    available.dedup();
    let candidates = bin_selector.map_or_else(
        || available.clone(),
        |selector| {
            available
                .iter()
                .filter(|(_package, name)| name == selector)
                .cloned()
                .collect()
        },
    );
    if candidates.len() != 1 {
        let choices = available
            .iter()
            .map(|(package, binary)| format!("{package}:{binary}"))
            .collect::<Vec<_>>();
        let message = if choices.is_empty() {
            "no runnable Cargo binary found".to_owned()
        } else if bin_selector.is_some() {
            format!(
                "selected Cargo binary is unavailable; choices: {}",
                choices.join(", ")
            )
        } else {
            format!(
                "multiple Cargo binaries found; choose one of: {}",
                choices.join(", ")
            )
        };
        return Err(artifact_failure(message));
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| artifact_failure("Cargo binary selection failed"))
}

pub(super) fn run_spec(
    invocation: &Invocation,
    build: &CommandSpec,
    package: &str,
    binary: &str,
) -> Result<CommandSpec, super::products::Failure> {
    let root = build
        .cwd()
        .ok_or_else(|| artifact_failure("build root is unavailable"))?;
    let mut arguments = vec![OsString::from("run")];
    if build
        .arguments()
        .iter()
        .skip(1)
        .any(|argument| argument == "--quiet")
    {
        arguments.push(OsString::from("--quiet"));
    }
    let forwarded = invocation.forwarded_args();
    let mut index = 0_usize;
    while let Some(argument) = forwarded.get(index) {
        let Some(text) = argument.to_str() else {
            arguments.push(argument.clone());
            index = index.saturating_add(1);
            continue;
        };
        if text == "--" {
            break;
        }
        if text == "--bin" || text == "--package" || text == "-p" {
            index = index.saturating_add(2);
            continue;
        }
        if text.starts_with("--bin=")
            || text.starts_with("--package=")
            || (text.starts_with("-p") && text.len() > 2)
            || text == "--quiet"
        {
            index = index.saturating_add(1);
            continue;
        }
        if [
            "--all-targets",
            "--bins",
            "--examples",
            "--tests",
            "--benches",
            "--lib",
        ]
        .contains(&text)
        {
            index = index.saturating_add(1);
            continue;
        }
        arguments.push(argument.clone());
        index = index.saturating_add(1);
    }
    arguments.extend([
        OsString::from("--package"),
        OsString::from(package),
        OsString::from("--bin"),
        OsString::from(binary),
    ]);
    Ok(CommandSpec::new("cargo")
        .args(arguments)
        .label("cargo runtime")
        .with_status_class(crate::status::StatusClass::Runtime)
        .current_dir(root.to_owned()))
}

fn is_binary_target(target: &Value) -> bool {
    target
        .get("kind")
        .and_then(Value::as_array)
        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
}

fn binary_path(
    root: &Path,
    target_root: &Path,
    arguments: &[std::ffi::OsString],
    binary: &str,
) -> PathBuf {
    let profile = option_value(arguments, "--profile")
        .or_else(|| {
            arguments
                .iter()
                .any(|value| value == "--release")
                .then(|| "release".to_owned())
        })
        .unwrap_or_else(|| "debug".to_owned());
    let target_dir = option_value(arguments, "--target-dir").map_or_else(
        || target_root.to_owned(),
        |directory| {
            let directory = PathBuf::from(directory);
            if directory.is_absolute() {
                directory
            } else {
                root.join(directory)
            }
        },
    );
    let target = option_value(arguments, "--target");
    let mut path = target_dir;
    if let Some(target) = target {
        path.push(target);
    }
    path.push(profile);
    path.push(binary);
    path
}

fn option_value(arguments: &[OsString], name: &str) -> Option<String> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        let text = argument.to_str()?;
        if text == name {
            arguments
                .get(index.saturating_add(1))?
                .to_str()
                .map(str::to_owned)
        } else {
            text.strip_prefix(&format!("{name}=")).map(str::to_owned)
        }
    })
}
