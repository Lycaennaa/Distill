#![allow(
    unsafe_code,
    reason = "Windows storage uses official windows-rs handle APIs"
)]

use std::fs::{self, File};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::AtomicU64;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ACCESS_RIGHTS, FILE_ADD_FILE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_CREATION_DISPOSITION, FILE_DISPOSITION_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FileAttributeTagInfo,
    FileDispositionInfo, FileIdInfo, GetFileInformationByHandleEx, GetFileType,
    GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL, SetFileInformationByHandle, UnlockFile,
    VOLUME_NAME_GUID,
};
use windows::core::PCWSTR;

use crate::acl::{
    PrivateWindowsSecurity, validate_windows_ancestor_acl, validate_windows_parent_acl,
    validate_windows_private_acl,
};

use super::LogError;
#[path = "storage_windows_workspace.rs"]
mod workspace;
pub(super) use workspace::TempWorkspace;

const TEMP_ATTEMPTS: usize = 128;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct ParentDirectory {
    path: PathBuf,
    handle: File,
    ancestors: Vec<File>,
}

impl ParentDirectory {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn all_handles(&self) -> impl Iterator<Item = &File> {
        self.ancestors.iter().chain(std::iter::once(&self.handle))
    }
}

#[derive(Debug)]
pub(super) struct ReservationLock {
    _file: File,
}

pub(super) fn open_directory(path: &Path) -> Result<ParentDirectory, LogError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| LogError::new(format!("cannot resolve log directory: {error}")))?
            .join(path)
    };
    if !absolute.is_absolute() {
        return Err(LogError::new(
            "log directory must be an absolute local path",
        ));
    }

    let mut prefix = None;
    let mut current = None;
    let mut handles = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(value) => {
                if !matches!(value.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
                    return Err(LogError::new("log directory must be on a local disk"));
                }
                let mut root = value.as_os_str().to_os_string();
                root.push("\\");
                let root = PathBuf::from(root);
                current = Some(root);
                prefix = Some(value.as_os_str().to_os_string());
            }
            Component::RootDir => {
                if prefix.is_none() || !handles.is_empty() {
                    return Err(LogError::new("log directory has an unsupported root"));
                }
                let root = current
                    .as_ref()
                    .ok_or_else(|| LogError::new("log directory has no drive prefix"))?;
                handles.push(open_directory_handle(root).map_err(|error| {
                    LogError::new(format!("cannot open log directory root: {error}"))
                })?);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if handles.len() > 1 {
                    drop(handles.pop());
                    if let Some(current) = current.as_mut() {
                        current.pop();
                    }
                }
            }
            Component::Normal(name) => {
                if handles.is_empty() {
                    return Err(LogError::new("log directory has an unsupported root"));
                }
                let current = current
                    .as_mut()
                    .ok_or_else(|| LogError::new("log directory has no drive prefix"))?;
                current.push(name);
                let handle = open_directory_handle(current).map_err(|error| {
                    LogError::new(format!("cannot safely open log directory: {error}"))
                })?;
                handles.push(handle);
            }
        }
    }
    if handles.is_empty() {
        return Err(LogError::new("log directory has an unsupported root"));
    }
    let path = current.ok_or_else(|| LogError::new("log directory has no drive prefix"))?;
    let previous_handle = handles
        .pop()
        .ok_or_else(|| LogError::new("cannot open log directory"))?;
    let handle = open_handle(
        &path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | FILE_ADD_FILE,
        directory_share_mode(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
        None,
    )
    .map_err(|error| LogError::new(format!("cannot open log directory for writing: {error}")))?;
    ensure_directory_handle(&handle)?;
    if file_id(&handle)? != file_id(&previous_handle)? {
        return Err(LogError::new(
            "log directory changed while opening it for writing",
        ));
    }
    drop(previous_handle);
    Ok(ParentDirectory {
        path,
        handle,
        ancestors: handles,
    })
}

pub(super) fn validate_parent_ancestry(
    _path: &Path,
    parent: &ParentDirectory,
) -> Result<(), LogError> {
    let ancestor_count = parent.ancestors.len();
    for (index, directory) in parent.all_handles().enumerate() {
        ensure_directory_handle(directory)?;
        let validation = if index < ancestor_count {
            validate_windows_ancestor_acl(directory)
        } else {
            validate_windows_parent_acl(directory)
        };
        validation
            .map_err(|error| LogError::new(format!("log directory has an unsafe ACL: {error}")))?;
    }
    Ok(())
}

