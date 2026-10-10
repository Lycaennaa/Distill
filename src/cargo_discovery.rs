use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{Action, Invocation, Tool};
use crate::discovery::{
    Discovery, DiscoveryContext, RootKind, check_deadline, choose_one, invalid_explicit,
    marker_candidates, resolve_relative, validate_manifest_path,
};
use crate::discovery_error::DiscoveryError;
use crate::tool_args::{ExplicitPath, collect_path_options};
pub fn discover_cargo(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    let explicit = match action {
        Action::Xtask => Vec::new(),
        Action::Install => collect_path_options(invocation.forwarded_args(), &["--path"]),
        _ => collect_path_options(invocation.forwarded_args(), &["--manifest-path"]),
    };
    if explicit.len() > 1 {
        return Err(DiscoveryError::ConflictingExplicit {
            tool: Tool::Cargo,
            action,
            options: explicit.into_iter().map(|value| value.option).collect(),
        });
    }
    if let Some(value) = explicit.first() {
        if action == Action::Install {
            let root = validate_cargo_path(invocation, action, start, value, context)?;
            return Ok(Discovery::new(
                root.clone(),
                Some(root.join("Cargo.toml")),
                RootKind::CargoPackage,
                true,
            ));
        }
        let marker =
            validate_manifest_path(invocation, action, start, value, "Cargo.toml", context)?;
        let root = marker
            .parent()
            .map_or_else(|| marker.clone(), Path::to_path_buf);
        return Ok(Discovery::new(
            root,
            Some(marker),
            RootKind::CargoPackage,
            true,
        ));
    }
    if action == Action::Install {
        return Ok(Discovery::new(
            start.to_owned(),
            None,
            RootKind::WorkingDirectory,
            false,
        ));
    }
    let marker = choose_one(
        Tool::Cargo,
        action,
        start,
        marker_candidates(start, "Cargo.toml", context)?,
    )?;
    let root = marker
        .parent()
        .map_or_else(|| marker.clone(), Path::to_path_buf);
    Ok(Discovery::new(
        root,
        Some(marker),
        RootKind::CargoPackage,
        false,
    ))
}

fn validate_cargo_path(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    value: &ExplicitPath,
    context: DiscoveryContext<'_>,
) -> Result<PathBuf, DiscoveryError> {
    check_deadline(context, start)?;
    if value.path.as_os_str().is_empty() {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "--path must not be empty",
        ));
    }
    let path = resolve_relative(start, &value.path);
    if !(path.is_dir() && path.join("Cargo.toml").is_file()) {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "--path must be an existing directory containing Cargo.toml",
        ));
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|error| invalid_explicit(invocation.tool(), action, value, &error.to_string()))?;
    check_deadline(context, &canonical)?;
    Ok(canonical)
}
