#![allow(
    unsafe_code,
    reason = "isolated FFI for checking inherited macOS filesystem ACLs"
)]

#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::fs::File;
use std::io;
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;

#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_get_fd_np(fd: i32, acl_type: i32) -> *mut c_void;
    fn acl_free(object: *mut c_void) -> i32;
}

#[cfg(not(windows))]
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
#[cfg(windows)]
pub fn validate_windows_parent_acl(file: &File) -> io::Result<()> {
    validate_windows_acl(file, false, 0)
}

#[cfg(windows)]
pub fn validate_windows_ancestor_acl(file: &File) -> io::Result<()> {
    // This right permits creating new siblings, not replacing existing path components.
    validate_windows_acl(
        file,
        false,
        windows::Win32::Storage::FileSystem::FILE_ADD_SUBDIRECTORY.0,
    )
}

#[cfg(windows)]
pub fn validate_windows_private_acl(file: &File) -> io::Result<()> {
    validate_windows_acl(file, true, 0)
}

#[cfg(windows)]
fn validate_windows_acl(
    file: &File,
    private: bool,
    allowed_untrusted_write_mask: u32,
) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR_CONTROL,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };

    let current_user = current_windows_user_sid()?;
    let administrators = well_known_windows_sid(WinBuiltinAdministratorsSid)?;
    let system = well_known_windows_sid(WinLocalSystemSid)?;
    let trusted_installer = trusted_installer_windows_sid()?;
    let trusted = [
        current_user.as_slice(),
        administrators.as_slice(),
        system.as_slice(),
        trusted_installer.as_slice(),
    ];
    let mut owner = PSID::default();
    let mut dacl = std::ptr::null_mut::<windows::Win32::Security::ACL>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `file` owns a valid kernel handle; output pointers live through the call.
    let status = unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(std::ptr::addr_of_mut!(owner)),
            None,
            Some(std::ptr::addr_of_mut!(dacl)),
            None,
            Some(std::ptr::addr_of_mut!(descriptor)),
        )
    };
    if status.0 != 0 {
        return Err(windows_status_error(status.0));
    }
    let _descriptor = SecurityDescriptorGuard(descriptor);
    if owner.is_invalid() || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log directory has a missing owner or null DACL",
        ));
    }
    // SAFETY: `owner` and the DACL are owned by the returned security descriptor.
    let owner_is_trusted = trusted.iter().any(|sid| unsafe {
        windows::Win32::Security::EqualSid(owner, PSID(sid.as_ptr().cast_mut().cast())).is_ok()
    });
    if !owner_is_trusted {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log directory owner is not trusted",
        ));
    }
    if private {
        let mut control = SECURITY_DESCRIPTOR_CONTROL(0);
        let mut revision = 0;
        // SAFETY: `descriptor` points to a valid descriptor returned by GetSecurityInfo.
        unsafe {
            GetSecurityDescriptorControl(
                descriptor,
                std::ptr::from_mut(&mut control.0),
                std::ptr::from_mut(&mut revision),
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
        }
        if !control.contains(SE_DACL_PROTECTED) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "temporary log DACL is not protected from inheritance",
            ));
        }
    }
    validate_windows_dacl(dacl, private, allowed_untrusted_write_mask, &trusted)
}
#[cfg(windows)]
fn windows_dword_size(size: usize) -> io::Result<u32> {
    u32::try_from(size).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows API size exceeds the DWORD limit",
        )
    })
}

#[cfg(windows)]
fn windows_status_error(status: u32) -> io::Error {
    i32::try_from(status).map_or_else(
        |_| {
            io::Error::other(format!(
                "Windows API returned an unrepresentable error status: {status:#010x}"
            ))
        },
        io::Error::from_raw_os_error,
    )
}

