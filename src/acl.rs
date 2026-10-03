#![allow(
    unsafe_code,
    reason = "isolated FFI for checking inherited macOS filesystem ACLs"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_get_fd_np(fd: i32, acl_type: i32) -> *mut c_void;
    fn acl_free(object: *mut c_void) -> i32;
}

pub fn has_extended_acl(file: &File) -> io::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `file` owns a live descriptor and ACL_TYPE_EXTENDED is a valid macOS ACL type.
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) {
                Ok(false)
            } else {
                Err(error)
            };
        }

        // SAFETY: `acl` is the owned non-null value returned by acl_get_fd_np above.
        if unsafe { acl_free(acl) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "extended ACL inspection is supported only on macOS",
        ))
    }
}
