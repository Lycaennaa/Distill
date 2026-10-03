use std::fs::{self, File};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::LogError;

const TEMP_ATTEMPTS: u16 = 128;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(super) fn open_directory(path: &Path) -> Result<File, LogError> {
    let descriptor = nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_DIRECTORY | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|error| LogError::new(format!("cannot open log directory: {error}")))?;
    Ok(File::from(descriptor))
}
pub(super) fn validate_parent(directory: &File) -> Result<(), LogError> {
    let metadata = directory
        .metadata()
        .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
    let mode = metadata.mode();
    let owner_is_trusted = metadata.uid() == nix::unistd::geteuid().as_raw() || metadata.uid() == 0;
    let shared_write_is_protected = mode & 0o022 == 0 || mode & 0o1000 != 0;
    if !metadata.is_dir() || !owner_is_trusted || !shared_write_is_protected {
        return Err(LogError::new(
            "log directory is unsafe for exclusive publication",
        ));
    }
    if crate::acl::has_extended_acl(directory)
        .map_err(|error| LogError::new(format!("cannot inspect log directory ACL: {error}")))?
    {
        return Err(LogError::new("log directory has an extended ACL"));
    }
    Ok(())
}
pub(super) fn validate_parent_ancestry(parent_path: &Path, parent: &File) -> Result<(), LogError> {
    let canonical_parent = fs::canonicalize(parent_path)
        .map_err(|error| LogError::new(format!("cannot resolve log directory: {error}")))?;
    let path_metadata = fs::metadata(&canonical_parent)
        .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
    let parent_metadata = parent
        .metadata()
        .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
    if !same_file(&path_metadata, &parent_metadata) {
        return Err(LogError::new("log directory changed during validation"));
    }

    let mut current = canonical_parent.as_path();
    loop {
        let directory = if current == canonical_parent.as_path() {
            parent
                .try_clone()
                .map_err(|error| LogError::new(format!("cannot retain log directory: {error}")))?
        } else {
            let directory = open_directory(current)?;
            let path_metadata = fs::metadata(current)
                .map_err(|error| LogError::new(format!("cannot inspect log ancestor: {error}")))?;
            let directory_metadata = directory
                .metadata()
                .map_err(|error| LogError::new(format!("cannot inspect log ancestor: {error}")))?;
            if !same_file(&path_metadata, &directory_metadata) {
                return Err(LogError::new("log ancestor changed during validation"));
            }
            directory
        };
        validate_parent(&directory)?;
        if current == Path::new("/") {
            return Ok(());
        }
        current = current
            .parent()
            .ok_or_else(|| LogError::new("log directory ancestry is invalid"))?;
    }
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

pub(super) fn reserve_lock(
    parent: &File,
    destination_name: &str,
) -> Result<nix::fcntl::Flock<File>, LogError> {
    let lock_name = format!(".{destination_name}.distill.lock");
    let descriptor = nix::fcntl::openat(
        parent.as_fd(),
        lock_name.as_str(),
        nix::fcntl::OFlag::O_CREAT
            | nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_NONBLOCK
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .map_err(|error| LogError::new(format!("cannot open log reservation: {error}")))?;
    let lock = File::from(descriptor);
    let metadata = lock
        .metadata()
        .map_err(|error| LogError::new(format!("cannot inspect log reservation: {error}")))?;
    if !metadata.is_file() {
        return Err(LogError::new("log reservation is not a regular file"));
    }
    if crate::acl::has_extended_acl(&lock)
        .map_err(|error| LogError::new(format!("cannot inspect reservation ACL: {error}")))?
    {
        return Err(LogError::new("log reservation has an extended ACL"));
    }
    nix::sys::stat::fchmod(
        lock.as_fd(),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .map_err(|error| LogError::new(format!("cannot secure log reservation: {error}")))?;
    nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
        |(_file, error)| LogError::new(format!("log destination is already reserved: {error}")),
    )
}

#[derive(Debug)]
pub(super) struct TempWorkspace {
    parent: File,
    directory: File,
    name: String,
    active: bool,
}

impl TempWorkspace {
    pub(super) fn create(parent: &File, destination_name: &str) -> Result<Self, LogError> {
        let parent = parent
            .try_clone()
            .map_err(|error| LogError::new(format!("cannot retain log directory: {error}")))?;
        for _attempt in 0..TEMP_ATTEMPTS {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let name = format!(".{destination_name}.distill-stage-{id}.tmp");
            match nix::sys::stat::mkdirat(
                parent.as_fd(),
                name.as_str(),
                nix::sys::stat::Mode::S_IRWXU,
            ) {
                Ok(()) => {
                    let directory = open_stage_directory(&parent, &name)?;
                    return Ok(Self {
                        parent,
                        directory,
                        name,
                        active: true,
                    });
                }
                Err(nix::errno::Errno::EEXIST) => {}
                Err(error) => {
                    return Err(LogError::new(format!(
                        "cannot create private log workspace: {error}"
                    )));
                }
            }
        }
        Err(LogError::new(
            "cannot allocate a unique temporary log workspace",
        ))
    }

    fn open_existing(parent: &File, name: &str) -> Result<Self, LogError> {
        Ok(Self {
            parent: parent
                .try_clone()
                .map_err(|error| LogError::new(format!("cannot retain log directory: {error}")))?,
            directory: open_stage_directory(parent, name)?,
            name: name.to_owned(),
            active: true,
        })
    }
    #[cfg(test)]
    pub(super) fn directory_name_for_test(&self) -> &str {
        &self.name
    }

    pub(super) fn create_capture(&self) -> Result<File, LogError> {
        self.create_file("capture.tmp")
    }

    pub(super) fn create_publish(&self) -> Result<File, LogError> {
        self.create_file("publish.tmp")
    }

    fn create_file(&self, name: &str) -> Result<File, LogError> {
        let descriptor = nix::fcntl::openat(
            self.directory.as_fd(),
            name,
            nix::fcntl::OFlag::O_CREAT
                | nix::fcntl::OFlag::O_EXCL
                | nix::fcntl::OFlag::O_RDWR
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .map_err(|error| LogError::new(format!("cannot create temporary log: {error}")))?;
        let file = File::from(descriptor);
        nix::sys::stat::fchmod(
            file.as_fd(),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .map_err(|error| LogError::new(format!("cannot secure temporary log: {error}")))?;
        if crate::acl::has_extended_acl(&file)
            .map_err(|error| LogError::new(format!("cannot inspect temporary log ACL: {error}")))?
        {
            return Err(LogError::new("temporary log has an extended ACL"));
        }
        Ok(file)
    }

    pub(super) fn publish(&self, destination_name: &str) -> Result<(), String> {
        nix::unistd::linkat(
            self.directory.as_fd(),
            "publish.tmp",
            self.parent.as_fd(),
            destination_name,
            nix::fcntl::AtFlags::empty(),
        )
        .map_err(|error| error.to_string())
    }

    pub(super) fn cleanup(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }
        let mut errors = Vec::new();
        for name in ["capture.tmp", "publish.tmp"] {
            if let Err(error) = unlink_entry(
                &self.directory,
                name,
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            ) {
                errors.push(error);
            }
        }
        match unlink_entry(
            &self.parent,
            &self.name,
            nix::unistd::UnlinkatFlags::RemoveDir,
        ) {
            Ok(()) => self.active = false,
            Err(error) => errors.push(error),
        }
        join_errors(&errors)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        drop(self.cleanup());
    }
}

fn open_stage_directory(parent: &File, name: &str) -> Result<File, LogError> {
    let descriptor = nix::fcntl::openat(
        parent.as_fd(),
        name,
        nix::fcntl::OFlag::O_RDONLY
            | nix::fcntl::OFlag::O_DIRECTORY
            | nix::fcntl::OFlag::O_NOFOLLOW
            | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|error| LogError::new(format!("cannot open private log workspace: {error}")))?;
    let directory = File::from(descriptor);
    let metadata = directory
        .metadata()
        .map_err(|error| LogError::new(format!("cannot inspect private log workspace: {error}")))?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(LogError::new("log workspace is not a private directory"));
    }
    if crate::acl::has_extended_acl(&directory)
        .map_err(|error| LogError::new(format!("cannot inspect workspace ACL: {error}")))?
    {
        return Err(LogError::new("log workspace has an extended ACL"));
    }
    nix::sys::stat::fchmod(directory.as_fd(), nix::sys::stat::Mode::S_IRWXU)
        .map_err(|error| LogError::new(format!("cannot secure private log workspace: {error}")))?;
    let secured_mode = directory
        .metadata()
        .map_err(|error| LogError::new(format!("cannot inspect private log workspace: {error}")))?
        .mode();
    if secured_mode & 0o777 != 0o700 {
        return Err(LogError::new("log workspace permissions are unsafe"));
    }
    Ok(directory)
}

fn unlink_entry(
    directory: &File,
    name: &str,
    flags: nix::unistd::UnlinkatFlags,
) -> Result<(), String> {
    match nix::unistd::unlinkat(directory.as_fd(), name, flags) {
        Ok(()) | Err(nix::errno::Errno::ENOENT) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn cleanup_incomplete(
    parent_path: &Path,
    parent: &File,
    destination_name: &str,
) -> Result<(), LogError> {
    let entries = fs::read_dir(parent_path)
        .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !is_stage_for(file_name, destination_name) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            LogError::new(format!("cannot inspect temporary workspace: {error}"))
        })?;
        if metadata.is_dir() {
            let mut workspace = TempWorkspace::open_existing(parent, file_name)?;
            if let Some(error) = workspace.cleanup() {
                return Err(LogError::new(format!(
                    "cannot clean temporary workspace: {error}"
                )));
            }
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            unlink_entry(parent, file_name, nix::unistd::UnlinkatFlags::NoRemoveDir).map_err(
                |error| LogError::new(format!("cannot clean temporary workspace: {error}")),
            )?;
        }
    }
    Ok(())
}

pub(super) fn ensure_absent(path: &Path) -> Result<(), LogError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let kind = if metadata.file_type().is_symlink() {
                "symlink"
            } else if metadata.is_file() {
                "existing file"
            } else {
                "non-regular path"
            };
            Err(LogError::new(format!("log destination is unsafe: {kind}")))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LogError::new(format!(
            "cannot inspect log destination: {error}"
        ))),
    }
}

