use std::ffi::OsString;
use std::path::Path;

#[derive(Debug)]
pub struct ExplicitPath {
    pub option: String,
    pub path: std::path::PathBuf,
}

pub fn arguments_before_child_separator(arguments: &[OsString]) -> impl Iterator<Item = &OsString> {
    arguments
        .iter()
        .take_while(|argument| argument.to_str() != Some("--"))
}

pub fn collect_path_options(arguments: &[OsString], names: &[&str]) -> Vec<ExplicitPath> {
    let mut values = Vec::new();
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        if argument.to_str() == Some("--") {
            break;
        }
        let Some(text) = argument.to_str() else {
            index = index.saturating_add(1);
            continue;
        };
        let Some((name, inline)) = matching_option(text, names) else {
            index = index.saturating_add(1);
            continue;
        };
        let (path, step) = match inline {
            Some(value) if !value.is_empty() => (std::path::PathBuf::from(value), 1_usize),
            Some(_value) => (std::path::PathBuf::new(), 1_usize),
            None => arguments.get(index.saturating_add(1)).map_or_else(
                || (std::path::PathBuf::new(), 1_usize),
                |value| (std::path::PathBuf::from(value), 2_usize),
            ),
        };
        values.push(ExplicitPath {
            option: name.to_owned(),
            path,
        });
        index = index.saturating_add(step);
    }
    values
}

pub fn rewrite_path_argument(arguments: &[OsString], option: &str, path: &Path) -> Vec<OsString> {
    let mut rewritten = arguments.to_vec();
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        if argument.to_str() == Some("--") {
            break;
        }
        let Some(text) = argument.to_str() else {
            index = index.saturating_add(1);
            continue;
        };
        if text == option {
            if let Some(value) = rewritten.get_mut(index.saturating_add(1)) {
                *value = path.as_os_str().to_os_string();
            }
            return rewritten;
        }
        let prefix = format!("{option}=");
        if text.starts_with(&prefix) {
            let mut replacement = OsString::from(option);
            replacement.push("=");
            replacement.push(path.as_os_str());
            if let Some(argument) = rewritten.get_mut(index) {
                *argument = replacement;
            }
            return rewritten;
        }
        index = index.saturating_add(1);
    }
    rewritten
}

fn matching_option<'a, 'b>(
    argument: &'a str,
    names: &'b [&str],
) -> Option<(&'b str, Option<&'a str>)> {
    names.iter().find_map(|name| {
        if argument == *name {
            Some((*name, None))
        } else {
            let prefix = format!("{name}=");
            argument
                .strip_prefix(&prefix)
                .map(|value| (*name, Some(value)))
        }
    })
}
