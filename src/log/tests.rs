use std::fs;
use std::time::Duration;

use super::*;

fn destination(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("distill-phase3-{name}-{}.log", std::process::id()))
}
fn remove_lock_file(path: &Path) {
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return;
    };
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    drop(fs::remove_file(
        parent.join(format!(".{name}.distill.lock")),
    ));
}

#[test]
fn publishes_sanitized_log_with_checksum() {
    let path = destination("publish");
    drop(fs::remove_file(&path));
    let mut log_writer = LogPublisher::prepare(&path).expect("reserve log");
    log_writer
        .write_chunk(b"token=secret\n\x1b[31mhello\x1b[0m\n")
        .expect("capture log");
    let result = log_writer.publish(RedactionPolicy::BestEffort, None, &CancellationToken::new());
    let published = result.published().expect("published log");
    assert_eq!(published.bytes(), 23);
    assert_eq!(
        fs::read_to_string(&path).expect("read log"),
        "token=[REDACTED]\nhello\n"
    );
    assert_eq!(published.checksum().len(), 64);
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
}

#[test]
fn existing_destination_is_not_overwritten() {
    let path = destination("existing");
    fs::write(&path, "keep").expect("create destination");
    assert!(LogPublisher::prepare(&path).is_err());
    assert_eq!(fs::read_to_string(&path).expect("read destination"), "keep");
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
}

#[test]
fn concurrent_reservation_is_rejected() {
    let path = destination("reserved");
    drop(fs::remove_file(&path));
    let first = LogPublisher::prepare(&path).expect("reserve destination");
    assert!(LogPublisher::prepare(&path).is_err());
    drop(first);
    let second = LogPublisher::prepare(&path).expect("reuse released destination");
    drop(second);
    remove_lock_file(&path);
}
#[test]
fn cleanup_preserves_neighbor_destination_capture() {
    let path = destination("cleanup-scope");
    let neighbor = PathBuf::from(format!("{}.distill-other", path.display()));
    drop(fs::remove_file(&path));
    drop(fs::remove_file(&neighbor));
    let neighbor_capture = LogPublisher::prepare(&neighbor).expect("reserve neighboring log");
    let neighbor_raw = neighbor_capture.capture_path_for_test();
    let destination_writer = LogPublisher::prepare(&path).expect("reserve destination log");

    assert!(neighbor_raw.exists());

    drop(destination_writer);
    drop(neighbor_capture);
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
    remove_lock_file(&neighbor);
}
#[cfg(unix)]
#[test]
fn raw_capture_uses_exclusive_file_handle_after_path_replacement() {
    let path = destination("capture-handle");
    let target = destination("capture-target");
    drop(fs::remove_file(&path));
    drop(fs::remove_file(&target));
    fs::write(&target, "preserve").expect("create symlink target");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    let raw_path = publisher.capture_path_for_test();
    fs::remove_file(&raw_path).expect("remove temporary path");
    std::os::unix::fs::symlink(&target, &raw_path).expect("replace temporary path");

    publisher
        .write_chunk(b"unredacted child output")
        .expect("capture child output");
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "preserve"
    );

    publisher.abort();
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "preserve"
    );
    drop(fs::remove_file(&path));
    drop(fs::remove_file(&target));
    remove_lock_file(&path);
}
#[cfg(unix)]
#[test]
fn publication_sanitizes_original_capture_after_path_replacement() {
    let path = destination("publish-handle");
    let target = destination("publish-target");
    drop(fs::remove_file(&path));
    drop(fs::remove_file(&target));
    fs::write(&target, "attacker target\n").expect("create symlink target");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    let raw_path = publisher.capture_path_for_test();
    publisher
        .write_chunk(b"DEVELOPMENT_TEAM = TEAMSECRET\n")
        .expect("capture child output");
    fs::remove_file(&raw_path).expect("remove temporary path");
    std::os::unix::fs::symlink(&target, &raw_path).expect("replace temporary path");

    let result = publisher.publish(
        RedactionPolicy::XcodeMandatory,
        None,
        &CancellationToken::new(),
    );
    assert!(result.published().is_some());
    let saved = fs::read_to_string(&path).expect("read published log");
    assert!(saved.contains("[TEAM_ID]"));
    assert!(!saved.contains("TEAMSECRET"));
    assert_eq!(
        fs::read_to_string(&target).expect("read symlink target"),
        "attacker target\n"
    );

    drop(fs::remove_file(&path));
    drop(fs::remove_file(&target));
    remove_lock_file(&path);
}
#[cfg(unix)]
#[test]
fn publication_links_from_private_workspace_handle_not_replaced_path() {
    let path = destination("stage-handle");
    let attacker_dir = destination("stage-attacker");
    drop(fs::remove_file(&path));
    drop(fs::remove_dir_all(&attacker_dir));
    fs::create_dir(&attacker_dir).expect("create attacker directory");
    fs::write(attacker_dir.join("publish.tmp"), "ATTACKERSECRET\n")
        .expect("create attacker payload");
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    let stage_path = publisher.workspace_path_for_test();
    let moved_stage = PathBuf::from(format!("{}.moved", stage_path.display()));
    drop(fs::remove_dir_all(&moved_stage));
    fs::rename(&stage_path, &moved_stage).expect("move private stage entry");
    std::os::unix::fs::symlink(&attacker_dir, &stage_path).expect("replace stage path");
    publisher
        .write_chunk(b"password=SOURCESECRET\n")
        .expect("capture child output");

    let result = publisher.publish(RedactionPolicy::BestEffort, None, &CancellationToken::new());
    assert_eq!(
        result.failure().map(|(class, _)| class),
        Some(StatusClass::LogWrite)
    );
    assert!(result.published().is_some());
    let saved = fs::read_to_string(&path).expect("read committed log");
    assert!(saved.contains("password=[REDACTED]"));
    assert!(!saved.contains("ATTACKERSECRET"));
    assert_eq!(
        fs::read_to_string(attacker_dir.join("publish.tmp")).expect("read attacker payload"),
        "ATTACKERSECRET\n"
    );

    drop(publisher);
    drop(fs::remove_file(&stage_path));
    drop(fs::remove_dir_all(&moved_stage));
    drop(fs::remove_dir_all(&attacker_dir));
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
}