#[cfg(windows)]
fn validate_windows_dacl(
    dacl: *mut windows::Win32::Security::ACL,
    private: bool,
    allowed_untrusted_write_mask: u32,
    trusted: &[&[u8]],
) -> io::Result<()> {
    use windows::Win32::Security::{
        ACL_SIZE_INFORMATION, AclSizeInformation, GetAce, GetAclInformation, IsValidAcl,
    };

    // SAFETY: `dacl` is the valid pointer returned by GetSecurityInfo.
    if !unsafe { IsValidAcl(dacl) }.as_bool() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log DACL is malformed",
        ));
    }
    let mut acl_info = ACL_SIZE_INFORMATION::default();
    let acl_info_size = windows_dword_size(std::mem::size_of::<ACL_SIZE_INFORMATION>())?;
    // SAFETY: `dacl` is a valid ACL returned by GetSecurityInfo.
    unsafe {
        GetAclInformation(
            dacl,
            std::ptr::from_mut(&mut acl_info).cast(),
            acl_info_size,
            AclSizeInformation,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
    }
    for index in 0..acl_info.AceCount {
        let mut ace = std::ptr::null_mut();
        // SAFETY: `dacl` is valid and `index` is bounded by its reported ACE count.
        unsafe { GetAce(dacl, index, std::ptr::from_mut(&mut ace)) }
            .map_err(|error| io::Error::other(error.to_string()))?;
        validate_windows_ace(ace, private, allowed_untrusted_write_mask, trusted)?;
    }
    Ok(())
}

#[cfg(windows)]
fn validate_windows_ace(
    ace: *mut std::ffi::c_void,
    private: bool,
    allowed_untrusted_write_mask: u32,
    trusted: &[&[u8]],
) -> io::Result<()> {
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, GetLengthSid, IsValidSid, PSID,
    };

    const ACCESS_ALLOWED_ACE_TYPE_VALUE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE_VALUE: u8 = 1;

    if ace.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log DACL contains an invalid ACE",
        ));
    }
    // SAFETY: GetAce returned a pointer to an ACE in the validated DACL.
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if header.AceType != ACCESS_ALLOWED_ACE_TYPE_VALUE
        && header.AceType != ACCESS_DENIED_ACE_TYPE_VALUE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log DACL contains an unsupported ACE type",
        ));
    }
    if usize::from(header.AceSize) < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log DACL contains a truncated ACE",
        ));
    }
    let (mask, sid) = if header.AceType == ACCESS_ALLOWED_ACE_TYPE_VALUE {
        // SAFETY: the ACE type and size were checked above.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        (
            allowed.Mask,
            PSID(std::ptr::addr_of!(allowed.SidStart).cast_mut().cast()),
        )
    } else {
        // SAFETY: the ACE type and size were checked above.
        let denied = unsafe { &*ace.cast::<ACCESS_DENIED_ACE>() };
        (
            denied.Mask,
            PSID(std::ptr::addr_of!(denied.SidStart).cast_mut().cast()),
        )
    };
    let inherit_only_flag =
        u8::try_from(windows::Win32::Security::INHERIT_ONLY_ACE.0).map_err(io::Error::other)?;
    if header.AceType == ACCESS_DENIED_ACE_TYPE_VALUE || header.AceFlags & inherit_only_flag != 0 {
        return Ok(());
    }
    // SAFETY: `sid` points into a validated ACE containing a minimum-size SID.
    let sid_length = usize::try_from(unsafe { GetLengthSid(sid) }).map_err(io::Error::other)?;
    if sid_length > usize::from(header.AceSize).saturating_sub(8) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log DACL contains a malformed SID",
        ));
    }
    // SAFETY: the SID length is bounded by the validated ACE before IsValidSid reads it.
    if !unsafe { IsValidSid(sid) }.as_bool() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log DACL contains a malformed SID",
        ));
    }
    validate_windows_ace_access(mask, sid, private, allowed_untrusted_write_mask, trusted)
}

