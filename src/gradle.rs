use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
pub fn is_gradle_test_task_name(task: &str) -> bool {
    task == "test" || task.ends_with("Test")
}

pub fn is_gradle_test_action_component(task: &str) -> bool {
    task == "check" || is_gradle_test_task_name(task)
}
pub fn gradle_option_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "-p" | "--project-dir"
            | "--project-cache-dir"
            | "-g"
            | "--gradle-user-home"
            | "-b"
            | "--build-file"
            | "-c"
            | "--settings-file"
            | "--include-build"
            | "-I"
            | "--init-script"
            | "--tests"
            | "--args"
            | "--task"
            | "-x"
            | "--exclude-task"
            | "--max-workers"
            | "--console"
            | "--warning-mode"
            | "--configuration-cache-problems"
            | "--dependency-verification"
            | "--priority"
            | "--update-locks"
            | "-M"
            | "--write-verification-metadata"
    )
}

pub fn gradle_project_root(root: &Path, arguments: &[OsString]) -> Option<PathBuf> {
    let mut project_dir = None;
    let mut consume_value = false;
    let mut consume_project_dir = false;
    for argument in arguments {
        if consume_value {
            if consume_project_dir {
                project_dir = Some(resolve_project_dir(root, argument.as_os_str())?);
            }
            consume_value = false;
            consume_project_dir = false;
            continue;
        }
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument == "--" {
            break;
        }
        if matches!(argument, "-p" | "--project-dir") {
            consume_value = true;
            consume_project_dir = true;
            continue;
        }
        if let Some(value) = argument
            .strip_prefix("--project-dir=")
            .or_else(|| argument.strip_prefix("-p="))
        {
            project_dir = Some(resolve_project_dir(root, OsStr::new(value))?);
        } else if argument.starts_with('-') {
            consume_value = gradle_option_takes_value(argument);
        }
    }
    if consume_value {
        return None;
    }
    Some(project_dir.unwrap_or_else(|| root.to_owned()))
}

fn resolve_project_dir(root: &Path, value: &OsStr) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    let path = Path::new(value);
    Some(if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_skips_option_operands_and_resolves_project_dir() {
        let root = Path::new("wrapper");
        let options = [
            OsString::from("build"),
            OsString::from("--tests"),
            OsString::from("test"),
            OsString::from("-g"),
            OsString::from("test"),
            OsString::from("--update-locks"),
            OsString::from("test"),
        ];
        assert_eq!(gradle_project_root(root, &options), Some(root.to_owned()));

        for option in ["-p", "--project-dir"] {
            let arguments = [
                OsString::from("test"),
                OsString::from(option),
                OsString::from("other"),
            ];
            assert_eq!(
                gradle_project_root(root, &arguments),
                Some(root.join("other"))
            );
        }
        let inline = [
            OsString::from("test"),
            OsString::from("--project-dir=other"),
        ];
        assert_eq!(gradle_project_root(root, &inline), Some(root.join("other")));
    }

    #[test]
    fn project_root_rejects_missing_or_empty_operands() {
        let root = Path::new("wrapper");
        for arguments in [
            vec![OsString::from("test"), OsString::from("-p")],
            vec![OsString::from("test"), OsString::from("--project-dir=")],
        ] {
            assert_eq!(gradle_project_root(root, &arguments), None);
        }
    }
}
