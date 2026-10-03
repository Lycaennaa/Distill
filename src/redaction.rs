use std::fmt;

use crate::digest::strip_ansi;

/// Locked policy labels for captured text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionPolicy {
    BestEffort,
    XcodeMandatory,
}

impl fmt::Display for RedactionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BestEffort => "best_effort",
            Self::XcodeMandatory => "xcode_mandatory",
        })
    }
}

/// Sanitize captured or wrapper-generated text under one output policy.
#[must_use]
pub fn redact(value: &str, policy: RedactionPolicy) -> String {
    let value = strip_ansi(value);
    let value = match policy {
        RedactionPolicy::BestEffort => redact_best_effort(&value),
        RedactionPolicy::XcodeMandatory => redact_xcode(&value),
    };
    redact_controls(&value)
}

fn redact_best_effort(value: &str) -> String {
    let value = replace_labeled_values(
        value,
        &[
            "password", "passwd", "secret", "token", "api_key", "api-key",
        ],
        "[REDACTED]",
        false,
    );
    let value = replace_labeled_values(&value, &["authorization"], "[REDACTED]", true);
    replace_spans(&value, &email_spans(&value), "[EMAIL]")
}

fn redact_xcode(value: &str) -> String {
    let value = replace_labeled_values(
        value,
        &["CODE_SIGN_IDENTITY", "Signing Identity", "certificate"],
        "[SIGNING_IDENTITY]",
        true,
    );
    let value = replace_labeled_values(
        &value,
        &["DEVELOPMENT_TEAM", "TEAM_ID", "TEAM ID", "team-id"],
        "[TEAM_ID]",
        false,
    );
    let value = replace_labeled_values(
        &value,
        &[
            "PROVISIONING_PROFILE_SPECIFIER",
            "PROVISIONING_PROFILE",
            "Provisioning Profile",
        ],
        "[PROFILE_ID]",
        true,
    );
    redact_best_effort(&value)
}

fn replace_labeled_values(
    value: &str,
    labels: &[&str],
    replacement: &str,
    rest_of_line: bool,
) -> String {
    let lower = value.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut search_from = 0_usize;
    while let Some((label_start, label_end)) = find_label(&lower, value, labels, search_from) {
        let Some((value_start, value_end)) = labeled_value_bounds(value, label_end, rest_of_line)
        else {
            search_from = label_end;
            continue;
        };
        if value_start < value_end {
            spans.push((value_start, value_end));
        }
        search_from = value_end.max(label_start.saturating_add(1));
    }
    replace_spans(value, &spans, replacement)
}

fn find_label(
    lower: &str,
    original: &str,
    labels: &[&str],
    search_from: usize,
) -> Option<(usize, usize)> {
    labels
        .iter()
        .filter_map(|label| {
            let label = label.to_ascii_lowercase();
            let relative = lower.get(search_from..)?.find(&label)?;
            let start = search_from.saturating_add(relative);
            let end = start.saturating_add(label.len());
            let before_is_label = original
                .get(..start)
                .and_then(|prefix| prefix.chars().next_back())
                .is_some_and(is_label_character);
            let after_is_label = original
                .get(end..)
                .and_then(|suffix| suffix.chars().next())
                .is_some_and(is_label_character);
            (!before_is_label && !after_is_label).then_some((start, end))
        })
        .min_by_key(|(start, _end)| *start)
}

fn labeled_value_bounds(
    value: &str,
    label_end: usize,
    rest_of_line: bool,
) -> Option<(usize, usize)> {
    let mut cursor = skip_whitespace(value, label_end);
    if value.get(cursor..)?.starts_with('[') {
        let qualifier_end = value.get(cursor..)?.find(']')?;
        cursor = cursor.saturating_add(qualifier_end.saturating_add(1));
        cursor = skip_whitespace(value, cursor);
    }
    if value.get(cursor..)?.starts_with('"') {
        cursor = cursor.saturating_add('"'.len_utf8());
        cursor = skip_whitespace(value, cursor);
    }
    let separator = value.get(cursor..)?.chars().next()?;
    if separator != '=' && separator != ':' {
        return None;
    }
    cursor = cursor.saturating_add(separator.len_utf8());
    let mut start = skip_whitespace(value, cursor);
    if value.get(start..)?.starts_with('"') {
        start = start.saturating_add('"'.len_utf8());
        let end = value.get(start..)?.find('"')?;
        return Some((start, start.saturating_add(end)));
    }
    let end = if rest_of_line {
        value
            .get(start..)?
            .find([';', '\n', '\r'])
            .map_or(value.len(), |offset| start.saturating_add(offset))
    } else {
        value
            .get(start..)?
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ';' | ',' | ')' | ']' | '}')
            })
            .map_or(value.len(), |offset| start.saturating_add(offset))
    };
    if start < end
        && value
            .get(..end)
            .and_then(|prefix| prefix.chars().next_back())
            .is_some_and(char::is_whitespace)
    {
        return Some((start, end.saturating_sub(1)));
    }
    Some((start, end))
}