#[cfg(windows)]
fn validate_windows_ace_access(
    mask: u32,
    sid: windows::Win32::Security::PSID,
    private: bool,
    allowed_untrusted_write_mask: u32,
    trusted: &[&[u8]],
) -> io::Result<()> {
    const MAXIMUM_ALLOWED_MASK: u32 = 0x0200_0000;
    // SAFETY: the SID is validated and points either into the ACE or a trusted SID buffer.
    let principal_is_trusted = trusted.iter().any(|known| unsafe {
        windows::Win32::Security::EqualSid(
            sid,
            windows::Win32::Security::PSID(known.as_ptr().cast_mut().cast()),
        )
        .is_ok()
    });
    if private && !principal_is_trusted {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "temporary log DACL grants access to an untrusted principal",
        ));
    }
    let untrusted_write_mask = (MAXIMUM_ALLOWED_MASK
        | windows::Win32::Foundation::GENERIC_ALL.0
        | windows::Win32::Foundation::GENERIC_WRITE.0
        | windows::Win32::Storage::FileSystem::DELETE.0
        | windows::Win32::Storage::FileSystem::FILE_ADD_FILE.0
        | windows::Win32::Storage::FileSystem::FILE_ADD_SUBDIRECTORY.0
        | windows::Win32::Storage::FileSystem::FILE_DELETE_CHILD.0
        | windows::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES.0
        | windows::Win32::Storage::FileSystem::FILE_WRITE_EA.0
        | windows::Win32::Storage::FileSystem::WRITE_DAC.0
        | windows::Win32::Storage::FileSystem::WRITE_OWNER.0)
        & !allowed_untrusted_write_mask;
    if !principal_is_trusted && mask & untrusted_write_mask != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "log directory DACL grants write access to an untrusted principal (SID {}, mask {mask:#010x})",
                windows_sid_string(sid)?,
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub struct PrivateWindowsSecurity {
    descriptor: windows::Win32::Security::SECURITY_DESCRIPTOR,
    _acl: Vec<u32>,
}

