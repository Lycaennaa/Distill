use serde_json::{Map, Value};

use crate::digest::model::{Diagnostic, Severity};

/// Remove terminal escape sequences before digest rendering.
#[must_use]
pub fn strip_ansi(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            result.push(character);
            continue;
        }

        let Some(introducer) = characters.next() else {
            break;
        };
        match introducer {
            '[' => skip_csi(&mut characters),
            ']' => skip_osc(&mut characters),
            _ => {}
        }
    }
    result
}

fn skip_csi<I>(characters: &mut I)
where
    I: Iterator<Item = char>,
{
    for character in characters {
        if ('@'..='~').contains(&character) {
            break;
        }
    }
}

fn skip_osc<I>(characters: &mut I)
where
    I: Iterator<Item = char>,
{
    let mut escaped = false;
    for character in characters {
        if character == '\u{7}' || (escaped && character == '\\') {
            break;
        }
        escaped = character == '\u{1b}';
    }
}

/// Collapse human diagnostic text to one line and remove terminal escapes.
#[must_use]
pub fn compact_text(value: &str) -> String {
    strip_ansi(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse one or more compiler, linter, Cargo, or Clippy diagnostics.
#[must_use]
pub fn parse_diagnostics(value: &str) -> Vec<Diagnostic> {
    let value = strip_ansi(value);
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if let Some(diagnostics) = parse_json_diagnostics(value) {
        return diagnostics;
    }
    parse_text_diagnostic(value).into_iter().collect()
}

/// Parse the first diagnostic on a line.
#[must_use]
pub fn parse_diagnostic(value: &str) -> Option<Diagnostic> {
    parse_diagnostics(value).into_iter().next()
}

fn parse_text_diagnostic(value: &str) -> Option<Diagnostic> {
    for (token, severity) in severity_tokens() {
        if let Some(message) = value.strip_prefix(token) {
            return Some(build_diagnostic(
                severity,
                None,
                None,
                None,
                message.trim(),
                None,
            ));
        }
        if let Some(diagnostic) = parse_code_marker(value, token, severity) {
            return Some(diagnostic);
        }
        let spaced_marker = format!(": {token}");
        if let Some((source, message)) = value.split_once(&spaced_marker) {
            let (source, line, column) = parse_location(source);
            return Some(build_diagnostic(
                severity, source, line, column, message, None,
            ));
        }
        let marker = format!(":{token}");
        if let Some((source, message)) = value.split_once(&marker) {
            let (source, line, column) = parse_location(source);
            return Some(build_diagnostic(
                severity, source, line, column, message, None,
            ));
        }
    }
    None
}

const fn severity_tokens() -> [(&'static str, Severity); 6] {
    [
        ("fatal error:", Severity::Error),
        ("error:", Severity::Error),
        ("warning:", Severity::Warning),
        ("note:", Severity::Note),
        ("help:", Severity::Help),
        ("info:", Severity::Info),
    ]
}

fn parse_code_marker(value: &str, token: &str, severity: Severity) -> Option<Diagnostic> {
    let word = token.trim_end_matches(':');
    let prefix = format!("{word}[");
    if let Some(rest) = value.strip_prefix(&prefix)
        && let Some((code, message)) = rest.split_once("]:")
    {
        return Some(build_diagnostic(
            severity,
            None,
            None,
            None,
            message,
            Some(code),
        ));
    }

    for marker in [format!(": {prefix}"), format!(":{prefix}")] {
        if let Some((source, rest)) = value.split_once(&marker)
            && let Some((code, message)) = rest.split_once("]:")
        {
            let (source, line, column) = parse_location(source);
            return Some(build_diagnostic(
                severity,
                source,
                line,
                column,
                message,
                Some(code),
            ));
        }
    }
    None
}

fn build_diagnostic(
    severity: Severity,
    source: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
    message: &str,
    code: Option<&str>,
) -> Diagnostic {
    let (message, rule) = extract_rule(message);
    let mut diagnostic = Diagnostic::new(severity, source, line, column, message);
    if let Some(rule) = rule {
        diagnostic = diagnostic.with_rule(rule);
    }
    if let Some(code) = code {
        if code.starts_with("clippy::") {
            diagnostic = diagnostic.with_rule(code.trim());
        } else {
            diagnostic = diagnostic.with_code(code.trim());
        }
    }
    diagnostic
}

fn extract_rule(message: &str) -> (String, Option<String>) {
    let message = message.trim();
    if let Some(without_close) = message.strip_suffix(']')
        && let Some((clean, candidate)) = without_close.rsplit_once('[')
        && is_rule_identifier(candidate)
    {
        return (clean.trim().to_owned(), Some(candidate.to_owned()));
    }
    if let Some(without_close) = message.strip_suffix(')')
        && let Some((clean, candidate)) = without_close.rsplit_once('(')
        && is_rule_identifier(candidate)
    {
        return (clean.trim().to_owned(), Some(candidate.to_owned()));
    }
    (message.to_owned(), None)
}

fn is_rule_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
        && (value.starts_with("clippy::")
            || value.contains('_')
            || value.contains('-')
            || value.contains("::")
            || value
                .chars()
                .all(|character| !character.is_ascii_uppercase()))
}

fn parse_location(value: &str) -> (Option<String>, Option<u32>, Option<u32>) {
    let value = value.trim();
    let Some((prefix, last)) = value.rsplit_once(':') else {
        return (normalize_source(value), None, None);
    };
    let Some(last_number) = last.trim().parse::<u32>().ok() else {
        return (normalize_source(value), None, None);
    };
    if let Some((source, line)) = prefix.rsplit_once(':')
        && let Ok(line_number) = line.trim().parse::<u32>()
    {
        return (
            normalize_source(source),
            Some(line_number),
            Some(last_number),
        );
    }
    (normalize_source(prefix), Some(last_number), None)
}

fn normalize_source(value: &str) -> Option<String> {
    let value = value.trim().replace('\\', "/");
    if value.is_empty() {
        None
    } else {
        Some(value.strip_prefix("./").unwrap_or(&value).to_owned())
    }
}

fn parse_json_diagnostics(value: &str) -> Option<Vec<Diagnostic>> {
    let first = value.chars().next()?;
    if first != '{' && first != '[' {
        return None;
    }
    let json = serde_json::from_str::<Value>(value).ok()?;
    let diagnostics = match json {
        Value::Object(object) => parse_json_object(&object).into_iter().collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| value.as_object().and_then(parse_swiftlint_object))
            .collect(),
        _ => Vec::new(),
    };
    Some(diagnostics)
}

fn parse_json_object(object: &Map<String, Value>) -> Option<Diagnostic> {
    if object.get("reason").and_then(Value::as_str) == Some("compiler-message") {
        return parse_cargo_object(object);
    }
    parse_swiftlint_object(object)
}

fn parse_cargo_object(object: &Map<String, Value>) -> Option<Diagnostic> {
    let message = object.get("message")?.as_object()?;
    let severity = parse_severity(message.get("level")?.as_str()?)?;
    let text = message
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| message.get("rendered").and_then(Value::as_str))?;
    let (source, line, column) = parse_primary_span(message.get("spans"));
    let mut diagnostic = build_diagnostic(severity, source, line, column, text, None);
    if let Some(code) = message
        .get("code")
        .and_then(Value::as_object)
        .and_then(|code| code.get("code"))
        .and_then(Value::as_str)
    {
        if code.starts_with("clippy::") {
            diagnostic = diagnostic.with_rule(code);
        } else {
            diagnostic = diagnostic.with_code(code);
        }
    }
    append_children(&mut diagnostic, message.get("children"));
    Some(diagnostic)
}

