use super::model::{Diagnostic, Severity};
use super::parser::{compact_text, parse_diagnostic, strip_ansi};

/// Parsed counts from a supported test runner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestSummary {
    pub(super) observed: bool,
    pub(super) executed: u32,
    pub(super) passed: u32,
    pub(super) failed: u32,
    pub(super) skipped: u32,
}

impl TestSummary {
    #[must_use]
    pub const fn executed(&self) -> u32 {
        self.executed
    }

    #[must_use]
    pub const fn passed(&self) -> u32 {
        self.passed
    }

    #[must_use]
    pub const fn failed(&self) -> u32 {
        self.failed
    }

    #[must_use]
    pub const fn skipped(&self) -> u32 {
        self.skipped
    }

    #[must_use]
    pub const fn has_tests(&self) -> bool {
        self.observed || self.executed > 0 || self.skipped > 0
    }
}

/// One failed test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    pub(super) name: String,
    pub(super) source: Option<String>,
    pub(super) line: Option<u32>,
    pub(super) reason: Option<String>,
}

impl TestFailure {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    #[must_use]
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct TestDigest {
    active_test: Option<String>,
    test_summary: TestSummary,
    test_cases_observed: bool,
    pending_failure_diagnostics: Vec<TestFailure>,
    test_failures: Vec<TestFailure>,
}

impl TestDigest {
    pub(super) fn ingest(&mut self, text: &str) {
        self.ingest_test_line(text);
    }

    pub(super) fn merge(&mut self, other: &Self) {
        self.active_test.clone_from(&other.active_test);
        self.test_summary.observed |= other.test_summary.observed;
        self.test_summary.executed = self
            .test_summary
            .executed
            .saturating_add(other.test_summary.executed);
        self.test_summary.passed = self
            .test_summary
            .passed
            .saturating_add(other.test_summary.passed);
        self.test_summary.failed = self
            .test_summary
            .failed
            .saturating_add(other.test_summary.failed);
        self.test_summary.skipped = self
            .test_summary
            .skipped
            .saturating_add(other.test_summary.skipped);
        self.test_cases_observed |= other.test_cases_observed;
        self.pending_failure_diagnostics
            .extend(other.pending_failure_diagnostics.iter().cloned());
        self.test_failures
            .extend(other.test_failures.iter().cloned());
    }

    fn ingest_test_line(&mut self, text: &str) {
        if let Some(name) = parse_test_case_name(text) {
            self.active_test = Some(name);
        }
        if let Some(diagnostic) = parse_diagnostic(text)
            && diagnostic.severity() == Severity::Error
        {
            let name = self
                .active_test
                .clone()
                .or_else(|| diagnostic_test_name(&diagnostic));
            if let Some(name) = name {
                self.record_failure_diagnostic(name, &diagnostic);
            }
        }
        if let Some(test) = parse_test_case(text) {
            if !self.test_cases_observed {
                self.test_cases_observed = true;
                self.test_summary = TestSummary::default();
            }
            self.test_summary.observed = true;
            self.test_summary.executed = self.test_summary.executed.saturating_add(1);
            match test.outcome {
                TestOutcome::Passed => {
                    self.test_summary.passed = self.test_summary.passed.saturating_add(1);
                    self.active_test = None;
                }
                TestOutcome::Failed => {
                    self.test_summary.failed = self.test_summary.failed.saturating_add(1);
                    self.record_test_failure(test);
                }
                TestOutcome::Skipped => {
                    self.test_summary.skipped = self.test_summary.skipped.saturating_add(1);
                    self.active_test = None;
                }
            }
        }
        if let Some(summary) = parse_cargo_test_summary(text)
            && !self.test_cases_observed
        {
            self.test_summary.observed = true;
            self.test_summary.executed =
                self.test_summary.executed.saturating_add(summary.executed);
            self.test_summary.passed = self
                .test_summary
                .passed
                .saturating_add(summary.executed.saturating_sub(summary.failed));
            self.test_summary.failed = self.test_summary.failed.saturating_add(summary.failed);
            self.test_summary.skipped = self.test_summary.skipped.saturating_add(summary.skipped);
        } else if let Some(summary) = parse_test_summary(text)
            && !self.test_cases_observed
        {
            self.test_summary.observed = true;
            self.test_summary.executed = summary.executed;
            self.test_summary.failed = summary.failed;
            self.test_summary.skipped = summary.skipped;
            self.test_summary.passed = summary.executed.saturating_sub(summary.failed);
        }
    }

