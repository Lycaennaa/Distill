use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::process::{CancellationToken, CommandSpec, execute_until};
use crate::status::StatusClass;

use super::products::artifact_failure;
use super::status_from_stop;
use super::wrapper_failure;

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) enum MoveOutcome {
    Published {
        destination: PathBuf,
    },
    PublishedWithFailure {
        destination: PathBuf,
        backup: Option<PathBuf>,
        failure: super::products::Failure,
    },
}

pub(super) fn move_to_applications(
    source: &Path,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<MoveOutcome, super::products::Failure> {
    move_to_directory(source, Path::new("/Applications"), deadline, cancellation)
}

fn move_to_directory(
    source: &Path,
    applications: &Path,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<MoveOutcome, super::products::Failure> {
    check_stop(deadline, cancellation)?;
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|_error| artifact_failure("Xcode app product is missing"))?;
    if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
        return Err(artifact_failure("Xcode app product is not a directory"));
    }
    let applications_metadata = fs::symlink_metadata(applications).map_err(|_error| {
        wrapper_failure(
            StatusClass::PostAction,
            "Applications directory is unavailable",
        )
    })?;
    if !applications_metadata.is_dir() || applications_metadata.file_type().is_symlink() {
        return Err(wrapper_failure(
            StatusClass::PostAction,
            "Applications destination is not a directory",
        ));
    }
    let source = fs::canonicalize(source)
        .map_err(|_error| artifact_failure("Xcode app product path could not be resolved"))?;
    let applications = fs::canonicalize(applications).map_err(|_error| {
        wrapper_failure(
            StatusClass::PostAction,
            "Applications path could not be resolved",
        )
    })?;
    let source_name = source
        .file_name()
        .filter(|name| name.to_string_lossy().ends_with(".app"))
        .ok_or_else(|| artifact_failure("Xcode product path is not an app bundle"))?;
    let destination = applications.join(source_name);
    if source == destination {
        return Ok(MoveOutcome::Published { destination });
    }
    let _lock = destination_lock(&applications, source_name)?;
    let destination_exists = destination_exists(&destination)?;
    let (staging, backup) = unique_paths(&applications, source_name);
    copy_to_staging(&source, &applications, &staging, deadline, cancellation)?;
    publish_staging(
        &source,
        &staging,
        &backup,
        &destination,
        destination_exists,
        deadline,
        cancellation,
    )
}

fn destination_lock(
    applications: &Path,
    source_name: &std::ffi::OsStr,
) -> Result<std::fs::File, super::products::Failure> {
    let lock_path = applications.join(format!(".{}.distill.lock", source_name.to_string_lossy()));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let lock = options.open(lock_path).map_err(|_error| {
        wrapper_failure(
            StatusClass::Artifact,
            "destination move lock is unavailable",
        )
    })?;
    lock.try_lock().map_err(|_error| {
        wrapper_failure(
            StatusClass::Artifact,
            "another move to this destination is active",
        )
    })?;
    Ok(lock)
}

fn destination_exists(destination: &Path) -> Result<bool, super::products::Failure> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_metadata) => Err(artifact_failure(
            "existing Applications destination is not a regular app directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_error) => Err(wrapper_failure(
            StatusClass::PostAction,
            "Applications destination could not be inspected",
        )),
    }
}