#[cfg(windows)]
impl PrivateWindowsSecurity {
    pub fn new() -> io::Result<Self> {
        use windows::Win32::Security::{
            ACL, ACL_REVISION, AddAccessAllowedAce, InitializeAcl, InitializeSecurityDescriptor,
            SE_DACL_PROTECTED, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, WinBuiltinAdministratorsSid, WinLocalSystemSid,
        };

        let user_sid = current_windows_user_sid()?;
        let administrators = well_known_windows_sid(WinBuiltinAdministratorsSid)?;
        let system = well_known_windows_sid(WinLocalSystemSid)?;
        let mut acl = vec![0_u32; 128];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        let acl_size = windows_dword_size(std::mem::size_of_val(acl.as_slice()))?;
        // SAFETY: `acl` is suitably aligned and large enough for the bounded ACE list.
        unsafe {
            InitializeAcl(acl_ptr, acl_size, ACL_REVISION)
                .map_err(|error| io::Error::other(error.to_string()))?;
            for sid in [&user_sid, &administrators, &system] {
                AddAccessAllowedAce(
                    acl_ptr,
                    ACL_REVISION,
                    windows::Win32::Foundation::GENERIC_ALL.0,
                    windows::Win32::Security::PSID(sid.as_ptr().cast_mut().cast()),
                )
                .map_err(|error| io::Error::other(error.to_string()))?;
            }
        }

        let mut descriptor = std::mem::MaybeUninit::<SECURITY_DESCRIPTOR>::zeroed();
        // SAFETY: InitializeSecurityDescriptor initializes the official Windows descriptor.
        unsafe {
            InitializeSecurityDescriptor(
                windows::Win32::Security::PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast()),
                1,
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
        }
        // SAFETY: `descriptor` has been initialized and `acl_ptr` remains owned by `acl`.
        let mut descriptor = unsafe { descriptor.assume_init() };
        // SAFETY: both pointers remain valid for the lifetime of this security wrapper.
        unsafe {
            SetSecurityDescriptorDacl(
                windows::Win32::Security::PSECURITY_DESCRIPTOR(
                    std::ptr::from_mut(&mut descriptor).cast(),
                ),
                true,
                Some(acl_ptr.cast_const()),
                false,
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
            SetSecurityDescriptorControl(
                windows::Win32::Security::PSECURITY_DESCRIPTOR(
                    std::ptr::from_mut(&mut descriptor).cast(),
                ),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
            .map_err(|error| io::Error::other(error.to_string()))?;
        }
        Ok(Self {
            descriptor,
            _acl: acl,
        })
    }

    pub fn security_attributes(
        &mut self,
    ) -> io::Result<windows::Win32::Security::SECURITY_ATTRIBUTES> {
        let n_length = u32::try_from(std::mem::size_of::<
            windows::Win32::Security::SECURITY_ATTRIBUTES,
        >())
        .map_err(io::Error::other)?;
        Ok(windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: n_length,
            lpSecurityDescriptor: std::ptr::from_mut(&mut self.descriptor).cast(),
            bInheritHandle: false.into(),
        })
    }
}

#[cfg(windows)]
struct SecurityDescriptorGuard(windows::Win32::Security::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl Drop for SecurityDescriptorGuard {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};

        if !self.0.is_invalid() {
            // SAFETY: GetSecurityInfo allocated this descriptor with LocalAlloc.
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

#[cfg(windows)]
fn current_windows_user_sid() -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct TokenHandle(HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: this handle was returned by OpenProcessToken and is owned here.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    let mut token = HANDLE::default();
    // SAFETY: OpenProcessToken writes one owned token handle to `token`.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            std::ptr::from_mut(&mut token),
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))?;
    let _token = TokenHandle(token);
    let mut required = 0_u32;
    // SAFETY: this size query intentionally supplies no buffer.
    let _ = unsafe {
        GetTokenInformation(token, TokenUser, None, 0, std::ptr::from_mut(&mut required))
    };
    if required == 0 {
        return Err(io::Error::other(
            "cannot determine current Windows user SID size",
        ));
    }
    let buffer_size = usize::try_from(required).map_err(io::Error::other)?;
    let mut buffer = vec![0_u64; buffer_size.div_ceil(std::mem::size_of::<u64>())];
    // SAFETY: the aligned buffer has the size reported by GetTokenInformation.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            std::ptr::from_mut(&mut required),
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))?;
    // SAFETY: TokenUser filled the buffer with a TOKEN_USER structure.
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_bytes(token_user.User.Sid)
}

#[cfg(windows)]
fn well_known_windows_sid(
    sid_type: windows::Win32::Security::WELL_KNOWN_SID_TYPE,
) -> io::Result<Vec<u8>> {
    use windows::Win32::Security::{CreateWellKnownSid, PSID};

    let mut storage = [0_u32; 17];
    let mut size = windows_dword_size(std::mem::size_of_val(&storage))?;
    // SAFETY: the aligned output buffer is large enough for any well-known SID.
    unsafe {
        CreateWellKnownSid(
            sid_type,
            None,
            Some(PSID(storage.as_mut_ptr().cast())),
            std::ptr::from_mut(&mut size),
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))?;
    sid_bytes(PSID(storage.as_mut_ptr().cast()))
}
#[cfg(windows)]
fn trusted_installer_windows_sid() -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows::Win32::Security::PSID;

    let mut sid = PSID::default();
    // SAFETY: the generated API writes one locally allocated SID to `sid`.
    unsafe {
        ConvertStringSidToSidW(
            windows::core::w!("S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"),
            std::ptr::addr_of_mut!(sid),
        )
    }
    .map_err(|error| io::Error::other(error.to_string()))?;
    // SAFETY: the SID returned by ConvertStringSidToSidW is valid until LocalFree.
    let result = sid_bytes(sid);
    // SAFETY: ConvertStringSidToSidW allocates its output with LocalAlloc.
    let _ = unsafe { LocalFree(Some(HLOCAL(sid.0))) };
    result
}

