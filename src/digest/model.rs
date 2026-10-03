use std::fmt;
/// Maximum number of fallback lines retained for each child stream.
pub const FALLBACK_LINE_LIMIT: usize = 20;

/// Identifies the child stream that produced one output line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stream {
    Stderr,
    Stdout,
}

impl Stream {
    pub(super) const fn rank(self) -> u8 {
        match self {
            Self::Stderr => 0,
            Self::Stdout => 1,
        }
    }
}

impl fmt::Display for Stream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stderr => "stderr",
            Self::Stdout => "stdout",
        })
    }
}

/// One newline-delimited child output item with a per-stream ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    stream: Stream,
    ordinal: u64,
    text: String,
}

impl OutputLine {
    #[must_use]
    pub fn new(stream: Stream, ordinal: u64, text: impl Into<String>) -> Self {
        Self {
            stream,
            ordinal,
            text: text.into(),
        }
    }

    #[must_use]
    pub const fn stream(&self) -> Stream {
        self.stream
    }

    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Diagnostic severity used by deterministic rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
    Info,
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
            Self::Help => "help",
            Self::Info => "info",
        })
    }
}

/// Parsed diagnostic before repeated locations are grouped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    severity: Severity,
    source: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    rule: Option<String>,
    code: Option<String>,
    message: String,
}

impl Diagnostic {
    #[must_use]
    pub fn new(
        severity: Severity,
        source: Option<String>,
        line: Option<u32>,
        column: Option<u32>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            source,
            line,
            column,
            rule: None,
            code: None,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn with_rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self
    }

    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
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
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    #[must_use]
    pub const fn column(&self) -> Option<u32> {
        self.column
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

    pub(super) fn append_message(&mut self, suffix: &str) {
        self.message.push(' ');
        self.message.push_str(suffix);
    }
}