fn parse_primary_span(spans: Option<&Value>) -> (Option<String>, Option<u32>, Option<u32>) {
    let Some(spans) = spans.and_then(Value::as_array) else {
        return (None, None, None);
    };
    let span = spans
        .iter()
        .find(|span| {
            span.as_object()
                .and_then(|span| span.get("is_primary"))
                .and_then(Value::as_bool)
                == Some(true)
        })
        .or_else(|| spans.first());
    let Some(span) = span.and_then(Value::as_object) else {
        return (None, None, None);
    };
    (
        span.get("file_name")
            .and_then(Value::as_str)
            .and_then(normalize_source),
        json_u32(span.get("line_start")),
        json_u32(span.get("column_start")),
    )
}

fn append_children(diagnostic: &mut Diagnostic, children: Option<&Value>) {
    let Some(children) = children.and_then(Value::as_array) else {
        return;
    };
    let mut details = Vec::new();
    for child in children {
        append_child_detail(child, &mut details);
    }
    if details.is_empty() {
        return;
    }
    diagnostic.append_message(&format!("({})", details.join("; ")));
}

fn append_child_detail(value: &Value, details: &mut Vec<String>) {
    let Some(child) = value.as_object() else {
        return;
    };
    if let Some(message) = child.get("message").and_then(Value::as_str) {
        let level = child.get("level").and_then(Value::as_str).unwrap_or("note");
        details.push(format!("{level}: {}", compact_text(message)));
    }
    if let Some(children) = child.get("children").and_then(Value::as_array) {
        for child in children {
            append_child_detail(child, details);
        }
    }
}