    fn record_failure_diagnostic(&mut self, name: String, diagnostic: &Diagnostic) {
        let detail = TestFailure {
            name,
            source: diagnostic.source().map(str::to_owned),
            line: diagnostic.line(),
            reason: Some(test_failure_reason(diagnostic)),
        };
        if let Some(failure) = self
            .pending_failure_diagnostics
            .iter_mut()
            .find(|failure| failure.name == detail.name)
        {
            merge_failure(failure, detail);
        } else if let Some(failure) = self
            .test_failures
            .iter_mut()
            .rev()
            .find(|failure| failure.name == detail.name)
        {
            merge_failure(failure, detail);
        } else {
            self.pending_failure_diagnostics.push(detail);
        }
    }

    fn record_test_failure(&mut self, test: ParsedTestCase) {
        let name = test.name;
        let Some(index) = self
            .pending_failure_diagnostics
            .iter()
            .rposition(|failure| failure.name == name)
        else {
            self.test_failures.push(TestFailure {
                name,
                source: None,
                line: None,
                reason: test.reason,
            });
            return;
        };
        let mut failure = self.pending_failure_diagnostics.remove(index);
        if failure.reason.is_none() {
            failure.reason = test.reason;
        }
        self.test_failures.push(failure);
    }

    #[must_use]
    pub(super) const fn test_summary(&self) -> &TestSummary {
        &self.test_summary
    }

