use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::{Action, Invocation, Tool};
use crate::discovery_error::DiscoveryError;
use crate::process::CancellationToken;
use crate::tool_args::{ExplicitPath, collect_path_options};

/// Filesystem marker that selected a tool's working root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    XcodeWorkspace,
    XcodeProject,
    SwiftPackage,
    CargoPackage,
    WorkingDirectory,
}

impl fmt::Display for RootKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::XcodeWorkspace => "xcworkspace",
            Self::XcodeProject => "xcodeproj",
            Self::SwiftPackage => "swift-package",
            Self::CargoPackage => "cargo-package",
            Self::WorkingDirectory => "working-directory",
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DiscoveryContext<'a> {
    pub(super) deadline: Option<Instant>,
    pub(super) cancellation: Option<&'a CancellationToken>,
}

impl<'a> DiscoveryContext<'a> {
    pub(super) const fn new(
        deadline: Option<Instant>,
        cancellation: Option<&'a CancellationToken>,
    ) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }
}

/// Filesystem-only resolution used by command construction and `--plan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    root: PathBuf,
    marker: Option<PathBuf>,
    kind: RootKind,
    explicit: bool,
}

impl Discovery {
    pub(super) const fn new(
        root: PathBuf,
        marker: Option<PathBuf>,
        kind: RootKind,
        explicit: bool,
    ) -> Self {
        Self {
            root,
            marker,
            kind,
            explicit,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn marker(&self) -> Option<&Path> {
        self.marker.as_deref()
    }

    #[must_use]
    pub const fn kind(&self) -> RootKind {
        self.kind
    }

    #[must_use]
    pub const fn is_explicit(&self) -> bool {
        self.explicit
    }
}

/// Discover the single tool root visible from an invocation's starting path.
///
/// # Errors
/// Returns an error when the starting path or the unique tool root is invalid.
pub fn discover(invocation: &Invocation) -> Result<Discovery, DiscoveryError> {
    discover_with_context(invocation, DiscoveryContext::new(None, None))
}

pub(crate) fn discover_with_context(
    invocation: &Invocation,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    let start = starting_directory(invocation.options().cwd.as_deref(), context)?;
    match (invocation.tool(), invocation.action()) {
        (Tool::Xcode, action) => {
            crate::xcode_discovery::discover_xcode(invocation, action, &start, context)
        }
        (Tool::Swift, Action::Lint) => discover_swift_lint(invocation, &start, context),
        (Tool::Swift, action @ (Action::Build | Action::Test)) => {
            discover_swift(invocation, action, &start, context)
        }
        (
            Tool::Cargo,
            action @ (Action::Build
            | Action::Test
            | Action::Fmt
            | Action::Package
            | Action::Clippy
            | Action::Install
            | Action::Xtask),
        ) => crate::cargo_discovery::discover_cargo(invocation, action, &start, context),
        _ => Err(DiscoveryError::Missing {
            tool: invocation.tool(),
            action: invocation.action(),
            start,
        }),
    }
}

fn starting_directory(
    path: Option<&Path>,
    context: DiscoveryContext<'_>,
) -> Result<PathBuf, DiscoveryError> {
    check_deadline(context, Path::new("."))?;
    let base = std::env::current_dir().map_err(|error| DiscoveryError::StartDirectory {
        path: PathBuf::from("."),
        reason: error.to_string(),
    })?;
    let requested = path.map_or_else(|| base.clone(), |path| resolve_relative(&base, path));
    check_deadline(context, &requested)?;
    let metadata = fs::metadata(&requested).map_err(|error| DiscoveryError::StartDirectory {
        path: requested.clone(),
        reason: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(DiscoveryError::StartDirectory {
            path: requested,
            reason: "path is not a directory".to_owned(),
        });
    }
    let canonical =
        fs::canonicalize(&requested).map_err(|error| DiscoveryError::StartDirectory {
            path: requested,
            reason: error.to_string(),
        })?;
    check_deadline(context, &canonical)?;
    Ok(canonical)
}

fn discover_swift_lint(
    invocation: &Invocation,
    start: &Path,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    discover_swift_package(invocation, Action::Lint, start, true, context)
}

fn discover_swift(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    discover_swift_package(invocation, action, start, false, context)
}

fn discover_swift_package(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    allow_working_directory: bool,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    let explicit = collect_path_options(
        invocation.forwarded_args(),
        &["--package-path", "--manifest-path"],
    );
    if explicit.len() > 1 {
        return Err(DiscoveryError::ConflictingExplicit {
            tool: Tool::Swift,
            action,
            options: explicit.into_iter().map(|value| value.option).collect(),
        });
    }
    if let Some(value) = explicit.first() {
        let root = validate_package_path(invocation, action, start, value, context)?;
        return Ok(Discovery::new(
            root.clone(),
            Some(root.join("Package.swift")),
            RootKind::SwiftPackage,
            true,
        ));
    }

    let choices = marker_candidates(start, "Package.swift", context)?;
    if choices.is_empty() && allow_working_directory {
        return Ok(Discovery::new(
            start.to_owned(),
            None,
            RootKind::WorkingDirectory,
            false,
        ));
    }
    let marker = choose_one(Tool::Swift, action, start, choices)?;
    let root = marker
        .parent()
        .map_or_else(|| marker.clone(), Path::to_path_buf);
    Ok(Discovery::new(
        root,
        Some(marker),
        RootKind::SwiftPackage,
        false,
    ))
}

fn validate_package_path(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    value: &ExplicitPath,
    context: DiscoveryContext<'_>,
) -> Result<PathBuf, DiscoveryError> {
    check_deadline(context, start)?;
    let path = resolve_relative(start, &value.path);
    if value.option == "--package-path" {
        if path.is_dir() && path.join("Package.swift").is_file() {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                invalid_explicit(invocation.tool(), action, value, &error.to_string())
            })?;
            check_deadline(context, &canonical)?;
            return Ok(canonical);
        }
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "--package-path must be an existing package directory",
        ));
    }
    if value.option == "--manifest-path"
        && path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name == OsStr::new("Package.swift"))
    {
        let canonical = fs::canonicalize(&path).map_err(|error| {
            invalid_explicit(invocation.tool(), action, value, &error.to_string())
        })?;
        check_deadline(context, &canonical)?;
        return canonical_parent(invocation, action, value, &canonical, context);
    }
    Err(invalid_explicit(
        invocation.tool(),
        action,
        value,
        "--manifest-path must be an existing Package.swift file",
    ))
}

