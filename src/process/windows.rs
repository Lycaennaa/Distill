#![allow(
    unsafe_code,
    reason = "Windows process supervision uses official Win32 APIs"
)]
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{forget, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use super::child::{ChildPipe, ChildProcess};

pub(super) struct WindowsChild {
    process: OwnedWinHandle,
    job: Option<OwnedWinHandle>,
    pid: u32,
    stdout: Option<File>,
    stderr: Option<File>,
}

impl WindowsChild {
    pub(super) fn spawn(
        program: &OsStr,
        arguments: &[OsString],
        environment: &[(OsString, OsString)],
        current_directory: Option<&Path>,
    ) -> io::Result<Self> {
        let job = create_job()?;
        let security = inheritable_security_attributes()?;
        let stdin = open_null_stdin(&security)?;
        let (stdout_read, stdout_write) = create_pipe(&security)?;
        let (stderr_read, stderr_write) = create_pipe(&security)?;
        let mut command_line = make_command_line(program, arguments)?;
        let environment_block = make_environment_block(environment)?;
        let current_directory = current_directory
            .map(|path| encode_wide(path.as_os_str()))
            .transpose()?;

        let inherited_handles = [stdin.get(), stdout_write.get(), stderr_write.get()];
        let attribute_list = ThreadAttributeList::new(&inherited_handles)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb =
            u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin.get();
        startup.StartupInfo.hStdOutput = stdout_write.get();
        startup.StartupInfo.hStdError = stderr_write.get();
        startup.lpAttributeList = attribute_list.list;

        let environment_pointer: Option<*const std::ffi::c_void> =
            (!environment_block.is_empty()).then(|| environment_block.as_ptr().cast());
        let current_directory_pointer = current_directory
            .as_ref()
            .map_or_else(PCWSTR::null, |value| PCWSTR(value.as_ptr()));
        let mut process_information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NEW_PROCESS_GROUP
                    | CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment_pointer,
                current_directory_pointer,
                std::ptr::from_ref(&startup.StartupInfo),
                std::ptr::from_mut(&mut process_information),
            )
        }
        .map_err(|error| windows_error(&error))?;

        let process = OwnedWinHandle::new(process_information.hProcess);
        let thread = OwnedWinHandle::new(process_information.hThread);
        drop(stdin);
        drop(stdout_write);
        drop(stderr_write);

        if let Err(error) = unsafe { AssignProcessToJobObject(job.get(), process.get()) } {
            terminate_suspended_process(&job, &process);
            return Err(windows_error(&error));
        }

        if unsafe { ResumeThread(thread.get()) } == u32::MAX {
            let error = io::Error::last_os_error();
            terminate_suspended_process(&job, &process);
            return Err(error);
        }
        drop(thread);

        Ok(Self {
            process,
            job: Some(job),
            pid: process_information.dwProcessId,
            stdout: Some(stdout_read.into_file()),
            stderr: Some(stderr_read.into_file()),
        })
    }

    fn try_wait_inner(&mut self) -> io::Result<Option<ExitStatus>> {
        let wait = unsafe { WaitForSingleObject(self.process.get(), 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }

        let mut exit_code = 0;
        unsafe { GetExitCodeProcess(self.process.get(), std::ptr::from_mut(&mut exit_code)) }
            .map_err(|error| windows_error(&error))?;
        Ok(Some(ExitStatus::from_raw(exit_code)))
    }

    fn active_process_count(&self) -> io::Result<u32> {
        let Some(job) = self.job.as_ref().map(OwnedWinHandle::get) else {
            return Ok(0);
        };
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: The output structure is writable and the job handle remains open during the query.
        unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut accounting).cast(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                    .map_err(io::Error::other)?,
                None,
            )
        }
        .map_err(|error| windows_error(&error))?;
        Ok(accounting.ActiveProcesses)
    }

    fn terminate_job_inner(&self) -> io::Result<bool> {
        let Some(job) = self.job.as_ref().map(OwnedWinHandle::get) else {
            return Ok(true);
        };
        if self.active_process_count()? == 0 {
            return Ok(true);
        }
        // SAFETY: `job` is a live Job Object handle owned by this child.
        unsafe { TerminateJobObject(job, 1) }.map_err(|error| windows_error(&error))?;
        Ok(false)
    }

    fn send_break_inner(&self) -> io::Result<bool> {
        if self.active_process_count()? == 0 {
            return Ok(true);
        }
        // SAFETY: The process was launched as a new console group and `pid` is its group id.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) }
            .map_err(|error| windows_error(&error))?;
        Ok(false)
    }
}

