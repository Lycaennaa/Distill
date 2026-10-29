use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use distill::CancellationToken;

const MAX_REPORT_FILES: usize = 4_096;
const MAX_REPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_REPORT_SCAN_ENTRIES: usize = 65_536;
const MAX_SETTINGS_FILE_BYTES: u64 = 1_048_576;
const UNKNOWN_PROJECT_DIRECTORY: &str = "\0";
const MAX_FAILURE_DETAILS: usize = 100;
const MAX_FAILURE_DETAIL_CHARS: usize = 512;
#[derive(Default)]
struct ReportBudget {
    files: usize,
    bytes: u64,
    entries: usize,
    project_directories: Option<BTreeMap<String, Option<PathBuf>>>,
    exceeded: bool,
}

impl ReportBudget {
    const fn reserve_entry(&mut self) -> bool {
        if self.exceeded {
            return false;
        }
        let Some(total_entries) = self.entries.checked_add(1) else {
            self.exceeded = true;
            return false;
        };
        if total_entries > MAX_REPORT_SCAN_ENTRIES {
            self.exceeded = true;
            return false;
        }
        self.entries = total_entries;
        true
    }

    const fn reserve(&mut self, bytes: u64) -> bool {
        if self.exceeded {
            return false;
        }
        let Some(total_files) = self.files.checked_add(1) else {
            self.exceeded = true;
            return false;
        };
        let Some(total_bytes) = self.bytes.checked_add(bytes) else {
            self.exceeded = true;
            return false;
        };
        if total_files > MAX_REPORT_FILES
            || bytes > MAX_REPORT_FILE_BYTES
            || total_bytes > MAX_REPORT_BYTES
        {
            self.exceeded = true;
            return false;
        }
        self.files = total_files;
        self.bytes = total_bytes;
        true
    }
}

#[derive(Debug, Clone, Copy)]
enum ReportCollectionError {
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GradleTestFailure {
    pub task_path: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct GradleTestSummary {
    pub observed: bool,
    pub executed: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub failures: Vec<GradleTestFailure>,
}

impl GradleTestSummary {
    fn merge(&mut self, other: &Self) {
        self.observed |= other.observed;
        self.executed = self.executed.saturating_add(other.executed);
        self.passed = self.passed.saturating_add(other.passed);
        self.failed = self.failed.saturating_add(other.failed);
        self.skipped = self.skipped.saturating_add(other.skipped);
        for failure in &other.failures {
            if self.failures.len() == MAX_FAILURE_DETAILS {
                break;
            }
            self.failures.push(failure.clone());
        }
    }

    pub const fn has_tests(&self) -> bool {
        self.observed || self.executed > 0 || self.skipped > 0
    }
}

pub fn summarize<'a>(
    root: &Path,
    task_paths: impl IntoIterator<Item = &'a str>,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Option<GradleTestSummary> {
    let root = fs::canonicalize(root).ok()?;
    let mut summary = GradleTestSummary::default();
    let mut budget = ReportBudget::default();
    for task_path in task_paths {
        if should_stop(deadline, cancellation) {
            return None;
        }
        let directory = find_report_directory(&root, task_path, &mut budget).ok()?;
        let Some(directory) = directory else {
            continue;
        };
        let task_summary =
            read_task_reports(&directory, task_path, &mut budget, deadline, cancellation).ok()?;
        if let Some(task_summary) = task_summary {
            summary.merge(&task_summary);
        }
    }
    summary.has_tests().then_some(summary)
}

fn report_directory(root: &Path, task_path: &str) -> Option<PathBuf> {
    let task_path = task_path.strip_prefix(':')?;
    let (project_path, task_name) = task_path.rsplit_once(':').unwrap_or(("", task_path));
    if !safe_component(task_name) {
        return None;
    }
    let mut directory = root.to_owned();
    for component in project_path.split(':').filter(|part| !part.is_empty()) {
        if !safe_component(component) {
            return None;
        }
        directory.push(component);
    }
    Some(directory.join("build").join("test-results").join(task_name))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':' | '\0'))
}