fn is_stage_for(file_name: &str, destination_name: &str) -> bool {
    let prefix = format!(".{destination_name}.distill-stage-");
    let Some(suffix) = file_name.strip_prefix(&prefix) else {
        return false;
    };
    let Some(id) = suffix.strip_suffix(".tmp") else {
        return false;
    };
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn join_errors(errors: &[String]) -> Option<String> {
    if errors.is_empty() {
        None
    } else {
        Some(errors.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_stage_for, open_directory, validate_parent, validate_parent_ancestry};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn temp_cleanup_names_match_only_exact_destinations() {
        assert!(is_stage_for(".foo.distill-stage-1.tmp", "foo"));
        assert!(!is_stage_for(".foo.distill-other-stage-1.tmp", "foo"));
        assert!(!is_stage_for(".foobar.distill-stage-1.tmp", "foo"));
        assert!(!is_stage_for(".foo.distill-stage-x.tmp", "foo"));
    }
    #[test]
    fn shared_writable_parent_requires_sticky_protection() {
        let path =
            std::env::temp_dir().join(format!("distill-parent-policy-{}", std::process::id()));
        drop(fs::remove_dir_all(&path));
        fs::create_dir(&path).expect("create test directory");
        let directory = open_directory(&path).expect("open test directory");
        let mut permissions = fs::metadata(&path)
            .expect("inspect test directory")
            .permissions();

        permissions.set_mode(0o777);
        fs::set_permissions(&path, permissions.clone()).expect("make directory shared writable");
        assert!(validate_parent(&directory).is_err());

        permissions.set_mode(0o1777);
        fs::set_permissions(&path, permissions).expect("protect shared directory entries");
        assert!(validate_parent(&directory).is_ok());

        drop(directory);
        fs::remove_dir_all(path).expect("remove test directory");
    }
    #[test]
    fn unsafe_ancestor_invalidates_destination_parent() {
        let ancestor =
            std::env::temp_dir().join(format!("distill-ancestor-policy-{}", std::process::id()));
        let parent = ancestor.join("nested");
        drop(fs::remove_dir_all(&ancestor));
        fs::create_dir_all(&parent).expect("create nested test directory");
        let mut permissions = fs::metadata(&ancestor)
            .expect("inspect ancestor")
            .permissions();
        permissions.set_mode(0o777);
        fs::set_permissions(&ancestor, permissions).expect("make ancestor unsafe");

        let directory = open_directory(&parent).expect("open destination parent");
        let validation = validate_parent_ancestry(&parent, &directory);
        drop(directory);

        let mut permissions = fs::metadata(&ancestor)
            .expect("inspect ancestor")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&ancestor, permissions).expect("restore ancestor permissions");
        fs::remove_dir_all(&ancestor).expect("remove nested test directory");
        assert!(validation.is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn acl_granted_directory_writes_are_rejected() {
        let path = std::env::temp_dir().join(format!("distill-acl-policy-{}", std::process::id()));
        drop(fs::remove_dir_all(&path));
        fs::create_dir(&path).expect("create ACL test directory");
        let mut permissions = fs::metadata(&path)
            .expect("inspect ACL test directory")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("restrict ACL test directory mode");
        let acl_status = std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow add_file,search,add_subdirectory,delete_child",
            ])
            .arg(&path)
            .status()
            .expect("run ACL setup");
        assert!(acl_status.success());

        let directory = open_directory(&path).expect("open ACL test directory");
        let acl_present = crate::acl::has_extended_acl(&directory).expect("inspect ACL");
        let validation = validate_parent(&directory);
        drop(directory);
        let clear_status = std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(&path)
            .status()
            .expect("remove ACL from test directory");
        assert!(clear_status.success());
        fs::remove_dir_all(path).expect("remove ACL test directory");

        assert!(acl_present);
        assert!(validation.is_err());
    }
}
