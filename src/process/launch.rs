use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::model::{CancellationToken, CommandSpec, DetachedLaunchError};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
pub(super) fn resolve_program(program: &OsStr) -> Option<PathBuf> {
    let process_cwd = std::env::current_dir().ok()?;
    let path = Path::new(program);
    if path.is_absolute() {
        return is_executable(path).then(|| path.to_owned());
    }
    if program.to_string_lossy().contains('/') {
        let candidate = process_cwd.join(path);
        return is_executable(&candidate).then_some(candidate);
    }

    let path_variable = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path_variable) {
        let directory = if directory.as_os_str().is_empty() {
            process_cwd.clone()
        } else if directory.is_absolute() {
            directory
        } else {
            process_cwd.join(directory)
        };
        let candidate = directory.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Spawn one detached process without retaining ownership after successful spawn.
///
/// # Errors
/// Returns the wrapper classification for a stopped, missing, or unlaunchable process.
pub fn spawn_detached_until(
    spec: &CommandSpec,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(), DetachedLaunchError> {
    if deadline.is_some_and(|value| Instant::now() >= value) {
        return Err(DetachedLaunchError::Deadline);
    }
    if cancellation.is_cancelled() {
        return Err(DetachedLaunchError::Interrupted);
    }
    let program = resolve_program(spec.program()).ok_or(DetachedLaunchError::ToolMissing)?;
    let mut command = Command::new(program);
    command
        .args(spec.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = spec.cwd() {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    command.process_group(0);
    command
        .spawn()
        .map(drop)
        .map_err(|_error| DetachedLaunchError::SpawnFailed)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
