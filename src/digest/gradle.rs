use std::collections::BTreeSet;

use super::parser::{compact_text, strip_ansi};
use crate::gradle::is_gradle_test_task_name;

const MAX_FAILED_TASKS: usize = 32;
const MAX_FAILURE_REASON_CHARS: usize = 512;
const MAX_GRADLE_TEST_TASKS: usize = 4_096;
const MAX_GRADLE_TASK_PATH_BYTES: usize = 512;

/// A Gradle build failure summarized from the wrapper's output.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GradleBuildFailure {
    failed_tasks: Vec<String>,
    reason: Option<String>,
}

impl GradleBuildFailure {
    #[must_use]
    pub fn failed_tasks(&self) -> &[String] {
        &self.failed_tasks
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn add_task(&mut self, task: &str) {
        if task.len() > MAX_GRADLE_TASK_PATH_BYTES
            || self.failed_tasks.iter().any(|existing| existing == task)
        {
            return;
        }
        if self.failed_tasks.len() < MAX_FAILED_TASKS {
            self.failed_tasks.push(task.to_owned());
        } else if self
            .failed_tasks
            .last()
            .is_some_and(|largest| task < largest.as_str())
        {
            self.failed_tasks.pop();
            self.failed_tasks.push(task.to_owned());
        } else {
            return;
        }
        self.failed_tasks.sort_unstable();
    }

    fn keep_reason(&mut self, reason: &str) {
        if self.reason.is_some() {
            return;
        }
        self.reason = Some(bounded_compact_text(reason));
    }

    fn append_reason(&mut self, reason: &str) {
        let Some(existing) = self.reason.as_mut() else {
            self.keep_reason(reason);
            return;
        };
        if existing.is_empty() || existing.len() >= MAX_FAILURE_REASON_CHARS {
            return;
        }
        existing.push_str("; ");
        existing.push_str(&bounded_compact_text(reason));
        truncate_chars(existing, MAX_FAILURE_REASON_CHARS);
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct GradleDigest {
    test_tasks: BTreeSet<String>,
    test_tasks_incomplete: bool,
    build_failure: Option<GradleBuildFailure>,
    reading_failure_reason: bool,
    awaiting_task_cause: bool,
}

impl GradleDigest {
    pub(super) fn ingest(&mut self, line: &str) {
        let stripped;
        let line = if line.contains('\u{1b}') {
            stripped = strip_ansi(line);
            stripped.as_str()
        } else {
            line
        };
        let line = line.trim();

        if let Some((task, status)) = parse_task_line(line) {
            if is_test_task(task) && !matches!(status, Some("SKIPPED" | "NO-SOURCE")) {
                self.add_test_task(task);
            }
            if status == Some("FAILED") {
                self.failure().add_task(task);
            }
        }

        if line.starts_with("FAILURE: Build failed") {
            self.failure();
        }
        if line.starts_with("BUILD FAILED") {
            self.failure();
            self.reading_failure_reason = false;
            return;
        }
        if line == "* What went wrong:" {
            self.failure();
            self.reading_failure_reason = true;
            self.awaiting_task_cause = false;
            return;
        }
        if self.reading_failure_reason {
            self.ingest_failure_reason(line);
        }
    }

    pub(super) fn merge(&mut self, other: &Self) {
        if other.test_tasks_incomplete {
            self.mark_test_tasks_incomplete();
        } else {
            for task in &other.test_tasks {
                self.add_test_task(task);
            }
        }
        let Some(other_failure) = &other.build_failure else {
            return;
        };
        let failure = self.failure();
        for task in &other_failure.failed_tasks {
            failure.add_task(task);
        }
        if failure.reason.is_none() {
            failure.reason.clone_from(&other_failure.reason);
        }
    }

    pub(super) const fn test_tasks(&self) -> &BTreeSet<String> {
        &self.test_tasks
    }

    fn add_test_task(&mut self, task: &str) {
        if self.test_tasks_incomplete {
            return;
        }
        if task.len() > MAX_GRADLE_TASK_PATH_BYTES
            || (!self.test_tasks.contains(task) && self.test_tasks.len() == MAX_GRADLE_TEST_TASKS)
        {
            self.mark_test_tasks_incomplete();
        } else {
            self.test_tasks.insert(task.to_owned());
        }
    }

    fn mark_test_tasks_incomplete(&mut self) {
        self.test_tasks.clear();
        self.test_tasks_incomplete = true;
    }

    pub(super) const fn build_failure(&self) -> Option<&GradleBuildFailure> {
        self.build_failure.as_ref()
    }

    fn failure(&mut self) -> &mut GradleBuildFailure {
        self.build_failure
            .get_or_insert_with(GradleBuildFailure::default)
    }

    fn ingest_failure_reason(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        if line.starts_with("* ") || line.starts_with("BUILD ") || line.starts_with("FAILURE:") {
            self.reading_failure_reason = false;
            return;
        }

        let reason = line.trim_start_matches('>').trim();
        if reason.is_empty() {
            return;
        }
        if let Some(task) = parse_execution_failed_task(reason) {
            self.failure().add_task(task);
            self.awaiting_task_cause = true;
            return;
        }

        let awaiting_task_cause = self.awaiting_task_cause;
        let failure = self.failure();
        if failure.reason.is_none() || awaiting_task_cause {
            failure.keep_reason(reason);
            self.awaiting_task_cause = false;
        } else if line.starts_with('>') {
            failure.append_reason(reason);
        }
    }
}

fn parse_task_line(line: &str) -> Option<(&str, Option<&str>)> {
    let rest = line.strip_prefix("> Task ")?;
    let mut parts = rest.split_whitespace();
    let task = parts.next()?;
    if !task.starts_with(':') {
        return None;
    }
    Some((task, parts.next()))
}

fn is_test_task(task_path: &str) -> bool {
    task_path
        .rsplit(':')
        .next()
        .is_some_and(is_gradle_test_task_name)
}

fn parse_execution_failed_task(reason: &str) -> Option<&str> {
    let rest = reason.strip_prefix("Execution failed for task '")?;
    let (task, _) = rest.split_once('\'')?;
    task.starts_with(':').then_some(task)
}

fn bounded_compact_text(value: &str) -> String {
    let mut compact = compact_text(value);
    truncate_chars(&mut compact, MAX_FAILURE_REASON_CHARS);
    compact
}

fn truncate_chars(value: &mut String, max_chars: usize) {
    if let Some((index, _)) = value.char_indices().nth(max_chars) {
        value.truncate(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_gradle_failed_task_and_nested_cause() {
        let mut digest = GradleDigest::default();
        for line in [
            "> Task :app:compileDebugKotlin FAILED",
            "FAILURE: Build failed with an exception.",
            "* What went wrong:",
            "Execution failed for task ':app:compileDebugKotlin'.",
            "> Unresolved reference: missingSymbol",
            "* Try:",
            "BUILD FAILED in 2s",
        ] {
            digest.ingest(line);
        }

        let failure = digest.build_failure().expect("Gradle build failure");
        assert_eq!(failure.failed_tasks(), &[":app:compileDebugKotlin"]);
        assert_eq!(
            failure.reason(),
            Some("Unresolved reference: missingSymbol")
        );
    }

    #[test]
    fn captures_build_configuration_failure_without_a_task_path() {
        let mut digest = GradleDigest::default();
        for line in [
            "FAILURE: Build failed with an exception.",
            "* What went wrong:",
            "Task 'missingTask' not found in root project 'Example'.",
            "* Try:",
            "BUILD FAILED in 1s",
        ] {
            digest.ingest(line);
        }

        let failure = digest.build_failure().expect("Gradle build failure");
        assert_eq!(failure.failed_tasks(), Vec::<String>::new().as_slice());
        assert_eq!(
            failure.reason(),
            Some("Task 'missingTask' not found in root project 'Example'.")
        );
    }

    #[test]
    fn captures_only_observed_gradle_test_tasks() {
        let mut digest = GradleDigest::default();
        for line in [
            "> Task :app:testDebugUnitTest UP-TO-DATE",
            "> Task :app:test SKIPPED",
            "> Task :app:compileDebugKotlin",
        ] {
            digest.ingest(line);
        }

        assert_eq!(
            digest
                .test_tasks()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![":app:testDebugUnitTest"]
        );
    }
    #[test]
    fn bounds_observed_gradle_test_tasks_and_path_lengths() {
        let mut digest = GradleDigest::default();
        for index in 0..MAX_GRADLE_TEST_TASKS {
            digest.ingest(&format!("> Task :project:test{index}Test"));
        }
        assert_eq!(digest.test_tasks().len(), MAX_GRADLE_TEST_TASKS);

        digest.ingest("> Task :project:testOverflowTest");
        assert_eq!(digest.test_tasks(), &BTreeSet::new());

        let mut long_path = GradleDigest::default();
        long_path.ingest(&format!(
            "> Task :{}Test",
            "x".repeat(MAX_GRADLE_TASK_PATH_BYTES)
        ));
        assert_eq!(long_path.test_tasks(), &BTreeSet::new());
    }
    #[test]
    fn sorts_bounded_failed_task_names_independently_of_output_order() {
        let task_paths = (0..=MAX_FAILED_TASKS)
            .map(|index| format!(":project:task{index:02}"))
            .collect::<Vec<_>>();

        let mut forward = GradleDigest::default();
        for task in &task_paths {
            forward.ingest(&format!("> Task {task} FAILED"));
        }
        let mut reverse = GradleDigest::default();
        for task in task_paths.iter().rev() {
            reverse.ingest(&format!("> Task {task} FAILED"));
        }

        let forward = forward.build_failure().expect("failed tasks");
        let reverse = reverse.build_failure().expect("failed tasks");
        assert_eq!(forward.failed_tasks(), reverse.failed_tasks());
        assert_eq!(forward.failed_tasks().len(), MAX_FAILED_TASKS);
        assert_eq!(
            forward.failed_tasks().first().map(String::as_str),
            Some(":project:task00")
        );
        assert_eq!(
            forward.failed_tasks().last().map(String::as_str),
            Some(":project:task31")
        );
    }
}