#[cfg(unix)]
#[test]
fn symlink_destination_is_rejected_without_following() {
    let path = destination("symlink");
    let target = destination("symlink-target");
    drop(fs::remove_file(&path));
    fs::write(&target, "preserve").expect("create target");
    std::os::unix::fs::symlink(&target, &path).expect("create symlink");

    assert!(LogPublisher::prepare(&path).is_err());
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        "preserve"
    );

    drop(fs::remove_file(&path));
    drop(fs::remove_file(&target));
    remove_lock_file(&path);
}

#[test]
fn cancellation_before_commit_removes_temporary_capture() {
    let path = destination("cancel");
    drop(fs::remove_file(&path));
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    publisher.write_chunk(b"content\n").expect("capture log");
    let token = CancellationToken::new();
    token.cancel();
    let result = publisher.publish(RedactionPolicy::BestEffort, None, &token);
    assert_eq!(
        result.failure().map(|(class, _)| class),
        Some(StatusClass::Interrupt)
    );
    assert!(!path.exists());
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
}

#[test]
fn publication_checks_deadline() {
    let path = destination("deadline");
    drop(fs::remove_file(&path));
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    publisher.write_chunk(b"content\n").expect("capture log");
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(1))
        .expect("deadline");
    std::thread::sleep(Duration::from_millis(2));
    let result = publisher.publish(
        RedactionPolicy::BestEffort,
        Some(deadline),
        &CancellationToken::new(),
    );
    assert_eq!(
        result.failure().map(|(class, _)| class),
        Some(StatusClass::Timeout)
    );
    drop(fs::remove_file(&path));
    remove_lock_file(&path);
}
#[test]
fn overlong_xcode_line_fails_closed_before_publication() {
    let path = destination("overlong-xcode");
    drop(fs::remove_file(&path));
    let mut publisher = LogPublisher::prepare(&path).expect("reserve log");
    let label = b"DEVELOPMENT_TEAM =";
    let mut content = vec![b'x'; super::sanitize::MAX_PENDING_LINE_BYTES - label.len()];
    content.extend_from_slice(label);
    content.extend_from_slice(b"TEAMSECRET");
    content.extend(std::iter::repeat_n(b'x', 20_000));
    content.push(b'\n');
    publisher
        .write_chunk(&content)
        .expect("capture complete line");

    let result = publisher.publish(
        RedactionPolicy::XcodeMandatory,
        None,
        &CancellationToken::new(),
    );
    assert_eq!(
        result.failure().map(|(class, _)| class),
        Some(StatusClass::LogWrite)
    );
    remove_lock_file(&path);
}