#[cfg(windows)]
fn sid_bytes(sid: windows::Win32::Security::PSID) -> io::Result<Vec<u8>> {
    use windows::Win32::Security::{GetLengthSid, IsValidSid};

    if sid.is_invalid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null SID",
        ));
    }
    // SAFETY: `sid` is returned by the current process token or CreateWellKnownSid.
    if !unsafe { IsValidSid(sid) }.as_bool() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned an invalid SID",
        ));
    }
    // SAFETY: the SID has just been validated.
    let length = usize::try_from(unsafe { GetLengthSid(sid) }).map_err(io::Error::other)?;
    // SAFETY: a valid SID has `length` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), length) };
    Ok(bytes.to_vec())
}
#[cfg(windows)]
fn windows_sid_string(sid: windows::Win32::Security::PSID) -> io::Result<String> {
    let bytes = sid_bytes(sid)?;
    let invalid_length = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned an invalid SID length",
        )
    };
    let header = bytes.get(..8).ok_or_else(invalid_length)?;
    let mut header_bytes = header.iter().copied();
    let revision = header_bytes.next().ok_or_else(invalid_length)?;
    let sub_authority_count = usize::from(header_bytes.next().ok_or_else(invalid_length)?);
    let expected_length = sub_authority_count
        .checked_mul(4)
        .and_then(|length| length.checked_add(8))
        .ok_or_else(invalid_length)?;
    if bytes.len() != expected_length {
        return Err(invalid_length());
    }
    let identifier_authority = bytes
        .get(2..8)
        .ok_or_else(invalid_length)?
        .iter()
        .fold(0_u64, |authority, byte| (authority << 8) | u64::from(*byte));
    let mut display = format!("S-{revision}-{identifier_authority}");
    let sub_authority_bytes = bytes.get(8..).ok_or_else(invalid_length)?;
    let (sub_authorities, remainder) = sub_authority_bytes.as_chunks::<4>();
    if !remainder.is_empty() {
        return Err(invalid_length());
    }
    for sub_authority in sub_authorities {
        let sub_authority = u32::from_le_bytes(*sub_authority);
        display.push('-');
        display.push_str(&sub_authority.to_string());
    }
    Ok(display)
}

#[cfg(all(windows, test))]
pub fn set_test_untrusted_write_acl(file: &File) -> io::Result<()> {
    set_test_untrusted_acl(file, windows::Win32::Storage::FileSystem::FILE_ADD_FILE.0)
}

#[cfg(all(windows, test))]
pub fn set_test_untrusted_subdirectory_acl(file: &File) -> io::Result<()> {
    set_test_untrusted_acl(
        file,
        windows::Win32::Storage::FileSystem::FILE_ADD_SUBDIRECTORY.0,
    )
}

#[cfg(all(windows, test))]
fn set_test_untrusted_acl(file: &File, access_mask: u32) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION, InitializeAcl,
        PROTECTED_DACL_SECURITY_INFORMATION, WinWorldSid,
    };

    let user = current_windows_user_sid()?;
    let world = well_known_windows_sid(WinWorldSid)?;
    let mut acl = vec![0_u32; 128];
    let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
    let acl_size = windows_dword_size(std::mem::size_of_val(acl.as_slice()))?;
    // SAFETY: the aligned ACL buffer has room for both SIDs.
    unsafe {
        InitializeAcl(acl_ptr, acl_size, ACL_REVISION)
            .map_err(|error| io::Error::other(error.to_string()))?;
        AddAccessAllowedAce(
            acl_ptr,
            ACL_REVISION,
            windows::Win32::Foundation::GENERIC_ALL.0,
            windows::Win32::Security::PSID(user.as_ptr().cast_mut().cast()),
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        AddAccessAllowedAce(
            acl_ptr,
            ACL_REVISION,
            access_mask,
            windows::Win32::Security::PSID(world.as_ptr().cast_mut().cast()),
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
    }
    // SAFETY: `file` owns a handle with WRITE_DAC access and `acl_ptr` stays valid for the call.
    let status = unsafe {
        SetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl_ptr.cast_const()),
            None,
        )
    };
    if status.0 != 0 {
        return Err(windows_status_error(status.0));
    }
    Ok(())
}