impl ChildProcess for WindowsChild {
    fn id(&self) -> u32 {
        self.pid
    }

    fn take_stdout(&mut self) -> Option<ChildPipe> {
        self.stdout.take().map(ChildPipe::File)
    }

    fn take_stderr(&mut self) -> Option<ChildPipe> {
        self.stderr.take().map(ChildPipe::File)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.try_wait_inner()
    }

    fn kill(&mut self) -> io::Result<()> {
        unsafe { TerminateProcess(self.process.get(), 1) }.map_err(|error| windows_error(&error))
    }

    fn terminate_job(&mut self) -> io::Result<bool> {
        self.terminate_job_inner()
    }
    fn send_break(&mut self) -> io::Result<bool> {
        self.send_break_inner()
    }
}

impl Drop for WindowsChild {
    fn drop(&mut self) {
        if unsafe { WaitForSingleObject(self.process.get(), 0) } != WAIT_OBJECT_0 {
            let _ = self.terminate_job_inner();
            let _ = unsafe { TerminateProcess(self.process.get(), 1) };
            let _ = unsafe { WaitForSingleObject(self.process.get(), INFINITE) };
        }
    }
}

struct OwnedWinHandle(HANDLE);

impl OwnedWinHandle {
    const fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    const fn get(&self) -> HANDLE {
        self.0
    }

    fn into_file(self) -> File {
        let raw_handle: RawHandle = self.0.0;
        forget(self);
        unsafe { File::from_raw_handle(raw_handle) }
    }
}

impl Drop for OwnedWinHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct ThreadAttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl ThreadAttributeList {
    fn new(handles: &[HANDLE]) -> io::Result<Self> {
        let mut required_size = 0_usize;
        let _initialization = unsafe {
            InitializeProcThreadAttributeList(None, 1, None, std::ptr::from_mut(&mut required_size))
        };
        if required_size == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut storage = vec![0_usize; required_size.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(
                Some(list),
                1,
                None,
                std::ptr::from_mut(&mut required_size),
            )
        }
        .map_err(|error| windows_error(&error))?;
        let attributes = Self {
            list,
            _storage: storage,
        };
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                usize::try_from(PROC_THREAD_ATTRIBUTE_HANDLE_LIST).map_err(io::Error::other)?,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(|error| windows_error(&error))?;
        Ok(attributes)
    }
}

impl Drop for ThreadAttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

fn create_job() -> io::Result<OwnedWinHandle> {
    let job =
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|error| windows_error(&error))?;
    let job = OwnedWinHandle::new(job);
    let mut limits = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.get(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .map_err(io::Error::other)?,
        )
    }
    .map_err(|error| windows_error(&error))?;
    Ok(job)
}

fn inheritable_security_attributes() -> io::Result<SECURITY_ATTRIBUTES> {
    Ok(SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    })
}