fn skip_whitespace(value: &str, start: usize) -> usize {
    let mut cursor = start;
    while let Some(character) = value.get(cursor..).and_then(|rest| rest.chars().next()) {
        if !character.is_whitespace() {
            break;
        }
        cursor = cursor.saturating_add(character.len_utf8());
    }
    cursor
}

fn replace_spans(value: &str, spans: &[(usize, usize)], replacement: &str) -> String {
    if spans.is_empty() {
        return value.to_owned();
    }
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0_usize;
    for (start, end) in spans {
        let Some(prefix) = value.get(cursor..*start) else {
            return value.to_owned();
        };
        result.push_str(prefix);
        result.push_str(replacement);
        cursor = *end;
    }
    if let Some(suffix) = value.get(cursor..) {
        result.push_str(suffix);
    }
    result
}

fn email_spans(value: &str) -> Vec<(usize, usize)> {
    let bytes = value.as_bytes();
    let mut spans = Vec::new();
    for (at, character) in value.char_indices() {
        if character != '@' {
            continue;
        }
        let mut start = at;
        while let Some(previous) = start.checked_sub(1) {
            if !is_email_byte(bytes.get(previous).copied()) {
                break;
            }
            start = previous;
        }
        let mut end = at.saturating_add(character.len_utf8());
        while end < bytes.len() && is_email_byte(bytes.get(end).copied()) {
            end = end.saturating_add(1);
        }
        let Some(candidate) = value.get(start..end) else {
            continue;
        };
        let Some((local, domain)) = candidate.split_once('@') else {
            continue;
        };
        if local.is_empty() || !domain.contains('.') || domain.starts_with('.') {
            continue;
        }
        if spans
            .last()
            .is_some_and(|(_, previous_end)| *previous_end > start)
        {
            continue;
        }
        spans.push((start, end));
    }
    spans
}

fn is_email_byte(byte: Option<u8>) -> bool {
    byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || b"._%+-@".contains(&byte))
}

const fn is_label_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn redact_controls(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' | '\r' | '\t' => result.push(character),
            character if character.is_control() => result.extend(character.escape_default()),
            character => result.push(character),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_labels_are_stable() {
        assert_eq!(RedactionPolicy::BestEffort.to_string(), "best_effort");
        assert_eq!(
            RedactionPolicy::XcodeMandatory.to_string(),
            "xcode_mandatory"
        );
    }

    #[test]
    fn best_effort_redacts_secrets_and_emails() {
        let text = redact(
            "email jane@example.com token=abc123 local/path",
            RedactionPolicy::BestEffort,
        );
        assert_eq!(text, "email [EMAIL] token=[REDACTED] local/path");
    }

    #[test]
    fn xcode_redaction_uses_typed_context_tokens() {
        let text = redact(
            "CODE_SIGN_IDENTITY = Apple Development: Jane Doe (TEAM123)\nDEVELOPMENT_TEAM=TEAM123\nPROVISIONING_PROFILE_SPECIFIER = Demo Profile\n/Users/jane/project",
            RedactionPolicy::XcodeMandatory,
        );
        assert!(text.contains("[SIGNING_IDENTITY]"));
        assert!(text.contains("[TEAM_ID]"));
        assert!(text.contains("[PROFILE_ID]"));
        assert!(text.contains("/Users/jane/project"));
        assert!(!text.contains("TEAM123"));
    }

    #[test]
    fn xcode_redaction_handles_qualified_build_settings() {
        let text = redact(
            "CODE_SIGN_IDENTITY[sdk=iphoneos*] = Apple Development: Jane Doe (TEAM123)\nDEVELOPMENT_TEAM[sdk=iphoneos*] = TEAM123\nPROVISIONING_PROFILE_SPECIFIER[sdk=iphoneos*] = Demo Profile",
            RedactionPolicy::XcodeMandatory,
        );
        assert!(text.contains("[SIGNING_IDENTITY]"));
        assert!(text.contains("[TEAM_ID]"));
        assert!(text.contains("[PROFILE_ID]"));
        assert!(!text.contains("TEAM123"));
    }

    #[test]
    fn best_effort_redacts_authorization_credentials() {
        let text = redact(
            "Authorization: Bearer topsecret",
            RedactionPolicy::BestEffort,
        );
        assert_eq!(text, "Authorization: [REDACTED]");
    }
}