pub(super) fn validate_manifest_path(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    value: &ExplicitPath,
    filename: &str,
    context: DiscoveryContext<'_>,
) -> Result<PathBuf, DiscoveryError> {
    check_deadline(context, start)?;
    let path = resolve_relative(start, &value.path);
    if !(path.is_file()
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name == filename))
    {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            &format!("path must be an existing {filename}"),
        ));
    }
    let canonical = fs::canonicalize(&path)
        .map_err(|error| invalid_explicit(invocation.tool(), action, value, &error.to_string()))?;
    check_deadline(context, &canonical)?;
    Ok(canonical)
}

fn canonical_parent(
    invocation: &Invocation,
    action: Action,
    value: &ExplicitPath,
    path: &Path,
    context: DiscoveryContext<'_>,
) -> Result<PathBuf, DiscoveryError> {
    let Some(parent) = path.parent() else {
        return Err(invalid_explicit(
            invocation.tool(),
            action,
            value,
            "path has no parent directory",
        ));
    };
    check_deadline(context, parent)?;
    let canonical = fs::canonicalize(parent)
        .map_err(|error| invalid_explicit(invocation.tool(), action, value, &error.to_string()))?;
    check_deadline(context, &canonical)?;
    Ok(canonical)
}
pub(super) fn invalid_explicit(
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

pub(super) fn marker_candidates(
    start: &Path,
    filename: &str,
    context: DiscoveryContext<'_>,
) -> Result<Vec<PathBuf>, DiscoveryError> {
    let mut choices = Vec::new();
    for ancestor in ancestors(start) {
        check_deadline(context, &ancestor)?;
        let marker = ancestor.join(filename);
        if marker.is_file() {
            let canonical = fs::canonicalize(&marker).map_err(|error| DiscoveryError::Io {
                path: marker,
                reason: error.to_string(),
            })?;
            check_deadline(context, &canonical)?;
            choices.push(canonical);
        }
    }
    choices.sort_unstable();
    choices.dedup();
    check_deadline(context, start)?;
    Ok(choices)
}

pub(super) fn choose_one(
    tool: Tool,
    action: Action,
    start: &Path,
    mut choices: Vec<PathBuf>,
) -> Result<PathBuf, DiscoveryError> {
    choices.sort_unstable();
    choices.dedup();
    match choices.len() {
        0 => Err(DiscoveryError::Missing {
            tool,
            action,
            start: start.to_owned(),
        }),
        1 => choices.pop().ok_or_else(|| DiscoveryError::Missing {
            tool,
            action,
            start: start.to_owned(),
        }),
        _ => Err(DiscoveryError::Ambiguous {
            tool,
            action,
            choices,
        }),
    }
}

pub(super) fn ancestors(start: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut current = Some(start);
    while let Some(path) = current {
        paths.push(path.to_owned());
        let parent = path.parent();
        current = parent.filter(|candidate| *candidate != path);
    }
    paths
}

pub(super) fn resolve_relative(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}

pub(super) fn check_deadline(
    context: DiscoveryContext<'_>,
    path: &Path,
) -> Result<(), DiscoveryError> {
    if context
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        Err(DiscoveryError::Timeout {
            path: path.to_owned(),
        })
    } else if context
        .cancellation
        .is_some_and(CancellationToken::is_cancelled)
    {
        Err(DiscoveryError::Interrupted {
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}
