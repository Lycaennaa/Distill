use std::fmt;

use super::parser::compact_text;

/// Section kinds emitted by `xcodebuild -list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ListSectionKind {
    Project,
    Workspace,
    Targets,
    Configurations,
    Schemes,
}

impl fmt::Display for ListSectionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::Targets => "targets",
            Self::Configurations => "configurations",
            Self::Schemes => "schemes",
        })
    }
}

/// One compact section parsed from Xcode's list output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSection {
    pub(super) kind: ListSectionKind,
    pub(super) values: Vec<String>,
}

impl ListSection {
    pub(super) const fn new(kind: ListSectionKind, values: Vec<String>) -> Self {
        Self { kind, values }
    }

    #[must_use]
    pub const fn kind(&self) -> ListSectionKind {
        self.kind
    }

    #[must_use]
    pub fn values(&self) -> &[String] {
        &self.values
    }
}

/// Incremental Xcode project and workspace list state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct XcodeDigest {
    list_sections: Vec<ListSection>,
    active_list_section: Option<ListSection>,
}

impl XcodeDigest {
    pub(super) fn ingest(&mut self, text: &str) {
        self.ingest_list_line(text);
    }

    pub(super) fn merge(&mut self, other: &Self) {
        self.finish_list_section();
        self.list_sections
            .extend(other.list_sections.iter().cloned());
        self.active_list_section
            .clone_from(&other.active_list_section);
    }

    fn ingest_list_line(&mut self, text: &str) {
        if let Some((kind, name)) = parse_list_identity(text) {
            self.finish_list_section();
            self.active_list_section = Some(ListSection::new(kind, vec![name]));
            return;
        }
        if let Some(kind) = parse_list_heading(text) {
            self.finish_list_section();
            self.active_list_section = Some(ListSection::new(kind, Vec::new()));
            return;
        }
        if let Some(entry) = parse_list_entry(text)
            && let Some(section) = self.active_list_section.as_mut()
        {
            section.values.push(entry);
        }
    }

    fn finish_list_section(&mut self) {
        if let Some(section) = self.active_list_section.take() {
            self.list_sections.push(section);
        }
    }

    #[must_use]
    pub fn list_sections(&self) -> Vec<ListSection> {
        let mut sections = self.list_sections.clone();
        if let Some(section) = self.active_list_section.as_ref() {
            sections.push(section.clone());
        }
        sections
    }
}

pub(super) fn parse_list_identity(value: &str) -> Option<(ListSectionKind, String)> {
    let value = compact_text(value);
    for (prefix, kind) in [
        ("Information about project \"", ListSectionKind::Project),
        ("Information about workspace \"", ListSectionKind::Workspace),
    ] {
        if let Some(rest) = value.strip_prefix(prefix)
            && let Some(name) = rest.strip_suffix("\":")
        {
            return Some((kind, name.to_owned()));
        }
    }
    None
}

pub(super) fn parse_list_heading(value: &str) -> Option<ListSectionKind> {
    match compact_text(value).as_str() {
        "Targets:" => Some(ListSectionKind::Targets),
        "Build Configurations:" | "Configurations:" => Some(ListSectionKind::Configurations),
        "Schemes:" => Some(ListSectionKind::Schemes),
        _ => None,
    }
}

pub(super) fn parse_list_entry(value: &str) -> Option<String> {
    if !value.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let entry = compact_text(value);
    if entry.is_empty() || entry.ends_with(':') {
        None
    } else {
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xcode_list_lines() {
        assert_eq!(
            parse_list_identity("Information about project \"Demo\":"),
            Some((ListSectionKind::Project, "Demo".to_owned()))
        );
        assert_eq!(
            parse_list_heading("    Schemes:"),
            Some(ListSectionKind::Schemes)
        );
        assert_eq!(parse_list_entry("        Demo"), Some("Demo".to_owned()));
    }

    #[test]
    fn merge_preserves_xcode_sections() {
        let mut earlier = XcodeDigest::default();
        earlier.ingest("Information about project \"Demo\":");
        let mut later = XcodeDigest::default();
        later.ingest("Information about workspace \"Demo\":");

        earlier.merge(&later);

        let mut sections = earlier.list_sections().into_iter();
        assert_eq!(
            sections.next().map(|section| section.kind()),
            Some(ListSectionKind::Project)
        );
        assert_eq!(
            sections.next().map(|section| section.kind()),
            Some(ListSectionKind::Workspace)
        );
    }
}