pub(super) fn reserve_lock(
    parent: &ParentDirectory,
    destination_name: &str,
) -> Result<ReservationLock, LogError> {
    validate_destination_name(destination_name)?;
    let normalized_name = destination_name.to_lowercase();
    let name = format!(".{normalized_name}.distill.lock");
    let path = parent.path.join(&name);
    let mut security = PrivateWindowsSecurity::new()
        .map_err(|error| LogError::new(format!("cannot create log ACL: {error}")))?;
    let attributes = security
        .security_attributes()
        .map_err(|error| LogError::new(format!("cannot create log ACL: {error}")))?;
    let created = open_handle(
        &path,
        private_file_access(),
        FILE_SHARE_MODE(0),
        CREATE_NEW,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
        Some(&attributes),
    );
    let file = match created {
        Ok(file) => file,
        Err(error) if is_already_exists(&error) => open_handle(
            &path,
            private_file_access(),
            FILE_SHARE_MODE(0),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(|error| LogError::new(format!("cannot reserve log destination: {error}")))?,
        Err(error) => {
            return Err(LogError::new(format!(
                "cannot reserve log destination: {error}"
            )));
        }
    };
    ensure_regular_file(&file)?;
    validate_windows_private_acl(&file)
        .map_err(|error| LogError::new(format!("log reservation ACL is unsafe: {error}")))?;
    Ok(ReservationLock { _file: file })
}

pub(super) fn cleanup_incomplete(
    parent_path: &Path,
    parent: &ParentDirectory,
    destination_name: &str,
) -> Result<(), LogError> {
    let entries = fs::read_dir(parent_path)
        .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| LogError::new(format!("cannot inspect log directory: {error}")))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_stage_for(&name, destination_name) {
            remove_stale_workspace(parent, &name)?;
        }
    }
    Ok(())
}

pub(super) fn ensure_absent(
    parent: &ParentDirectory,
    destination_name: &str,
) -> Result<(), LogError> {
    let destination = parent.path.join(destination_name);
    match open_handle(
        &destination,
        FILE_READ_ATTRIBUTES | READ_CONTROL,
        directory_share_mode(),
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
        None,
    ) {
        Ok(_) => Err(LogError::new(
            "log destination already exists or is an unsafe reparse point",
        )),
        Err(error) if is_not_found(&error) => Ok(()),
        Err(error) => Err(LogError::new(format!(
            "cannot inspect log destination: {error}"
        ))),
    }
}

fn open_directory_handle(path: &Path) -> io::Result<File> {
    let file = open_handle(
        path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL,
        directory_share_mode(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        None,
    )?;
    ensure_directory_handle(&file).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(file)
}

fn ensure_directory_handle(file: &File) -> Result<(), LogError> {
    let attributes = file_attributes(file).map_err(|error| LogError::new(error.to_string()))?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(LogError::new("log directory is a symlink or reparse point"));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
        return Err(LogError::new("log parent is not a directory"));
    }
    Ok(())
}

fn ensure_regular_file(file: &File) -> Result<(), LogError> {
    let attributes = file_attributes(file).map_err(|error| LogError::new(error.to_string()))?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(LogError::new("log file is a symlink or reparse point"));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        return Err(LogError::new("log path is not a regular file"));
    }
    // SAFETY: `file` owns a valid handle kept open for the call.
    let file_type = unsafe { GetFileType(HANDLE(file.as_raw_handle())) };
    if file_type.0 != FILE_TYPE_DISK.0 {
        return Err(LogError::new("log path is not a regular disk file"));
    }
    Ok(())
}

fn file_attributes(file: &File) -> io::Result<u32> {
    let mut info = std::mem::MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::zeroed();
    // SAFETY: the official structure is writable and the handle remains open for the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileAttributeTagInfo,
            info.as_mut_ptr().cast(),
            u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                .map_err(|_| io::Error::other("Windows structure size exceeds u32"))?,
        )
    }
    .map_err(|error| windows_error(&error))?;
    // SAFETY: GetFileInformationByHandleEx initialized the requested structure.
    Ok(unsafe { info.assume_init() }.FileAttributes)
}