fn find_report_directory(
    root: &Path,
    task_path: &str,
    budget: &mut ReportBudget,
) -> Result<Option<PathBuf>, ReportCollectionError> {
    let Some(default_directory) = report_directory(root, task_path) else {
        return Ok(None);
    };
    let Some(task_path_without_root) = task_path.strip_prefix(':') else {
        return Ok(None);
    };
    let (project_path, task_name) = task_path_without_root
        .rsplit_once(':')
        .unwrap_or(("", task_path_without_root));
    if project_path.is_empty() {
        return canonical_report_directory(root, &default_directory);
    }
    if budget.project_directories.is_none() {
        budget.project_directories = Some(read_project_directories(root)?);
    }
    let Some(project_directories) = budget.project_directories.as_ref() else {
        return Err(ReportCollectionError::Incomplete);
    };
    match project_directories.get(project_path) {
        Some(Some(project_directory)) => {
            let expected = project_directory
                .join("build")
                .join("test-results")
                .join(task_name);
            canonical_report_directory(root, &expected)
        }
        Some(None) => Err(ReportCollectionError::Incomplete),
        None if project_directories.contains_key(UNKNOWN_PROJECT_DIRECTORY) => {
            Err(ReportCollectionError::Incomplete)
        }
        None => canonical_report_directory(root, &default_directory),
    }
}
fn canonical_report_directory(
    root: &Path,
    expected: &Path,
) -> Result<Option<PathBuf>, ReportCollectionError> {
    match fs::canonicalize(expected) {
        Ok(directory) if directory.starts_with(root) && directory.is_dir() => Ok(Some(directory)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ReportCollectionError::Incomplete),
    }
}

fn read_project_directories(
    root: &Path,
) -> Result<BTreeMap<String, Option<PathBuf>>, ReportCollectionError> {
    let settings_kotlin = root.join("settings.gradle.kts");
    let settings_groovy = root.join("settings.gradle");
    let settings = if settings_kotlin.exists() {
        Some(settings_kotlin)
    } else if settings_groovy.exists() {
        Some(settings_groovy)
    } else {
        None
    };
    let Some(settings) = settings else {
        return Ok(BTreeMap::new());
    };
    let settings = fs::canonicalize(settings).map_err(|_| ReportCollectionError::Incomplete)?;
    if !settings.starts_with(root) {
        return Err(ReportCollectionError::Incomplete);
    }
    let metadata = fs::metadata(&settings).map_err(|_| ReportCollectionError::Incomplete)?;
    if metadata.len() > MAX_SETTINGS_FILE_BYTES {
        return Err(ReportCollectionError::Incomplete);
    }
    let mut contents = String::new();
    let mut settings_file = File::open(settings)
        .map_err(|_| ReportCollectionError::Incomplete)?
        .take(MAX_SETTINGS_FILE_BYTES.saturating_add(1));
    settings_file
        .read_to_string(&mut contents)
        .map_err(|_| ReportCollectionError::Incomplete)?;
    let maximum_settings_bytes =
        usize::try_from(MAX_SETTINGS_FILE_BYTES).map_err(|_| ReportCollectionError::Incomplete)?;
    if contents.len() > maximum_settings_bytes {
        return Err(ReportCollectionError::Incomplete);
    }
    let mut project_directories = BTreeMap::new();
    let mut has_unresolved_project_directory = false;
    for line in contents.lines() {
        let Some((project_path, directory)) = parse_project_dir_assignment(line) else {
            has_unresolved_project_directory |=
                line.contains("projectDir") || line.contains("ProjectDir");
            continue;
        };
        let project_path = project_path.strip_prefix(':').unwrap_or(project_path);
        if project_path.is_empty() {
            continue;
        }
        let directory = directory.map(|directory| {
            let directory = Path::new(directory);
            if directory.is_absolute() {
                directory.to_owned()
            } else {
                root.join(directory)
            }
        });
        if let Some(previous) = project_directories.get(project_path) {
            if previous != &directory {
                drop(project_directories.insert(project_path.to_owned(), None));
            }
        } else {
            drop(project_directories.insert(project_path.to_owned(), directory));
        }
    }
    if has_unresolved_project_directory {
        drop(project_directories.insert(UNKNOWN_PROJECT_DIRECTORY.to_owned(), None));
    }
    Ok(project_directories)
}

fn parse_project_dir_assignment(line: &str) -> Option<(&str, Option<&str>)> {
    let remainder = line.trim().strip_prefix("project")?.trim_start();
    let remainder = remainder.strip_prefix('(')?.trim_start();
    let (project_path, remainder) = quoted_literal(remainder)?;
    let remainder = remainder.trim_start().strip_prefix(')')?.trim_start();
    let remainder = remainder.strip_prefix(".projectDir")?.trim_start();
    let expression = remainder.strip_prefix('=')?.trim_start();
    Some((project_path, project_directory_literal(expression)))
}

