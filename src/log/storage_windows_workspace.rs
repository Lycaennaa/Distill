#![allow(
    unsafe_code,
    reason = "Windows log publication uses official Win32 APIs"
)]

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Wdk::Storage::FileSystem::{
    FILE_LINK_INFORMATION, FILE_LINK_INFORMATION_0, FileLinkInformation, NtSetInformationFile,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, LockFile,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;

use super::{
    LogError, NEXT_TEMP_ID, ParentDirectory, PrivateWindowsSecurity, TEMP_ATTEMPTS,
    create_directory, ensure_regular_file, is_already_exists, mark_for_delete, open_handle,
    open_workspace_directory_handle, private_file_access, release_file_lock, remove_temp_file,
    validate_windows_private_acl,
};
#[derive(Debug)]
pub(in crate::log) struct TempWorkspace {
    parent: ParentDirectory,
    name: String,
    directory: Option<File>,
    capture_guard: Option<File>,
    publish_guard: Option<File>,
    active: bool,
}
impl TempWorkspace {
    pub(in crate::log) fn create(
        parent: ParentDirectory,
        destination_name: &str,
    ) -> Result<Self, LogError> {
        let mut last_error = None;
        let normalized_name = destination_name.to_lowercase();
        for _ in 0..TEMP_ATTEMPTS {
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0_u128, |duration| duration.as_nanos());
            let counter = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let id = format!("{}{}{}", std::process::id(), clock, counter);
            let name = format!(".{normalized_name}.distill-stage-{id}.tmp");
            let path = parent.path.join(&name);
            let mut security = PrivateWindowsSecurity::new()
                .map_err(|error| LogError::new(format!("cannot create temporary ACL: {error}")))?;
            let attributes = security
                .security_attributes()
                .map_err(|error| LogError::new(format!("cannot create temporary ACL: {error}")))?;
            match create_directory(&path, &attributes) {
                Ok(()) => {
                    let directory = open_workspace_directory_handle(&path).map_err(|error| {
                        LogError::new(format!("cannot open temporary log workspace: {error}"))
                    })?;
                    validate_windows_private_acl(&directory).map_err(|error| {
                        LogError::new(format!("temporary workspace ACL is unsafe: {error}"))
                    })?;
                    return Ok(Self {
                        parent,
                        name,
                        directory: Some(directory),
                        capture_guard: None,
                        publish_guard: None,
                        active: true,
                    });
                }
                Err(error) if is_already_exists(&error) => last_error = Some(error),
                Err(error) => {
                    return Err(LogError::new(format!(
                        "cannot create temporary log workspace: {error}"
                    )));
                }
            }
        }
        Err(LogError::new(format!(
            "cannot create temporary log workspace: {}",
            last_error.map_or_else(
                || "too many name collisions".to_owned(),
                |error| error.to_string()
            )
        )))
    }

    pub(in crate::log) fn create_capture(&mut self) -> Result<File, LogError> {
        self.create_file("capture.tmp")
    }

    pub(in crate::log) fn create_publish(&mut self) -> Result<File, LogError> {
        self.create_file("publish.tmp")
    }

    fn create_file(&mut self, name: &str) -> Result<File, LogError> {
        let path = self.parent.path.join(&self.name).join(name);
        let mut security = PrivateWindowsSecurity::new()
            .map_err(|error| LogError::new(format!("cannot create temporary ACL: {error}")))?;
        let attributes = security
            .security_attributes()
            .map_err(|error| LogError::new(format!("cannot create temporary ACL: {error}")))?;
        let share = if name == "publish.tmp" {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        };
        let file = open_handle(
            &path,
            private_file_access(),
            share,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
            Some(&attributes),
        )
        .map_err(|error| LogError::new(format!("cannot create temporary log file: {error}")))?;
        ensure_regular_file(&file)?;
        validate_windows_private_acl(&file)
            .map_err(|error| LogError::new(format!("temporary log ACL is unsafe: {error}")))?;
        let guard = file
            .try_clone()
            .map_err(|error| LogError::new(format!("cannot retain temporary log lock: {error}")))?;
        // Lock all valid file offsets while this process writes and commits staged bytes.
        // SAFETY: `guard` owns a live handle opened for generic read and write access.
        unsafe { LockFile(HANDLE(guard.as_raw_handle()), 0, 0, u32::MAX, 0x7fff_ffff) }.map_err(
            |error| LogError::new(format!("cannot lock temporary log contents: {error}")),
        )?;
        match name {
            "capture.tmp" => self.capture_guard = Some(guard),
            "publish.tmp" => self.publish_guard = Some(guard),
            _ => return Err(LogError::new("unsupported temporary log filename")),
        }
        Ok(file)
    }

    pub(in crate::log) fn publish(
        &self,
        sanitized_file: &File,
        destination_name: &str,
    ) -> Result<(), LogError> {
        ensure_regular_file(sanitized_file)?;
        validate_windows_private_acl(sanitized_file)
            .map_err(|error| LogError::new(format!("sanitized log ACL is unsafe: {error}")))?;
        if destination_name.is_empty()
            || destination_name == "."
            || destination_name == ".."
            || destination_name
                .chars()
                .any(|character| matches!(character, '\\' | '/' | ':' | '\0'))
        {
            return Err(LogError::new("log destination must be a single file name"));
        }
        let file_name: Vec<u16> = destination_name.encode_utf16().collect();
        let file_name_bytes = file_name
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or_else(|| LogError::new("log destination name is too long"))?;
        let file_name_length = u32::try_from(file_name_bytes)
            .map_err(|_| LogError::new("log destination name is too long"))?;
        let information_length = std::mem::offset_of!(FILE_LINK_INFORMATION, FileName)
            .checked_add(file_name_bytes)
            .ok_or_else(|| LogError::new("log destination name is too long"))?;
        let word_count = information_length.div_ceil(std::mem::size_of::<usize>());
        let mut information = vec![0_usize; word_count];
        let link_information = information.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();
        // SAFETY: The aligned buffer covers the generated structure header and full UTF-16 name.
        unsafe {
            std::ptr::write(
                link_information,
                FILE_LINK_INFORMATION {
                    Anonymous: FILE_LINK_INFORMATION_0 {
                        ReplaceIfExists: false,
                    },
                    RootDirectory: HANDLE(self.parent.handle.as_raw_handle()),
                    FileNameLength: file_name_length,
                    FileName: [0],
                },
            );
            std::ptr::copy_nonoverlapping(
                file_name.as_ptr(),
                std::ptr::addr_of_mut!((*link_information).FileName).cast(),
                file_name.len(),
            );
        }
        let information_length = u32::try_from(information_length)
            .map_err(|_| LogError::new("log destination name is too long"))?;
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: Both handles are live; the input buffer and status block stay valid through the call.
        let status = unsafe {
            NtSetInformationFile(
                HANDLE(sanitized_file.as_raw_handle()),
                std::ptr::from_mut(&mut status_block),
                link_information.cast(),
                information_length,
                FileLinkInformation,
            )
        };
        if status.is_err() {
            return Err(LogError::new(format!(
                "cannot commit log without replacement (NTSTATUS {:#x})",
                status.0
            )));
        }
        Ok(())
    }

    pub(in crate::log) fn cleanup(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }
        let result = (|| {
            let workspace_path = self.parent.path.join(&self.name);
            release_file_lock(&mut self.capture_guard)?;
            release_file_lock(&mut self.publish_guard)?;
            for name in ["capture.tmp", "publish.tmp"] {
                remove_temp_file(&workspace_path.join(name))?;
            }
            let directory = self
                .directory
                .as_ref()
                .ok_or_else(|| io::Error::other("temporary workspace handle is closed"))?;
            mark_for_delete(directory)?;
            drop(self.directory.take());
            Ok::<(), io::Error>(())
        })();
        match result {
            Ok(()) => {
                self.active = false;
                None
            }
            Err(error) => Some(error.to_string()),
        }
    }

    #[cfg(test)]
    pub(in crate::log) fn directory_name_for_test(&self) -> &str {
        &self.name
    }
}
impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