fn file_id(file: &File) -> Result<FILE_ID_INFO, LogError> {
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::zeroed();
    // SAFETY: the official structure is writable and the handle remains open for the call.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>())
                .map_err(|_| LogError::new("Windows structure size exceeds u32"))?,
        )
    }
    .map_err(|error| LogError::new(format!("cannot inspect log file identity: {error}")))?;
    // SAFETY: GetFileInformationByHandleEx initialized the requested structure.
    Ok(unsafe { info.assume_init() })
}

fn private_file_access() -> FILE_ACCESS_RIGHTS {
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | READ_CONTROL
}

fn directory_share_mode() -> FILE_SHARE_MODE {
    FILE_SHARE_READ | FILE_SHARE_WRITE
}

fn open_handle(
    path: &Path,
    access: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
    disposition: FILE_CREATION_DISPOSITION,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
    security: Option<&windows::Win32::Security::SECURITY_ATTRIBUTES>,
) -> io::Result<File> {
    let file = open_handle_unchecked(path, access, share, disposition, flags, security)?;
    if let Err(locality_error) = ensure_local_volume_handle(&file) {
        if disposition == CREATE_NEW
            && let Err(cleanup_error) = mark_for_delete(&file)
        {
            return Err(io::Error::other(format!(
                "cannot remove newly created non-local log file ({locality_error}): {cleanup_error}"
            )));
        }
        return Err(locality_error);
    }
    Ok(file)
}

fn open_handle_unchecked(
    path: &Path,
    access: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
    disposition: FILE_CREATION_DISPOSITION,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
    security: Option<&windows::Win32::Security::SECURITY_ATTRIBUTES>,
) -> io::Result<File> {
    let wide = wide_path(path);
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            access.0,
            share,
            security.map(std::ptr::from_ref),
            disposition,
            flags,
            None,
        )
    }
    .map_err(|error| windows_error(&error))?;
    // SAFETY: CreateFileW returned a valid owned file handle.
    Ok(unsafe { File::from_raw_handle(handle.0) })
}
fn ensure_local_volume_handle(file: &File) -> io::Result<()> {
    const INITIAL_BUFFER_SIZE: usize = 512;
    const MAX_BUFFER_SIZE: usize = 32_768;

    let mut path = vec![0_u16; INITIAL_BUFFER_SIZE];
    loop {
        // SAFETY: the file handle is live and `path` is writable for its entire length.
        let length = unsafe {
            GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut path, VOLUME_NAME_GUID)
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let length = usize::try_from(length).map_err(io::Error::other)?;
        if length < path.len() {
            let Some(final_path) = path.get(..length) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cannot read the final log volume path",
                ));
            };
            if is_local_volume_path(final_path) {
                return Ok(());
            }
            return Err(io::Error::other(
                "log handle does not resolve to a local volume",
            ));
        }
        let capacity = length
            .checked_add(1)
            .filter(|capacity| *capacity <= MAX_BUFFER_SIZE)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "final log volume path exceeds the Windows path limit",
                )
            })?;
        path.resize(capacity, 0);
    }
}

fn create_directory(
    path: &Path,
    security: &windows::Win32::Security::SECURITY_ATTRIBUTES,
) -> io::Result<()> {
    let wide = wide_path(path);
    // SAFETY: the path and security descriptor remain alive through the call.
    unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(std::ptr::from_ref(security))) }
        .map_err(|error| windows_error(&error))
}

fn remove_stale_workspace(parent: &ParentDirectory, name: &str) -> Result<(), LogError> {
    let path = parent.path.join(name);
    let directory = open_directory_handle_for_cleanup(&path)?;
    validate_windows_private_acl(&directory)
        .map_err(|error| LogError::new(format!("stale workspace ACL is unsafe: {error}")))?;
    for file_name in ["capture.tmp", "publish.tmp"] {
        remove_temp_file(&path.join(file_name))
            .map_err(|error| LogError::new(format!("cannot remove stale log file: {error}")))?;
    }
    mark_for_delete(&directory)
        .map_err(|error| LogError::new(format!("cannot remove stale workspace: {error}")))?;
    drop(directory);
    Ok(())
}

fn open_directory_handle_for_cleanup(path: &Path) -> Result<File, LogError> {
    let file = open_handle(
        path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
        directory_share_mode(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        None,
    )
    .map_err(|error| LogError::new(format!("cannot open stale workspace safely: {error}")))?;
    ensure_directory_handle(&file)?;
    Ok(file)
}

fn remove_temp_file(path: &Path) -> io::Result<()> {
    let file = match open_handle(
        path,
        DELETE | FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
        None,
    ) {
        Ok(file) => file,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let attributes = file_attributes(&file)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "temporary log path is a reparse point",
        ));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "temporary log path is not a regular file",
        ));
    }
    validate_windows_private_acl(&file)?;
    mark_for_delete(&file)?;
    drop(file);
    Ok(())
}