fn project_directory_literal(expression: &str) -> Option<&str> {
    let expression = expression.trim_start();
    let path = if let Some(arguments) = expression.strip_prefix("file") {
        let arguments = arguments.trim_start().strip_prefix('(')?.trim_start();
        project_directory_call(arguments)?
    } else {
        let expression = expression.strip_prefix("new ").unwrap_or(expression);
        let arguments = expression
            .strip_prefix("File")?
            .trim_start()
            .strip_prefix('(')?
            .trim_start();
        let (base, arguments) = arguments.split_once(',')?;
        if base.trim() != "rootDir" {
            return None;
        }
        project_directory_call(arguments.trim_start())?
    };
    (!path.contains('$')).then_some(path)
}

fn quoted_literal(input: &str) -> Option<(&str, &str)> {
    let quote = input.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    input.strip_prefix(quote)?.split_once(quote)
}
fn project_directory_call(input: &str) -> Option<&str> {
    let (path, remainder) = quoted_literal(input)?;
    let remainder = remainder.trim_start().strip_prefix(')')?.trim_start();
    let remainder = remainder
        .strip_prefix(';')
        .map_or(remainder, str::trim_start);
    let only_comment_or_empty = remainder.is_empty()
        || remainder.starts_with("//")
        || remainder
            .strip_prefix("/*")
            .and_then(|comment| comment.split_once("*/"))
            .is_some_and(|(_, suffix)| suffix.trim().is_empty());
    if !only_comment_or_empty {
        return None;
    }
    Some(path)
}