fn open_null_stdin(security: &SECURITY_ATTRIBUTES) -> io::Result<OwnedWinHandle> {
    let null = encode_wide(OsStr::new("NUL"))?;
    let handle = unsafe {
        CreateFileW(
            PCWSTR(null.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(std::ptr::from_ref(security)),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| windows_error(&error))?;
    Ok(OwnedWinHandle::new(handle))
}

fn create_pipe(security: &SECURITY_ATTRIBUTES) -> io::Result<(OwnedWinHandle, OwnedWinHandle)> {
    let mut read = OwnedWinHandle::new(HANDLE::default());
    let mut write = OwnedWinHandle::new(HANDLE::default());
    unsafe {
        CreatePipe(
            std::ptr::from_mut(&mut read.0),
            std::ptr::from_mut(&mut write.0),
            Some(std::ptr::from_ref(security)),
            0,
        )
    }
    .map_err(|error| windows_error(&error))?;
    unsafe {
        windows::Win32::Foundation::SetHandleInformation(
            read.get(),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS(0),
        )
    }
    .map_err(|error| windows_error(&error))?;
    Ok((read, write))
}

fn terminate_suspended_process(job: &OwnedWinHandle, process: &OwnedWinHandle) {
    let _ = unsafe { TerminateJobObject(job.get(), 1) };
    let _ = unsafe { TerminateProcess(process.get(), 1) };
    let _ = unsafe { WaitForSingleObject(process.get(), INFINITE) };
}

fn make_command_line(program: &OsStr, arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let mut command_line = Vec::new();
    append_quoted_argument(&mut command_line, program)?;
    for argument in arguments {
        command_line.push(u16::from(b' '));
        append_quoted_argument(&mut command_line, argument)?;
    }
    command_line.push(0);
    Ok(command_line)
}

fn append_quoted_argument(command_line: &mut Vec<u16>, argument: &OsStr) -> io::Result<()> {
    if argument.encode_wide().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process argument contains a NUL character",
        ));
    }

    command_line.push(u16::from(b'"'));
    let mut backslashes = 0_usize;
    for unit in argument.encode_wide() {
        match unit {
            unit if unit == u16::from(b'\\') => {
                backslashes = backslashes
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("process argument is too long"))?;
            }
            unit if unit == u16::from(b'"') => {
                let escaped_backslashes = backslashes
                    .checked_mul(2)
                    .and_then(|count| count.checked_add(1))
                    .ok_or_else(|| io::Error::other("process argument is too long"))?;
                command_line.extend(std::iter::repeat_n(u16::from(b'\\'), escaped_backslashes));
                command_line.push(unit);
                backslashes = 0;
            }
            _ => {
                command_line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
                command_line.push(unit);
                backslashes = 0;
            }
        }
    }
    let escaped_backslashes = backslashes
        .checked_mul(2)
        .ok_or_else(|| io::Error::other("process argument is too long"))?;
    command_line.extend(std::iter::repeat_n(u16::from(b'\\'), escaped_backslashes));
    command_line.push(u16::from(b'"'));
    Ok(())
}

fn make_environment_block(overrides: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
    if overrides.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in overrides {
        if key
            .encode_wide()
            .any(|unit| unit == 0 || unit == u16::from(b'='))
            || value.encode_wide().any(|unit| unit == 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process environment contains an invalid key or value",
            ));
        }
        if let Some((_, existing_value)) = entries
            .iter_mut()
            .find(|(existing_key, _)| environment_keys_equal(existing_key, key))
        {
            existing_value.clone_from(value);
        } else {
            entries.push((key.clone(), value.clone()));
        }
    }
    entries.sort_by_cached_key(|(key, _)| folded_environment_key(key));

    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn environment_keys_equal(left: &OsStr, right: &OsStr) -> bool {
    folded_environment_key(left) == folded_environment_key(right)
}

fn folded_environment_key(key: &OsStr) -> Vec<u16> {
    key.encode_wide()
        .map(|unit| {
            if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) {
                unit.saturating_sub(32)
            } else {
                unit
            }
        })
        .collect()
}

fn encode_wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let encoded: Vec<u16> = value.encode_wide().collect();
    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process value contains a NUL character",
        ));
    }
    let mut terminated = encoded;
    terminated.push(0);
    Ok(terminated)
}

fn windows_error(error: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0)
}
