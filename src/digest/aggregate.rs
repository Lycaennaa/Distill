use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};

use super::model::{Diagnostic, FALLBACK_LINE_LIMIT, OutputLine, Severity, Stream};
use super::parser::{compact_text, parse_diagnostics};
use super::xcode::XcodeDigest;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LocationKey {
    line: Option<u32>,
    column: Option<u32>,
}

/// One source location and the number of exact duplicate observations there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagnosticLocation {
    line: Option<u32>,
    column: Option<u32>,
    count: usize,
}

impl DiagnosticLocation {
    #[must_use]
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    #[must_use]
    pub const fn column(&self) -> Option<u32> {
        self.column
    }

    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    severity: Severity,
    source: Option<String>,
    rule: Option<String>,
    code: Option<String>,
    message: String,
}

/// A deterministic group of diagnostics sharing semantic identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticGroup {
    severity: Severity,
    source: Option<String>,
    rule: Option<String>,
    code: Option<String>,
    message: String,
    locations: Vec<DiagnosticLocation>,
}

impl DiagnosticGroup {
    fn from_diagnostic(diagnostic: &Diagnostic) -> Self {
        Self {
            severity: diagnostic.severity(),
            source: diagnostic.source().map(str::to_owned),
            rule: diagnostic.rule().map(str::to_owned),
            code: diagnostic.code().map(str::to_owned),
            message: diagnostic.message().to_owned(),
            locations: Vec::new(),
        }
    }

    fn add_location(&mut self, diagnostic: &Diagnostic) {
        let key = LocationKey {
            line: diagnostic.line(),
            column: diagnostic.column(),
        };
        if let Some(location) = self
            .locations
            .iter_mut()
            .find(|location| location.line == key.line && location.column == key.column)
        {
            location.count = location.count.saturating_add(1);
        } else {
            self.locations.push(DiagnosticLocation {
                line: key.line,
                column: key.column,
                count: 1,
            });
            self.locations.sort_unstable();
        }
    }

    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    #[must_use]
    pub fn rule(&self) -> Option<&str> {
        self.rule.as_deref()
    }

    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn locations(&self) -> &[DiagnosticLocation] {
        &self.locations
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.locations.iter().fold(0_usize, |total, location| {
            total.saturating_add(location.count)
        })
    }

    /// Render without ANSI or multiline human text.
    #[must_use]
    pub fn render(&self) -> String {
        self.render_with_source(self.source.as_deref().unwrap_or("<unknown>"))
    }

    /// Render paths relative to `root` when the resolved source is inside it.
    #[must_use]
    pub fn render_relative_to(&self, root: &Path) -> String {
        let Some(source) = self.source.as_deref() else {
            return self.render_with_source("<unknown>");
        };
        let source_path = Path::new(source);
        let normalized_root = Self::lexical_normalize(root);
        let absolute_source = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            normalized_root.join(source_path)
        };
        let source = match absolute_source.canonicalize() {
            Ok(resolved) => match resolved.strip_prefix(&normalized_root) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) if source_path.is_absolute() => source.to_owned(),
                Err(_) => resolved.to_string_lossy().into_owned(),
            },
            Err(_) if source_path.is_absolute() => source.to_owned(),
            Err(_) => absolute_source.to_string_lossy().into_owned(),
        };
        self.render_with_source(&source)
    }

    fn render_with_source(&self, source: &str) -> String {
        let source = compact_text(source);
        let severity = self.severity.to_string();
        let message = compact_text(&self.message);
        let identifiers = render_identifiers(self.rule.as_deref(), self.code.as_deref());

        let location_text = if self.locations.len() == 1 {
            self.locations.first().map_or_else(String::new, |location| {
                location
                    .line
                    .map_or_else(String::new, |line| format!(":{line}"))
            })
        } else {
            let lines = self
                .locations
                .iter()
                .map(|location| {
                    location
                        .line
                        .map_or_else(|| "<unknown>".to_owned(), |line| line.to_string())
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(" (lines {lines})")
        };

        let duplicate_suffix = if self.locations.len() == 1 && self.count() > 1 {
            format!(" (x{})", self.count())
        } else {
            String::new()
        };

        format!("{source}: {severity}{location_text}{identifiers}: {message}{duplicate_suffix}")
    }
    /// Normalize dot segments lexically without resolving symlinks.
    fn lexical_normalize(path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if normalized.file_name().is_some() {
                        let _ = normalized.pop();
                    } else if !normalized.has_root() {
                        normalized.push(component.as_os_str());
                    }
                }
                Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                    normalized.push(component.as_os_str());
                }
            }
        }
        normalized
    }
}

/// Incrementally collected diagnostics and bounded fallback tails.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Digest {
    groups: BTreeMap<GroupKey, DiagnosticGroup>,
    stdout_tail: VecDeque<OutputLine>,
    stderr_tail: VecDeque<OutputLine>,
    xcode: XcodeDigest,
}

impl Digest {
    /// Consume one output line without retaining complete raw child output.
    pub fn ingest(&mut self, line: &OutputLine) {
        match line.stream() {
            Stream::Stdout => retain_tail(&mut self.stdout_tail, line),
            Stream::Stderr => retain_tail(&mut self.stderr_tail, line),
        }
        for diagnostic in parse_diagnostics(line.text()) {
            self.add_diagnostic(&diagnostic);
        }
        self.xcode.ingest(line.text());
    }