fn parse_swiftlint_object(object: &Map<String, Value>) -> Option<Diagnostic> {
    let severity = parse_severity(object.get("severity")?.as_str()?)?;
    let text = object
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| object.get("message").and_then(Value::as_str))?;
    let source = object
        .get("file")
        .and_then(Value::as_str)
        .or_else(|| object.get("file_path").and_then(Value::as_str))
        .and_then(normalize_source);
    let mut diagnostic = build_diagnostic(
        severity,
        source,
        json_u32(object.get("line")),
        json_u32(object.get("character").or_else(|| object.get("column"))),
        text,
        None,
    );
    if let Some(rule) = object
        .get("rule_id")
        .and_then(Value::as_str)
        .or_else(|| object.get("rule").and_then(Value::as_str))
    {
        diagnostic = diagnostic.with_rule(rule);
    }
    Some(diagnostic)
}

fn parse_severity(value: &str) -> Option<Severity> {
    match value {
        "error" | "failure" => Some(Severity::Error),
        "warning" => Some(Severity::Warning),
        "note" => Some(Severity::Note),
        "help" => Some(Severity::Help),
        "info" => Some(Severity::Info),
        _ => None,
    }
}

fn json_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compiler_and_lint_identifiers() {
        let compiler = parse_diagnostic("src/lib.rs:4:8: error[E0308]: mismatch")
            .expect("compiler diagnostic");
        assert_eq!(compiler.code(), Some("E0308"));
        assert_eq!(compiler.line(), Some(4));
        let fatal = parse_diagnostic("src/foo.c:4:2: fatal error: missing.h file not found")
            .expect("fatal compiler diagnostic");
        assert_eq!(fatal.severity(), Severity::Error);
        assert_eq!(fatal.source(), Some("src/foo.c"));
        assert_eq!(fatal.line(), Some(4));
        assert_eq!(fatal.column(), Some(2));
        let lint = parse_diagnostic("Sources/Foo.swift:9:3: warning: use let (prefer_let)")
            .expect("lint diagnostic");
        assert_eq!(lint.rule(), Some("prefer_let"));
        let todo = parse_diagnostic("Sources/Foo.swift:2:1: warning: avoid TODO (todo)")
            .expect("SwiftLint rule");
        assert_eq!(todo.rule(), Some("todo"));
    }

    #[test]
    fn parses_cargo_json_with_primary_span_and_children() {
        let line = r#"{"reason":"compiler-message","message":{"message":"mismatch","level":"error","code":{"code":"E0308"},"spans":[{"file_name":"src/lib.rs","line_start":4,"column_start":8,"is_primary":true}],"children":[{"message":"try this","level":"help","spans":[]}]}}"#;
        let diagnostic = parse_diagnostic(line).expect("cargo diagnostic");
        assert_eq!(diagnostic.source(), Some("src/lib.rs"));
        assert_eq!(diagnostic.code(), Some("E0308"));
        assert!(diagnostic.message().contains("help: try this"));
    }

    #[test]
    fn parses_swiftlint_json_array() {
        let line = r#"[{"file":"Sources/Foo.swift","line":9,"character":3,"reason":"use let","rule_id":"prefer_let","severity":"warning"}]"#;
        let diagnostics = parse_diagnostics(line);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics.first().and_then(Diagnostic::rule),
            Some("prefer_let")
        );
    }
}
