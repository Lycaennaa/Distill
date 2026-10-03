use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{Action, Invocation, Tool};
use crate::discovery::{Discovery, DiscoveryContext, RootKind, check_deadline};
use crate::discovery_error::DiscoveryError;
use crate::tool_args::{ExplicitPath, collect_path_options};

pub fn discover_xcode(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    let explicit = collect_path_options(invocation.forwarded_args(), &["-workspace", "-project"]);
    if explicit.len() > 1 {
        return Err(DiscoveryError::ConflictingExplicit {
            tool: Tool::Xcode,
            action,
            options: explicit.into_iter().map(|value| value.option).collect(),
        });
    }
    if let Some(value) = explicit.first() {
        let (marker, kind) = validate_xcode_path(invocation, action, start, value, context)?;
        let root = marker
            .parent()
            .map_or_else(|| marker.clone(), Path::to_path_buf);
        return Ok(Discovery::new(root, Some(marker), kind, true));
    }

    let mut choices = Vec::new();
    for ancestor in crate::discovery::ancestors(start) {
        check_deadline(context, &ancestor)?;
        let entries = fs::read_dir(&ancestor).map_err(|error| DiscoveryError::Io {
            path: ancestor.clone(),
            reason: error.to_string(),
        })?;
        for entry in entries {
            check_deadline(context, &ancestor)?;
            let entry = entry.map_err(|error| DiscoveryError::Io {
                path: ancestor.clone(),
                reason: error.to_string(),
            })?;
            let path = entry.path();
            check_deadline(context, &path)?;
            if is_xcode_marker(&path) {
                let marker = fs::canonicalize(&path).map_err(|error| DiscoveryError::Io {
                    path,
                    reason: error.to_string(),
                })?;
                check_deadline(context, &marker)?;
                choices.push(marker);
            }
        }
    }
    check_deadline(context, start)?;
    choices.sort_unstable();
    choices.dedup();
    let marker = crate::discovery::choose_one(Tool::Xcode, action, start, choices)?;
    let kind = xcode_kind(&marker).ok_or_else(|| DiscoveryError::Io {
        path: marker.clone(),
        reason: "selected path is not an Xcode marker".to_owned(),
    })?;
    let root = marker
        .parent()
        .map_or_else(|| marker.clone(), Path::to_path_buf);
    Ok(Discovery::new(root, Some(marker), kind, false))
}

fn validate_xcode_path(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    value: &ExplicitPath,
    context: DiscoveryContext<'_>,
) -> Result<(PathBuf, RootKind), DiscoveryError> {
    check_deadline(context, start)?;
    let path = crate::discovery::resolve_relative(start, &value.path);
    let Some(kind) = xcode_kind(&path) else {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "path must end in .xcworkspace or .xcodeproj",
        ));
    };
    let expected = match value.option.as_str() {
        "-workspace" => RootKind::XcodeWorkspace,
        "-project" => RootKind::XcodeProject,
        _ => kind,
    };
    if kind != expected {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "option does not match the project path extension",
        ));
    }
    if !path.is_dir() {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "path is not an existing directory",
        ));
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|error| invalid_explicit(invocation.tool(), action, value, &error.to_string()))?;
    check_deadline(context, &canonical)?;
    Ok((canonical, kind))
}

fn invalid_explicit(
    tool: Tool,
    action: Action,
    value: &ExplicitPath,
    reason: &str,
) -> DiscoveryError {
    DiscoveryError::InvalidExplicit {
        tool,
        action,
        option: value.option.clone(),
        path: value.path.clone(),
        reason: reason.to_owned(),
    }
}

fn is_xcode_marker(path: &Path) -> bool {
    path.is_dir() && xcode_kind(path).is_some()
}

fn xcode_kind(path: &Path) -> Option<RootKind> {
    match path.extension().and_then(OsStr::to_str) {
        Some("xcworkspace") => Some(RootKind::XcodeWorkspace),
        Some("xcodeproj") => Some(RootKind::XcodeProject),
        _ => None,
    }
}
