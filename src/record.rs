use std::fmt;

/// Prefix record kinds reserved by the digest grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Plan,
    Diagnostic,
    Test,
    List,
    Action,
    Artifact,
    Result,
    Failure,
    Fallback,
    Log,
}

impl fmt::Display for RecordKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Plan => "PLAN",
            Self::Diagnostic => "DIAG",
            Self::Test => "TEST",
            Self::List => "LIST",
            Self::Action => "ACTION",
            Self::Artifact => "INFO",
            Self::Result => "RESULT",
            Self::Failure => "FAILURE",
            Self::Fallback => "FALLBACK",
            Self::Log => "LOG",
        })
    }
}

/// One stdout record. Payload escaping prevents raw control characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    kind: RecordKind,
    payload: String,
}

impl Record {
    pub fn new(kind: RecordKind, payload: impl Into<String>) -> Self {
        Self {
            kind,
            payload: payload.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RecordKind {
        self.kind
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!("{}: {}", self.kind, escape_field(&self.payload))
    }
}

/// Escape field data used in prefixed records.
#[must_use]
pub fn escape_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.extend(character.escape_default());
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_diagnostics_with_compact_prefix_and_escapes_controls() {
        let record = Record::new(RecordKind::Diagnostic, "line\nnext\t\u{1b}[31m");
        assert_eq!(record.render(), "DIAG: line\\nnext\\t\\u{1b}[31m");
    }

    #[test]
    fn renders_every_reserved_kind_without_distill_prefix() {
        let kinds = [
            RecordKind::Plan,
            RecordKind::Diagnostic,
            RecordKind::Test,
            RecordKind::List,
            RecordKind::Action,
            RecordKind::Artifact,
            RecordKind::Result,
            RecordKind::Failure,
            RecordKind::Fallback,
            RecordKind::Log,
        ];
        for kind in kinds {
            assert_eq!(
                Record::new(kind, "payload").render(),
                format!("{kind}: payload")
            );
        }
        assert_eq!(
            Record::new(RecordKind::Artifact, "payload").render(),
            "INFO: payload"
        );
    }
}