    /// Add an adapter-produced diagnostic to the same grouping policy.
    pub fn add_diagnostic(&mut self, diagnostic: &Diagnostic) {
        let key = GroupKey {
            severity: diagnostic.severity(),
            source: diagnostic.source().map(str::to_owned),
            rule: diagnostic.rule().map(str::to_owned),
            code: diagnostic.code().map(str::to_owned),
            message: diagnostic.message().to_owned(),
        };
        let group = self
            .groups
            .entry(key)
            .or_insert_with(|| DiagnosticGroup::from_diagnostic(diagnostic));
        group.add_location(diagnostic);
    }
    pub(crate) fn merge(&mut self, other: &Self) {
        for (key, incoming) in &other.groups {
            if let Some(group) = self.groups.get_mut(key) {
                for location in &incoming.locations {
                    if let Some(existing) = group.locations.iter_mut().find(|existing| {
                        existing.line == location.line && existing.column == location.column
                    }) {
                        existing.count = existing.count.saturating_add(location.count);
                    } else {
                        group.locations.push(location.clone());
                    }
                }
                group.locations.sort_unstable();
            } else {
                self.groups.insert(key.clone(), incoming.clone());
            }
        }
        self.xcode.merge(&other.xcode);
        append_tail(&mut self.stdout_tail, &other.stdout_tail, Stream::Stdout);
        append_tail(&mut self.stderr_tail, &other.stderr_tail, Stream::Stderr);
    }

    #[must_use]
    pub fn has_diagnostics(&self) -> bool {
        !self.groups.is_empty()
    }

    /// Return groups in the documented deterministic order.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<DiagnosticGroup> {
        let mut groups = self.groups.values().cloned().collect::<Vec<_>>();
        groups.sort_unstable_by(compare_groups);
        groups
    }

    #[must_use]
    pub const fn xcode(&self) -> &XcodeDigest {
        &self.xcode
    }

    /// Select at most 20 lines from the two bounded stream tails.
    #[must_use]
    pub fn fallback_lines(&self) -> Vec<OutputLine> {
        let mut candidates = self
            .stderr_tail
            .iter()
            .chain(self.stdout_tail.iter())
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|left, right| {
            right
                .ordinal()
                .cmp(&left.ordinal())
                .then_with(|| left.stream().rank().cmp(&right.stream().rank()))
        });
        candidates.truncate(FALLBACK_LINE_LIMIT);
        candidates.sort_unstable_by(|left, right| {
            left.stream()
                .rank()
                .cmp(&right.stream().rank())
                .then_with(|| left.ordinal().cmp(&right.ordinal()))
        });
        candidates
    }
}

fn retain_tail(tail: &mut VecDeque<OutputLine>, line: &OutputLine) {
    if tail.len() == FALLBACK_LINE_LIMIT {
        tail.pop_front();
    }
    tail.push_back(line.clone());
}
fn append_tail(
    destination: &mut VecDeque<OutputLine>,
    source: &VecDeque<OutputLine>,
    stream: Stream,
) {
    let offset = destination
        .back()
        .map_or(0, |line| line.ordinal().saturating_add(1));
    for line in source {
        let ordinal = offset.saturating_add(line.ordinal());
        retain_tail(
            destination,
            &OutputLine::new(stream, ordinal, line.text().to_owned()),
        );
    }
}

fn compare_groups(left: &DiagnosticGroup, right: &DiagnosticGroup) -> Ordering {
    left.severity
        .cmp(&right.severity)
        .then_with(|| {
            compact_optional(left.source.as_deref()).cmp(&compact_optional(right.source.as_deref()))
        })
        .then_with(|| first_line(left).cmp(&first_line(right)))
        .then_with(|| first_column(left).cmp(&first_column(right)))
        .then_with(|| left.rule.cmp(&right.rule))
        .then_with(|| left.code.cmp(&right.code))
        .then_with(|| compact_text(&left.message).cmp(&compact_text(&right.message)))
        .then_with(|| left.locations.cmp(&right.locations))
}

fn first_line(group: &DiagnosticGroup) -> u32 {
    group
        .locations
        .first()
        .and_then(|location| location.line)
        .unwrap_or(u32::MAX)
}

fn first_column(group: &DiagnosticGroup) -> u32 {
    group
        .locations
        .first()
        .and_then(|location| location.column)
        .unwrap_or(u32::MAX)
}

fn compact_optional(value: Option<&str>) -> String {
    value.map_or_else(String::new, compact_text)
}

fn render_identifiers(rule: Option<&str>, code: Option<&str>) -> String {
    let mut identifiers = String::new();
    if let Some(rule) = rule {
        identifiers.push_str(" [rule=");
        identifiers.push_str(&compact_text(rule));
        identifiers.push(']');
    }
    if let Some(code) = code {
        identifiers.push_str(" [code=");
        identifiers.push_str(&compact_text(code));
        identifiers.push(']');
    }
    identifiers
}
