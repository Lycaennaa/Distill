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
        return resolve_candidate(path);
    }
    let has_path_prefix = path.components().any(|component| {
        matches!(
            component,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::CurDir
                | std::path::Component::ParentDir
        )
    });
    #[cfg(windows)]
    let has_path_separator = program.to_string_lossy().contains(['/', '\\']);
    #[cfg(not(windows))]
    let has_path_separator = program.to_string_lossy().contains('/');
    if has_path_prefix || has_path_separator {
        return resolve_candidate(&process_cwd.join(path));
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
        if let Some(candidate) = resolve_candidate(&directory.join(program)) {
            return Some(candidate);
        }
    }
    None
}
#[cfg(windows)]
fn resolve_candidate(path: &Path) -> Option<PathBuf> {
    if let Some(extension) = path.extension() {
        let extension = extension.to_string_lossy();
        return if extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com") {
            is_executable(path).then(|| path.to_path_buf())
        } else {
            None
        };
    }
    for extension in ["exe", "com"] {
        let candidate = path.with_extension(extension);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn resolve_candidate(path: &Path) -> Option<PathBuf> {
    is_executable(path).then(|| path.to_path_buf())
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
    for (key, value) in spec.environment() {
        command.env(key, value);
    }
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

#[cfg(all(test, windows))]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::resolve_program;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn resolves_executables_without_extension_but_never_batch_scripts() {
        let directory = std::env::temp_dir().join(format!(
            "distill-resolver-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = || -> std::io::Result<()> {
            fs::create_dir(&directory)?;
            let executable = directory.join("tool.exe");
            fs::write(&executable, b"test executable")?;
            let script = directory.join("script.cmd");
            fs::write(&script, b"echo not launched")?;

            assert_eq!(
                resolve_program(directory.join("tool").as_os_str()),
                Some(executable)
            );
            assert_eq!(resolve_program(script.as_os_str()), None);
            fs::remove_dir_all(&directory)
        };
        result().expect("exercise Windows executable resolution");
    }
}