fn copy_to_staging(
    source: &Path,
    applications: &Path,
    staging: &Path,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(), super::products::Failure> {
    check_stop(deadline, cancellation)?;
    fs::create_dir(staging).map_err(|_error| {
        wrapper_failure(
            StatusClass::PostAction,
            "temporary app staging path could not be created",
        )
    })?;
    let copy = CommandSpec::new("/usr/bin/ditto")
        .args([
            source.as_os_str().to_owned(),
            staging.as_os_str().to_owned(),
        ])
        .label("copy app bundle")
        .with_status_class(StatusClass::Runtime)
        .current_dir(applications.to_owned());
    let copied = execute_until(&copy, deadline, cancellation).map_err(|_error| {
        cleanup_staging(staging, StatusClass::PostAction, "app bundle copy failed")
    })?;
    let status = copied.report().final_status();
    if !status.is_success() || !copied.capture_complete() {
        let class = match status.class() {
            StatusClass::Timeout | StatusClass::Interrupt => status.class(),
            _ => StatusClass::PostAction,
        };
        let message = format!(
            "app bundle copy failed ({} exit {}; capture_complete={})",
            status.class(),
            status.code(),
            copied.capture_complete()
        );
        return Err(cleanup_staging(staging, class, &message));
    }
    let staged_metadata = fs::symlink_metadata(staging).map_err(|_error| {
        cleanup_staging(
            staging,
            StatusClass::PostAction,
            "staged app bundle is missing",
        )
    })?;
    if !staged_metadata.is_dir() || staged_metadata.file_type().is_symlink() {
        return Err(cleanup_staging(
            staging,
            StatusClass::PostAction,
            "staged app bundle is invalid",
        ));
    }
    check_stop(deadline, cancellation)
        .map_err(|failure| cleanup_staging(staging, failure.class, &failure.message))
}

fn publish_staging(
    source: &Path,
    staging: &Path,
    backup: &Path,
    destination: &Path,
    destination_exists: bool,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<MoveOutcome, super::products::Failure> {
    if destination_exists && let Err(error) = fs::rename(destination, backup) {
        let cleanup_error = remove_any(staging).err();
        let message = cleanup_error.map_or_else(
            || format!("existing app could not be backed up: {error}"),
            |cleanup| format!("existing app could not be backed up: {error}; staging cleanup failed: {cleanup}"),
        );
        return Err(wrapper_failure(StatusClass::PostAction, message));
    }
    if let Err(failure) = check_stop(deadline, cancellation) {
        let restore_error = restore_backup(destination_exists, backup, destination);
        let cleanup_error = remove_any(staging).err();
        return Err(wrapper_failure(
            failure.class,
            rollback_message(failure.class, backup, restore_error, cleanup_error),
        ));
    }
    if let Err(error) = fs::rename(staging, destination) {
        let restore_error = restore_backup(destination_exists, backup, destination);
        let cleanup_error = remove_any(staging).err();
        let message = publish_failure(&error, backup, restore_error, cleanup_error);
        return Err(wrapper_failure(StatusClass::PostAction, message));
    }
    Ok(finish_published(
        source,
        backup,
        destination,
        destination_exists,
        deadline,
        cancellation,
    ))
}

fn finish_published(
    source: &Path,
    backup: &Path,
    destination: &Path,
    destination_exists: bool,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> MoveOutcome {
    if let Err(failure) = check_stop(deadline, cancellation) {
        let backup = destination_exists.then(|| backup.to_owned());
        let message = backup.as_ref().map_or_else(
            || {
                format!(
                    "{}; new app published at {}",
                    failure.message,
                    destination.display()
                )
            },
            |path| format!("{}; backup retained at {}", failure.message, path.display()),
        );
        return MoveOutcome::PublishedWithFailure {
            destination: destination.to_owned(),
            backup,
            failure: wrapper_failure(failure.class, message),
        };
    }
    let backup = if destination_exists {
        match fs::remove_dir_all(backup) {
            Ok(()) => None,
            Err(_error) => Some(backup.to_owned()),
        }
    } else {
        None
    };
    let mut failure = backup.as_ref().map(|path| {
        wrapper_failure(
            StatusClass::PostAction,
            format!(
                "new app published; old app backup retained at {}",
                path.display()
            ),
        )
    });
    if let Err(stopped) = check_stop(deadline, cancellation) {
        failure = Some(wrapper_failure(
            stopped.class,
            format!(
                "{}; new app published at {}",
                stopped.message,
                destination.display()
            ),
        ));
    } else if backup.is_none()
        && let Err(error) = fs::remove_dir_all(source)
    {
        failure = Some(wrapper_failure(
            StatusClass::PostAction,
            format!("new app published; source could not be removed: {error}"),
        ));
    }
    if let Some(class) = status_from_stop(deadline, cancellation) {
        let backup_message = backup.as_deref().map_or_else(String::new, |path| {
            format!("; backup retained at {}", path.display())
        });
        let source_message = if source.exists() {
            format!("; source may remain at {}", source.display())
        } else {
            String::new()
        };
        failure = Some(wrapper_failure(
            class,
            format!(
                "{}; app published at {}{}{}",
                stop_message(class),
                destination.display(),
                backup_message,
                source_message
            ),
        ));
    }
    failure.map_or_else(
        || MoveOutcome::Published {
            destination: destination.to_owned(),
        },
        |failure| MoveOutcome::PublishedWithFailure {
            destination: destination.to_owned(),
            backup,
            failure,
        },
    )
}

fn check_stop(
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<(), super::products::Failure> {
    status_from_stop(deadline, cancellation).map_or(Ok(()), |class| {
        Err(wrapper_failure(class, stop_message(class)))
    })
}

fn cleanup_staging(staging: &Path, class: StatusClass, message: &str) -> super::products::Failure {
    match remove_any(staging) {
        Ok(()) => wrapper_failure(class, message),
        Err(error) => wrapper_failure(
            class,
            format!(
                "{message}; staging path retained at {}: {error}",
                staging.display()
            ),
        ),
    }
}

fn publish_failure(
    error: &std::io::Error,
    backup: &Path,
    restore_error: Option<std::io::Error>,
    cleanup_error: Option<std::io::Error>,
) -> String {
    match (restore_error, cleanup_error) {
        (Some(restore), _) => format!(
            "new app could not be published ({error}); old app retained at {}: {restore}",
            backup.display()
        ),
        (None, Some(cleanup)) => {
            format!("new app could not be published: {error}; staging cleanup failed: {cleanup}")
        }
        (None, None) => format!("new app could not be published: {error}"),
    }
}

fn unique_paths(applications: &Path, source_name: &std::ffi::OsStr) -> (PathBuf, PathBuf) {
    let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
    let prefix = format!(".distill-{}-{id}-", std::process::id());
    (
        applications.join(format!("{prefix}stage-{}", source_name.to_string_lossy())),
        applications.join(format!("{prefix}backup-{}", source_name.to_string_lossy())),
    )
}

fn restore_backup(
    destination_state: bool,
    backup: &Path,
    destination: &Path,
) -> Option<std::io::Error> {
    if destination_state {
        fs::rename(backup, destination).err()
    } else {
        None
    }
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_metadata) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rollback_message(
    class: StatusClass,
    backup: &Path,
    restore_error: Option<std::io::Error>,
    cleanup_error: Option<std::io::Error>,
) -> String {
    match (restore_error, cleanup_error) {
        (Some(error), _) => format!(
            "{}; old app backup retained at {}: {error}",
            stop_message(class),
            backup.display()
        ),
        (None, Some(error)) => format!("{}; staging cleanup failed: {error}", stop_message(class)),
        (None, None) => stop_message(class).to_owned(),
    }
}

const fn stop_message(class: StatusClass) -> &'static str {
    match class {
        StatusClass::Timeout => "operation deadline exceeded",
        StatusClass::Interrupt => "operation interrupted",
        _ => "app move stopped",
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(
    clippy::panic_in_result_fn,
    reason = "test assertions are the intended failure mechanism"
)]
mod tests {
    use std::error::Error;
    use std::ffi::OsStr;

    use super::*;
    use crate::process::CancellationToken;

    #[test]
    fn replacement_publishes_new_app_and_removes_source() -> Result<(), Box<dyn Error>> {
        let root = test_root("replace")?;
        let source = root.join("build/Demo.app");
        let applications = root.join("Applications");
        let destination = applications.join("Demo.app");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&destination)?;
        fs::write(source.join("new.txt"), "new")?;
        fs::write(destination.join("old.txt"), "old")?;

        let moved = move_to_directory(&source, &applications, None, &CancellationToken::new())
            .map_err(|failure| std::io::Error::other(failure.message))?;

        assert!(matches!(moved, MoveOutcome::Published { .. }));
        assert_eq!(fs::read_to_string(destination.join("new.txt"))?, "new");
        assert!(!destination.join("old.txt").exists());
        assert!(!source.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn overlapping_destination_move_fails_without_touching_apps() -> Result<(), Box<dyn Error>> {
        let root = test_root("locked")?;
        let source = root.join("build/Demo.app");
        let applications = root.join("Applications");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&applications)?;
        fs::write(source.join("new.txt"), "new")?;
        let lock = destination_lock(&applications, OsStr::new("Demo.app"))
            .map_err(|failure| std::io::Error::other(failure.message))?;

        let failure = move_to_directory(&source, &applications, None, &CancellationToken::new())
            .expect_err("overlapping transaction must fail");
        assert_eq!(failure.class, StatusClass::Artifact);
        assert!(source.join("new.txt").is_file());
        assert!(!applications.join("Demo.app").exists());
        drop(lock);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cancellation_before_publish_preserves_source_and_existing_app() -> Result<(), Box<dyn Error>>
    {
        let root = test_root("cancel")?;
        let source = root.join("build/Demo.app");
        let applications = root.join("Applications");
        let destination = applications.join("Demo.app");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&destination)?;
        fs::write(source.join("new.txt"), "new")?;
        fs::write(destination.join("old.txt"), "old")?;
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let failure = move_to_directory(&source, &applications, None, &cancellation)
            .expect_err("cancelled move must stop before publishing");
        assert_eq!(failure.class, StatusClass::Interrupt);
        assert!(source.join("new.txt").is_file());
        assert_eq!(fs::read_to_string(destination.join("old.txt"))?, "old");
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_staged_publish_restores_the_previous_app() -> Result<(), Box<dyn Error>> {
        let root = test_root("rollback")?;
        let source = root.join("build/Demo.app");
        let applications = root.join("Applications");
        let destination = applications.join("Demo.app");
        let staging = applications.join("missing-stage.app");
        let backup = applications.join("backup.app");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&destination)?;
        fs::write(destination.join("old.txt"), "old")?;

        let failure = publish_staging(
            &source,
            &staging,
            &backup,
            &destination,
            true,
            None,
            &CancellationToken::new(),
        )
        .expect_err("missing staged bundle must fail publication");
        assert_eq!(failure.class, StatusClass::PostAction);
        assert_eq!(fs::read_to_string(destination.join("old.txt"))?, "old");
        assert!(!backup.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    fn test_root(name: &str) -> std::io::Result<PathBuf> {
        let root = PathBuf::from("target").join(format!(
            "distill-post-action-move-{name}-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        fs::create_dir_all(&root)?;
        Ok(root)
    }
}