    #[must_use]
    pub(super) fn test_failures(&self) -> &[TestFailure] {
        &self.test_failures
    }
}

fn merge_failure(existing: &mut TestFailure, detail: TestFailure) {
    if existing.source.is_none() {
        existing.source = detail.source;
    }
    if existing.line.is_none() {
        existing.line = detail.line;
    }
    if existing.reason.is_none() {
        existing.reason = detail.reason;
    }
}

fn test_failure_reason(diagnostic: &Diagnostic) -> String {
    let message = compact_text(diagnostic.message());
    message
        .split_once(": ")
        .map_or_else(|| message.clone(), |(_, reason)| reason.to_owned())
}

fn parse_test_case_name(value: &str) -> Option<String> {
    let value = compact_text(value);
    let rest = value.strip_prefix("Test Case '")?;
    Some(rest.split_once('\'')?.0.to_owned())
}

fn diagnostic_test_name(diagnostic: &Diagnostic) -> Option<String> {
    let message = compact_text(diagnostic.message());
    let start = message.find("-[")?;
    let rest = message.get(start..)?;
    let end = rest.find(']')?;
    Some(rest.get(..end.saturating_add(1))?.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTestCase {
    name: String,
    outcome: TestOutcome,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestOutcome {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedTestSummary {
    executed: u32,
    failed: u32,
    skipped: u32,
}

fn parse_test_case(value: &str) -> Option<ParsedTestCase> {
    let value = compact_text(value);
    let rest = value.strip_prefix("Test Case '")?;
    let (name, suffix) = rest.split_once('\'')?;
    let (outcome, marker) = if suffix.contains(" failed") {
        (TestOutcome::Failed, "failed")
    } else if suffix.contains(" skipped") {
        (TestOutcome::Skipped, "skipped")
    } else if suffix.contains(" passed") {
        (TestOutcome::Passed, "passed")
    } else {
        return None;
    };
    let reason = suffix
        .split_once(&format!("{marker}:"))
        .map(|(_, reason)| reason.trim().trim_end_matches('.').to_owned())
        .filter(|reason| !reason.is_empty());
    Some(ParsedTestCase {
        name: name.to_owned(),
        outcome,
        reason,
    })
}

fn parse_test_summary(value: &str) -> Option<ParsedTestSummary> {
    let value = compact_text(&strip_ansi(value));
    if !value.contains("Executed ") {
        return None;
    }
    let executed = number_before(&value, " tests").or_else(|| number_before(&value, " test"))?;
    let failed = number_before(&value, " failures")
        .or_else(|| number_before(&value, " failure"))
        .unwrap_or(0);
    let skipped = number_before(&value, " skipped").unwrap_or(0);
    Some(ParsedTestSummary {
        executed,
        failed,
        skipped,
    })
}

fn parse_cargo_test_summary(value: &str) -> Option<ParsedTestSummary> {
    let value = compact_text(&strip_ansi(value));
    let value = value.strip_prefix("test result: ")?;
    let passed = number_before(value, " passed")?;
    let failed = number_before(value, " failed")?;
    let skipped = number_before(value, " ignored")?;
    Some(ParsedTestSummary {
        executed: passed.saturating_add(failed),
        failed,
        skipped,
    })
}

fn number_before(value: &str, marker: &str) -> Option<u32> {
    let (prefix, _) = value.split_once(marker)?;
    let token = prefix.split_whitespace().last()?;
    let digits = token.trim_matches(|character: char| !character.is_ascii_digit());
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_swift_and_cargo_test_summaries() {
        let test = parse_test_case("Test Case '-[DemoTests testOne]' failed: bad assertion")
            .expect("test case");
        assert_eq!(test.outcome, TestOutcome::Failed);

        let swift = parse_test_summary(
            "Executed 3 tests, with 1 failure (0 unexpected) in 0.2 seconds (2 skipped)",
        )
        .expect("Swift test summary");
        assert_eq!(swift.executed, 3);
        assert_eq!(swift.failed, 1);
        assert_eq!(swift.skipped, 2);

        let cargo = parse_cargo_test_summary(
            "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.2s",
        )
        .expect("Cargo test summary");
        assert_eq!(cargo.executed, 2);
        assert_eq!(cargo.failed, 0);
        assert_eq!(cargo.skipped, 1);
    }

    #[test]
    fn aggregates_cargo_test_summaries() {
        let mut digest = TestDigest::default();
        digest.ingest(
            "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.2s",
        );
        digest.ingest(
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s",
        );

        let summary = digest.test_summary();
        assert_eq!(summary.executed(), 3);
        assert_eq!(summary.passed(), 2);
        assert_eq!(summary.failed(), 1);
        assert_eq!(summary.skipped(), 1);
    }

    #[test]
    fn associates_xctest_failure_diagnostics_with_test_locations() {
        let mut digest = TestDigest::default();
        digest.ingest(
            "/Users/jane/DemoTests.swift:12: error: -[DemoTests testOne] : XCTAssertEqual failed",
        );
        digest.ingest("Test Case '-[DemoTests testOne]' started.");
        digest.ingest("Test Case '-[DemoTests testOne]' failed (0.001 seconds).");

        let failure = digest
            .test_failures()
            .first()
            .expect("failure should parse");
        assert_eq!(failure.source(), Some("/Users/jane/DemoTests.swift"));
        assert_eq!(failure.line(), Some(12));
        assert_eq!(failure.reason(), Some("XCTAssertEqual failed"));
    }

    #[test]
    fn merge_preserves_test_counts() {
        let mut earlier = TestDigest::default();
        earlier.ingest("Executed 2 tests, with 0 failures (0 unexpected) in 1.0 seconds");
        let mut later = TestDigest::default();
        later.ingest("Executed 3 tests, with 1 failure (0 unexpected) in 1.0 seconds");

        earlier.merge(&later);

        let summary = earlier.test_summary();
        assert_eq!(summary.executed(), 5);
        assert_eq!(summary.passed(), 4);
        assert_eq!(summary.failed(), 1);
    }
}
