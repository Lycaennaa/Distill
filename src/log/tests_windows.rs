#![allow(
    unsafe_code,
    reason = "Windows tests use official generated Windows APIs"
)]
use std::fs::{self, File};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::symlink_file;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Storage::FileSystem::WRITE_DAC;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
};
use windows::core::PCWSTR;

use super::*;

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn create_private_stale_workspace(path: &Path) {
    let mut security = crate::acl::PrivateWindowsSecurity::new().expect("build private DACL");
    let attributes = security
        .security_attributes()
        .expect("build security attributes");
    let wide = wide_path(path);
    unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(std::ptr::from_ref(&attributes))) }
        .expect("create private stale workspace");

    for name in ["capture.tmp", "publish.tmp"] {
        let child_path = path.join(name);
        let child_wide = wide_path(&child_path);
        let mut child_security =
            crate::acl::PrivateWindowsSecurity::new().expect("build private file DACL");
        let child_attributes = child_security
            .security_attributes()
            .expect("build child security attributes");
        let handle = unsafe {
            CreateFileW(
                PCWSTR(child_wide.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | READ_CONTROL).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                Some(std::ptr::from_ref(&child_attributes)),
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .expect("create stale private log file");
        // SAFETY: CreateFileW returned an owned file handle.
        drop(unsafe { File::from_raw_handle(handle.0 as _) });
    }
}
static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

pub(super) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub(super) fn new(label: &str) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("local application data directory")
            .join(format!(
                "distill-log-windows-{label}-{}-{id}",
                std::process::id()
            ));
        let mut security =
            crate::acl::PrivateWindowsSecurity::new().expect("build private test directory DACL");
        let attributes = security
            .security_attributes()
            .expect("build test directory security attributes");
        let wide = wide_path(&path);
        unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(std::ptr::from_ref(&attributes))) }
            .expect("create private test directory");
        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open_directory_for_security(path: &Path) -> File {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            (FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .expect("open test directory");
    // SAFETY: CreateFileW returned an owned directory handle.
    unsafe { File::from_raw_handle(handle.0 as _) }
}

#[test]
fn windows_publication_sanitizes_and_keeps_logs_private() {
    let directory = TestDirectory::new("publication");
    let path = directory.path().join("published.log");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");

    let raw_path = publisher.capture_path_for_test();
    let raw_file = File::open(&raw_path).expect("open raw capture for ACL inspection");
    assert!(crate::acl::validate_windows_private_acl(&raw_file).is_ok());
    drop(raw_file);

    let workspace = publisher.workspace_path_for_test();
    let workspace_handle = open_directory_for_security(&workspace);
    assert!(crate::acl::validate_windows_private_acl(&workspace_handle).is_ok());
    drop(workspace_handle);

    publisher
        .write_chunk(b"password=secret-value\n")
        .expect("capture raw output");
    let result = publisher.publish(RedactionPolicy::BestEffort, None, &CancellationToken::new());
    let published = result.published().expect("publish sanitized output");
    let saved = fs::read_to_string(published.path()).expect("read published output");
    assert_eq!(saved, "password=[REDACTED]\n");
    let published_file = File::open(published.path()).expect("open published log");
    assert!(crate::acl::validate_windows_private_acl(&published_file).is_ok());
}

#[test]
fn windows_reservation_is_exclusive_and_reusable_after_abort() {
    let directory = TestDirectory::new("reservation");
    let path = directory.path().join("reserved.log");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve first writer");
    assert!(LogPublisher::prepare(&path).is_err());

    publisher.abort();
    drop(publisher);
    let mut next = LogPublisher::prepare(&path).expect("reuse released reservation");
    next.abort();
}

#[test]
fn windows_cleans_stale_private_workspace_from_previous_process() {
    let directory = TestDirectory::new("crash-cleanup");
    let destination = directory.path().join("recovered.log");
    let stale = directory
        .path()
        .join(".recovered.log.distill-stage-123.tmp");
    create_private_stale_workspace(&stale);

    let mut publisher = LogPublisher::prepare(&destination).expect("prepare after prior crash");
    assert!(!stale.exists());
    publisher.abort();
}
#[test]
fn windows_rejects_parent_with_untrusted_write_acl() {
    let directory = TestDirectory::new("unsafe-acl");
    let directory_handle = open_directory_for_security(directory.path());
    crate::acl::set_test_untrusted_write_acl(&directory_handle)
        .expect("install untrusted directory ACE");
    drop(directory_handle);

    assert!(LogPublisher::prepare(directory.path().join("unsafe.log")).is_err());
}

#[test]
fn windows_rejects_reparse_destination_and_parent_when_available() {
    let directory = TestDirectory::new("reparse");
    let target = directory.path().join("target.log");
    fs::write(&target, b"target\n").expect("write target");
    let link = directory.path().join("linked.log");
    if symlink_file(&target, &link).is_err() {
        return;
    }
    assert!(LogPublisher::prepare(&link).is_err());

    let target_dir = directory.path().join("target-dir");
    fs::create_dir(&target_dir).expect("create target directory");
    let parent_link = directory.path().join("linked-dir");
    if std::os::windows::fs::symlink_dir(&target_dir, &parent_link).is_ok() {
        assert!(LogPublisher::prepare(parent_link.join("output.log")).is_err());
    }
}

#[test]
fn windows_capture_handle_cannot_be_replaced_by_path_rename() {
    let directory = TestDirectory::new("capture-race");
    let path = directory.path().join("capture.log");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    let capture = publisher.capture_path_for_test();
    let workspace = publisher.workspace_path_for_test();
    let moved_workspace = directory.path().join("moved-workspace.tmp");
    assert!(fs::rename(&workspace, &moved_workspace).is_err());
    assert!(workspace.exists());
    let moved = directory.path().join("moved-capture.tmp");
    assert!(fs::rename(&capture, &moved).is_err());
    assert!(capture.exists());

    publisher
        .write_chunk(b"password=original-secret\n")
        .expect("write through capture handle");
    let result = publisher.publish(RedactionPolicy::BestEffort, None, &CancellationToken::new());
    assert!(result.published().is_some());
    let saved = fs::read_to_string(&path).expect("read committed log");
    assert_eq!(saved, "password=[REDACTED]\n");
}

#[test]
fn windows_destination_race_never_replaces_existing_file() {
    let directory = TestDirectory::new("destination-race");
    let path = directory.path().join("raced.log");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    publisher
        .write_chunk(b"password=original-secret\n")
        .expect("capture output");
    fs::write(&path, b"attacker-owned\n").expect("create racing destination");

    let result = publisher.publish(RedactionPolicy::BestEffort, None, &CancellationToken::new());
    assert_eq!(
        result.failure().map(|(class, _)| class),
        Some(StatusClass::LogWrite)
    );
    assert!(result.published().is_none());
    assert_eq!(
        fs::read(&path).expect("read existing destination"),
        b"attacker-owned\n"
    );
}

#[test]
fn publish_links_from_the_sanitized_handle_after_path_replacement() {
    let directory = TestDirectory::new("publish-handle");
    let parent = open_directory(directory.path()).expect("open log parent");
    let mut workspace = TempWorkspace::create(parent, "handle").expect("create log workspace");
    let mut sanitized = workspace
        .create_publish()
        .expect("create sanitized workspace file");
    std::io::Write::write_all(&mut sanitized, b"sanitized payload\n")
        .expect("write sanitized content");
    sanitized.sync_all().expect("flush sanitized content");

    let staged = directory.path().join(workspace.directory_name_for_test());
    let staged_path = staged.join("publish.tmp");
    let moved_path = directory.path().join("moved-publish.tmp");
    fs::rename(&staged_path, &moved_path).expect("move staged file name");
    fs::write(&staged_path, b"attacker-owned\n").expect("replace staged path");

    workspace
        .publish(&sanitized, "published.log")
        .expect("commit the opened sanitized file");
    let published = directory.path().join("published.log");
    assert_eq!(
        fs::read(&published).expect("read committed output"),
        b"sanitized payload\n"
    );
    assert_eq!(
        fs::read(&staged_path).expect("read replacement path"),
        b"attacker-owned\n"
    );

    drop(sanitized);
    assert!(workspace.cleanup().is_none());
    fs::remove_file(moved_path).expect("remove moved staged file");
    fs::remove_file(published).expect("remove committed output");
}