fn read_task_reports(
    directory: &Path,
    task_path: &str,
    budget: &mut ReportBudget,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<Option<GradleTestSummary>, ReportCollectionError> {
    if should_stop(deadline, cancellation) {
        return Err(ReportCollectionError::Incomplete);
    }
    let entries = fs::read_dir(directory).map_err(|_| ReportCollectionError::Incomplete)?;
    let mut summary = GradleTestSummary::default();
    let mut report_paths = Vec::new();
    for entry in entries {
        if should_stop(deadline, cancellation) || !budget.reserve_entry() {
            return Err(ReportCollectionError::Incomplete);
        }
        let entry = entry.map_err(|_| ReportCollectionError::Incomplete)?;
        let file_type = entry
            .file_type()
            .map_err(|_| ReportCollectionError::Incomplete)?;
        let path = entry.path();
        if path.extension().and_then(OsStr::to_str) != Some("xml") {
            continue;
        }
        if file_type.is_symlink() {
            return Err(ReportCollectionError::Incomplete);
        }
        if !file_type.is_file() {
            continue;
        }
        let bytes = entry
            .metadata()
            .map_err(|_| ReportCollectionError::Incomplete)?
            .len();
        if !budget.reserve(bytes) {
            return Err(ReportCollectionError::Incomplete);
        }
        report_paths.push(path);
    }
    sort_report_paths(&mut report_paths);
    if report_paths.is_empty() {
        return Ok(None);
    }
    for path in report_paths {
        if should_stop(deadline, cancellation) {
            return Err(ReportCollectionError::Incomplete);
        }
        let report = parse_report_file(&path, task_path, deadline, cancellation)
            .ok_or(ReportCollectionError::Incomplete)?;
        summary.merge(&report);
    }
    Ok(Some(summary))
}

fn sort_report_paths(paths: &mut [PathBuf]) {
    paths.sort_unstable();
}

fn parse_report_file(
    path: &Path,
    task_path: &str,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Option<GradleTestSummary> {
    let file = File::open(path).ok()?;
    parse_report(BufReader::new(file), task_path, deadline, cancellation)
}

fn parse_report(
    input: impl BufRead,
    task_path: &str,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Option<GradleTestSummary> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut root_is_testsuites = false;
    let mut summary = GradleTestSummary::default();
    let mut current_case = None;
    loop {
        if should_stop(deadline, cancellation) {
            return None;
        }
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Start(event) => {
                let name = event.name();
                if depth == 0 {
                    root_is_testsuites = name.as_ref() == "testsuites";
                }
                if name.as_ref() == "testsuite"
                    && (depth == 0 || (root_is_testsuites && depth == 1))
                {
                    summary.merge(&parse_suite_counts(&event)?);
                } else if name.as_ref() == "testcase" {
                    current_case = Some(parse_test_case_attributes(&event));
                } else if is_failure_element(name.as_ref())
                    && let Some(test_case) = current_case.as_mut()
                {
                    test_case.begin_failure(&event);
                }
                depth = depth.saturating_add(1);
            }
            Event::Empty(event) => {
                let name = event.name();
                if name.as_ref() == "testsuite"
                    && (depth == 0 || (root_is_testsuites && depth == 1))
                {
                    summary.merge(&parse_suite_counts(&event)?);
                } else if is_failure_element(name.as_ref())
                    && let Some(test_case) = current_case.as_mut()
                {
                    test_case.begin_failure(&event);
                    test_case.end_failure();
                }
            }
            Event::Text(event) => {
                if let Some(test_case) = current_case.as_mut().filter(|case| case.in_failure) {
                    let decoded = event.xml10_content();
                    let text = quick_xml::escape::unescape(decoded.as_ref()).ok()?;
                    test_case.keep_failure_text(&text);
                }
            }
            Event::End(event) => {
                let name = event.name();
                if is_failure_element(name.as_ref()) {
                    if let Some(test_case) = current_case.as_mut() {
                        test_case.end_failure();
                    }
                } else if name.as_ref() == "testcase"
                    && let Some(test_case) = current_case.take()
                {
                    record_test_failure(&mut summary, task_path, test_case);
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    summary.has_tests().then_some(summary)
}
fn is_failure_element(name: &str) -> bool {
    name == "failure" || name == "error"
}
#[derive(Default)]
struct ParsedTestCase {
    name: Option<String>,
    in_failure: bool,
    failure_message: Option<String>,
    failure_type: Option<String>,
    failure_text: Option<String>,
    reason: Option<String>,
}

impl ParsedTestCase {
    fn begin_failure(&mut self, event: &BytesStart<'_>) {
        if self.reason.is_some() || self.in_failure {
            return;
        }
        self.failure_message = event_attribute(event, "message")
            .filter(|message| !message.trim().is_empty())
            .map(|message| bounded_detail(&message));
        self.failure_type = event_attribute(event, "type")
            .filter(|kind| !kind.trim().is_empty())
            .map(|kind| bounded_detail(&kind));
        self.in_failure = true;
    }

    fn keep_failure_text(&mut self, text: &str) {
        if self.failure_message.is_some() || self.failure_text.is_some() {
            return;
        }
        if let Some(line) = text.lines().find(|line| !line.trim().is_empty()) {
            self.failure_text = Some(bounded_detail(line));
        }
    }

    fn end_failure(&mut self) {
        if self.reason.is_none() {
            self.reason = self
                .failure_message
                .take()
                .or_else(|| self.failure_text.take())
                .or_else(|| self.failure_type.take());
        }
        self.in_failure = false;
    }
}

fn parse_test_case_attributes(event: &BytesStart<'_>) -> ParsedTestCase {
    let class_name = event_attribute(event, "classname");
    let name = event_attribute(event, "name");
    let name = match (class_name.as_deref(), name.as_deref()) {
        (Some(class_name), Some(name)) if !class_name.is_empty() && !name.is_empty() => {
            Some(format!("{class_name}.{name}"))
        }
        (_, Some(name)) => Some(name.to_owned()),
        (Some(class_name), None) => Some(class_name.to_owned()),
        (None, None) => None,
    };
    ParsedTestCase {
        name: name.map(|name| bounded_detail(&name)),
        ..ParsedTestCase::default()
    }
}

fn event_attribute(event: &BytesStart<'_>, key: &str) -> Option<String> {
    event.attributes().flatten().find_map(|attribute| {
        if attribute.key.as_ref() != key {
            return None;
        }
        let value = quick_xml::escape::unescape(attribute.value.as_ref()).ok()?;
        Some(value.into_owned())
    })
}

fn record_test_failure(
    summary: &mut GradleTestSummary,
    task_path: &str,
    test_case: ParsedTestCase,
) {
    let Some(reason) = test_case.reason else {
        return;
    };
    if summary.failures.len() == MAX_FAILURE_DETAILS {
        return;
    }
    summary.failures.push(GradleTestFailure {
        task_path: task_path.to_owned(),
        name: test_case.name.unwrap_or_else(|| "<unknown>".to_owned()),
        reason,
    });
}

fn bounded_detail(value: &str) -> String {
    let mut compact = distill::compact_text(value);
    if let Some((index, _)) = compact.char_indices().nth(MAX_FAILURE_DETAIL_CHARS) {
        compact.truncate(index);
    }
    compact
}

fn parse_suite_counts(event: &BytesStart<'_>) -> Option<GradleTestSummary> {
    let mut tests = None;
    let mut failures = 0_u32;
    let mut errors = 0_u32;
    let mut skipped = 0_u32;
    for attribute in event.attributes() {
        let attribute = attribute.ok()?;
        let value = attribute.value.as_ref();
        match attribute.key.as_ref() {
            "tests" => tests = Some(value.parse::<u32>().ok()?),
            "failures" => failures = value.parse::<u32>().ok()?,
            "errors" => errors = value.parse::<u32>().ok()?,
            "skipped" => skipped = value.parse::<u32>().ok()?,
            _ => {}
        }
    }
    let tests = tests?;
    let skipped = skipped.min(tests);
    let executed = tests.saturating_sub(skipped);
    let failed = failures.saturating_add(errors).min(executed);
    Some(GradleTestSummary {
        observed: true,
        executed,
        passed: executed.saturating_sub(failed),
        failed,
        skipped,
        failures: Vec::new(),
    })
}

fn should_stop(deadline: Option<Instant>, cancellation: &CancellationToken) -> bool {
    cancellation.is_cancelled() || deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn parses_junit_suite_counts_including_errors_and_skips() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="Example" tests="4" skipped="1" failures="1" errors="1">
  <testcase name="passed"/>
  <testcase classname="Example" name="skipped"><skipped/></testcase>
  <testcase classname="Example" name="failed"><failure message="assertion failed"/></testcase>
  <testcase classname="Example" name="errored"><error message="java.lang.IllegalStateException: &lt;boom&gt;"/></testcase>
</testsuite>"#;
        let summary = parse_report(Cursor::new(xml), ":test", None, &CancellationToken::new())
            .expect("JUnit test summary");

        assert!(summary.has_tests());
        assert_eq!(summary.executed, 3);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.failures.len(), 2);
        assert_eq!(summary.failures[0].task_path, ":test");
        assert_eq!(summary.failures[0].name, "Example.failed");
        assert_eq!(summary.failures[0].reason, "assertion failed");
        assert_eq!(summary.failures[1].name, "Example.errored");
        assert_eq!(
            summary.failures[1].reason,
            "java.lang.IllegalStateException: <boom>"
        );
    }

    #[test]
    fn sums_child_suites_without_double_counting_container_totals() {
        let xml = r#"<testsuites tests="4" failures="1" skipped="1">
  <testsuite tests="2" failures="1" skipped="0"/>
  <testsuite tests="2" failures="0" skipped="1"/>
</testsuites>"#;
        let summary = parse_report(Cursor::new(xml), ":test", None, &CancellationToken::new())
            .expect("JUnit test summary");

        assert_eq!(summary.executed, 3);
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn rejects_task_paths_that_escape_the_project_root() {
        let root = Path::new("project");
        assert!(report_directory(root, ":app:testDebugUnitTest").is_some());
        assert!(report_directory(root, ":../outside:test").is_none());
    }
    #[test]
    fn report_budget_enforces_invocation_wide_file_and_byte_limits() {
        let mut file_budget = ReportBudget::default();
        for _ in 0..MAX_REPORT_FILES {
            assert!(file_budget.reserve(0));
        }
        assert!(!file_budget.reserve(0));
        assert!(file_budget.exceeded);

        let mut byte_budget = ReportBudget::default();
        for _ in 0..(MAX_REPORT_BYTES / MAX_REPORT_FILE_BYTES) {
            assert!(byte_budget.reserve(MAX_REPORT_FILE_BYTES));
        }
        assert!(!byte_budget.reserve(1));
        assert!(byte_budget.exceeded);
    }
    #[test]
    fn sorts_report_paths_for_deterministic_failure_selection() {
        let mut paths = vec![PathBuf::from("TEST-z.xml"), PathBuf::from("TEST-a.xml")];
        sort_report_paths(&mut paths);
        assert_eq!(
            paths,
            vec![PathBuf::from("TEST-a.xml"), PathBuf::from("TEST-z.xml")]
        );
    }
}
#[cfg(test)]
mod report_collection_regressions {
    use super::*;
    use std::error::Error;

    fn fixture_root(name: &str) -> Result<std::path::PathBuf, Box<dyn Error>> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-fixtures")
            .join(format!("gradle-reports-{name}-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn ensure(condition: bool, message: &str) -> Result<(), Box<dyn Error>> {
        if condition {
            Ok(())
        } else {
            Err(std::io::Error::other(message).into())
        }
    }

    #[test]
    fn omits_partial_summary_when_a_later_task_report_is_malformed() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("partial-report")?;
        let first = root.join("app/build/test-results/test");
        let second = root.join("lib/build/test-results/test");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        std::fs::write(
            first.join("TEST-app.xml"),
            r#"<testsuite tests="1"><testcase name="passed"/></testsuite>"#,
        )?;
        std::fs::write(second.join("TEST-lib.xml"), "<testsuite")?;

        let summary = summarize(
            &root,
            [":app:test", ":lib:test"],
            None,
            &CancellationToken::new(),
        );
        std::fs::remove_dir_all(&root)?;
        ensure(
            summary.is_none(),
            "malformed later report must omit all totals",
        )
    }

    #[test]
    fn finds_reports_for_a_remapped_gradle_project_directory() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("mapped-project")?;
        std::fs::write(
            root.join("settings.gradle"),
            "include(':app')\nproject(':app').projectDir = file('modules/app')\n",
        )?;
        let root_reports = root.join("app/build/test-results/test");
        std::fs::create_dir_all(&root_reports)?;
        std::fs::write(
            root_reports.join("TEST-root.xml"),
            r#"<testsuite tests="9"><testcase name="root"/></testsuite>"#,
        )?;
        let reports = root.join("modules/app/build/test-results/test");
        std::fs::create_dir_all(&reports)?;
        std::fs::write(
            reports.join("TEST-app.xml"),
            r#"<testsuite tests="1"><testcase name="passed"/></testsuite>"#,
        )?;

        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        let Some(summary) = summary else {
            return Err(std::io::Error::other("remapped project report was not found").into());
        };
        ensure(summary.executed == 1, "remapped report count was incorrect")
    }

    #[cfg(unix)]
    #[test]
    fn ignores_report_directory_symlinks_outside_the_project() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::symlink;

        let root = fixture_root("report-symlink-root")?;
        let outside = fixture_root("report-symlink-outside")?;
        let outside_reports = outside.join("test-results/test");
        std::fs::create_dir_all(&outside_reports)?;
        std::fs::write(
            outside_reports.join("TEST-secret.xml"),
            r#"<testsuite tests="1" failures="1"><testcase name="secret"><failure message="private detail"/></testcase></testsuite>"#,
        )?;
        let link = root.join("app/build/test-results/test");
        std::fs::create_dir_all(link.parent().ok_or("missing report parent")?)?;
        symlink(&outside_reports, &link)?;

        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        std::fs::remove_dir_all(&outside)?;
        ensure(summary.is_none(), "outside report symlink must not be read")
    }

    #[test]
    fn bounds_irrelevant_report_directory_entries() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("report-entry-budget")?;
        let reports = root.join("app/build/test-results/test");
        std::fs::create_dir_all(&reports)?;
        std::fs::write(reports.join("ignored.log"), "")?;
        let mut budget = ReportBudget::default();
        for _ in 0..MAX_REPORT_SCAN_ENTRIES {
            if !budget.reserve_entry() {
                return Err(std::io::Error::other("entry budget ended early").into());
            }
        }
        let result = read_task_reports(
            &reports,
            ":app:test",
            &mut budget,
            None,
            &CancellationToken::new(),
        );
        std::fs::remove_dir_all(&root)?;
        ensure(
            result.is_err() && budget.exceeded,
            "report entry limit was not enforced",
        )
    }
    #[test]
    fn cancellation_after_a_completed_task_omits_partial_summary() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("cancel-after-task")?;
        let first = root.join("app/build/test-results/test");
        let second = root.join("lib/build/test-results/test");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        std::fs::write(
            first.join("TEST-app.xml"),
            r#"<testsuite tests="1"><testcase name="passed"/></testsuite>"#,
        )?;
        std::fs::write(
            second.join("TEST-lib.xml"),
            r#"<testsuite tests="1"><testcase name="passed"/></testsuite>"#,
        )?;
        let cancellation = CancellationToken::new();
        let mut calls = 0;
        let task_paths = std::iter::from_fn(|| match calls {
            0 => {
                calls = 1;
                Some(":app:test")
            }
            1 => {
                calls = 2;
                cancellation.cancel();
                Some(":lib:test")
            }
            _ => None,
        });
        let summary = summarize(&root, task_paths, None, &cancellation);
        std::fs::remove_dir_all(&root)?;
        ensure(
            summary.is_none(),
            "cancelled collection must omit earlier totals",
        )
    }

    #[cfg(unix)]
    #[test]
    fn omits_summary_for_symlinked_xml_report_files() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::symlink;

        let root = fixture_root("xml-symlink-root")?;
        let outside = fixture_root("xml-symlink-outside")?;
        let reports = root.join("app/build/test-results/test");
        std::fs::create_dir_all(&reports)?;
        std::fs::write(
            reports.join("TEST-app.xml"),
            r#"<testsuite tests="1"><testcase name="passed"/></testsuite>"#,
        )?;
        let external_report = outside.join("TEST-private.xml");
        std::fs::write(
            &external_report,
            r#"<testsuite tests="1" failures="1"><testcase name="private"><failure message="secret"/></testcase></testsuite>"#,
        )?;
        symlink(external_report, reports.join("TEST-external.xml"))?;

        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        std::fs::remove_dir_all(&outside)?;
        ensure(
            summary.is_none(),
            "symlinked XML must not produce partial totals",
        )
    }

    #[test]
    fn does_not_use_root_reports_for_a_subproject_task() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("root-report-only")?;
        let reports = root.join("build/test-results/test");
        std::fs::create_dir_all(&reports)?;
        std::fs::write(
            reports.join("TEST-root.xml"),
            r#"<testsuite tests="1"><testcase name="root"/></testsuite>"#,
        )?;

        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        ensure(
            summary.is_none(),
            "root project reports must not satisfy subproject task paths",
        )
    }
    #[test]
    fn ignores_sibling_reports_without_a_project_mapping() -> Result<(), Box<dyn Error>> {
        let root = fixture_root("sibling-report-only")?;
        let reports = root.join("lib/build/test-results/test");
        std::fs::create_dir_all(&reports)?;
        std::fs::write(
            reports.join("TEST-lib.xml"),
            r#"<testsuite tests="1"><testcase name="lib"/></testsuite>"#,
        )?;

        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        ensure(
            summary.is_none(),
            "sibling reports must not be attributed to another project",
        )
    }

    #[test]
    fn omits_reports_when_a_project_directory_mapping_is_unresolved() -> Result<(), Box<dyn Error>>
    {
        let root = fixture_root("unresolved-project-map")?;
        std::fs::write(
            root.join("settings.gradle"),
            "include(':app')\nproject(':app').projectDir = new File(rootDir, 'modules/app').parentFile\n",
        )?;
        for (directory, count) in [
            ("app/build/test-results/test", 9),
            ("modules/build/test-results/test", 1),
        ] {
            let reports = root.join(directory);
            std::fs::create_dir_all(&reports)?;
            std::fs::write(
                reports.join("TEST-app.xml"),
                format!(r#"<testsuite tests="{count}"><testcase name="app"/></testsuite>"#),
            )?;
        }
        let summary = summarize(&root, [":app:test"], None, &CancellationToken::new());
        std::fs::remove_dir_all(&root)?;
        ensure(
            summary.is_none(),
            "unresolved project mapping must not select a stale report directory",
        )
    }
}
#[cfg(test)]
mod project_directory_expression_tests {
    use super::project_directory_literal;

    #[test]
    fn accepts_only_supported_complete_project_directory_expressions() {
        assert_eq!(
            project_directory_literal("file('modules/app')"),
            Some("modules/app")
        );
        assert_eq!(
            project_directory_literal("new File(rootDir, 'modules/app')"),
            Some("modules/app")
        );
        assert_eq!(
            project_directory_literal("new File(moduleDir, 'app')"),
            None
        );
        assert_eq!(
            project_directory_literal("new File(rootDir, 'modules/app').parentFile"),
            None
        );
        assert_eq!(
            project_directory_literal(
                "new File(rootDir, 'modules/app')/* mapping */.parentFile/* suffix */"
            ),
            None
        );
    }
}