fn mark_for_delete(file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `info` has the documented layout and the handle has DELETE access.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            std::ptr::from_ref(&info).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .map_err(io::Error::other)?,
        )
    }
    .map_err(|error| windows_error(&error))
}

fn is_stage_for(name: &str, destination_name: &str) -> bool {
    let prefix = format!(".{}.distill-stage-", destination_name.to_lowercase());
    let Some(suffix) = name.strip_prefix(&prefix) else {
        return false;
    };
    let Some(id) = suffix.strip_suffix(".tmp") else {
        return false;
    };
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn windows_error(error: &windows::core::Error) -> io::Error {
    let hresult = error.code().0;
    // HRESULT is signed; preserve its 32-bit pattern for the Win32 code check.
    let raw = u32::from_ne_bytes(hresult.to_ne_bytes());
    let code = if raw & 0xffff_0000 == 0x8007_0000 {
        // The mask bounds this status code to the nonnegative 16-bit range.
        i32::from_ne_bytes((raw & 0xffff).to_ne_bytes())
    } else {
        hresult
    };
    io::Error::from_raw_os_error(code)
}

fn is_already_exists(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(80 | 183))
}

fn is_not_found(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(2 | 3))
}
fn open_workspace_directory_handle(path: &Path) -> Result<File, LogError> {
    let directory = open_handle_unchecked(
        path,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
        directory_share_mode(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        None,
    )
    .map_err(|error| LogError::new(format!("cannot open temporary workspace: {error}")))?;
    if let Err(locality_error) = ensure_local_volume_handle(&directory) {
        if let Err(cleanup_error) = mark_for_delete(&directory) {
            return Err(LogError::new(format!(
                "cannot remove non-local temporary workspace ({locality_error}): {cleanup_error}"
            )));
        }
        return Err(LogError::new(format!(
            "temporary workspace is not on a local volume: {locality_error}"
        )));
    }
    ensure_directory_handle(&directory)?;
    Ok(directory)
}
fn validate_destination_name(name: &str) -> Result<(), LogError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with('.')
        || name.ends_with(' ')
        || name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err(LogError::new(
            "log filename is not a safe Windows path component",
        ));
    }
    let base = name.split('.').next().map_or("", |component| component);
    let named_device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|device| base.eq_ignore_ascii_case(device));
    let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
        let bytes = base.as_bytes();
        bytes.len() == 4
            && bytes
                .get(0..3)
                .is_some_and(|device| device.eq_ignore_ascii_case(prefix.as_bytes()))
            && bytes
                .get(3)
                .is_some_and(|digit| (b'1'..=b'9').contains(digit))
    });
    if named_device || numbered_device {
        return Err(LogError::new(
            "log filename names a reserved Windows device",
        ));
    }
    Ok(())
}
fn release_file_lock(file: &mut Option<File>) -> io::Result<()> {
    if let Some(handle) = file.as_ref() {
        // SAFETY: this handle owns the byte-range lock created by LockFile above.
        unsafe { UnlockFile(HANDLE(handle.as_raw_handle()), 0, 0, u32::MAX, 0x7fff_ffff) }
            .map_err(|error| windows_error(&error))?;
    }
    drop(file.take());
    Ok(())
}

fn is_local_volume_path(path: &[u16]) -> bool {
    const VOLUME_GUID_PREFIX: &[u8] = br"\\?\Volume{";
    path.get(..VOLUME_GUID_PREFIX.len()).is_some_and(|prefix| {
        prefix
            .iter()
            .zip(VOLUME_GUID_PREFIX)
            .all(|(unit, byte)| *unit == u16::from(*byte))
    })
}

#[cfg(test)]
mod tests {
    use super::is_local_volume_path;

    #[test]
    fn local_volume_policy_rejects_network_paths() {
        let local = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\logs"
            .encode_utf16()
            .collect::<Vec<_>>();
        let network = r"\\?\UNC\server\share\logs"
            .encode_utf16()
            .collect::<Vec<_>>();

        assert!(is_local_volume_path(&local));
        assert!(!is_local_volume_path(&network));
    }
}
