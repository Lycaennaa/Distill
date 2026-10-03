use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::process::{CancellationToken, CommandSpec, Execution};

use super::products::{
    BuiltProduct, artifact_failure, is_executable_file, metadata_spec, query_metadata,
};

pub(super) fn resolve(
    build: &CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<BuiltProduct, super::products::Failure> {
    let root = build
        .cwd()
        .ok_or_else(|| artifact_failure("build root is unavailable"))?;
    let mut arguments = vec![OsString::from("-showBuildSettings")];
    arguments.extend(
        build
            .arguments()
            .iter()
            .skip(1)
            .filter(|argument| argument.as_os_str() != "-quiet")
            .cloned(),
    );
    let settings_spec = metadata_spec(build.program().to_owned(), arguments, root);
    let text = query_metadata(
        execution,
        &settings_spec,
        deadline,
        cancellation,
        "Xcode build settings lookup failed",
    )?;
    let mut candidate_paths = BTreeMap::new();
    for candidate in parse_build_settings(&text).iter().filter_map(app_candidate) {
        candidate_paths
            .entry(candidate.path)
            .and_modify(|supported| *supported |= candidate.platform_supported)
            .or_insert(candidate.platform_supported);
    }
    let candidates = candidate_paths
        .into_iter()
        .map(|(path, platform_supported)| AppCandidate {
            path,
            platform_supported,
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(artifact_failure(
            "Xcode build settings did not identify an app product",
        ));
    }
    let mut valid = Vec::new();
    for candidate in candidates {
        if !candidate
            .path
            .extension()
            .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("app"))
        {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&candidate.path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let Some((executable, metadata_supported)) = bundle_executable(&candidate.path) else {
            continue;
        };
        if candidate.platform_supported && metadata_supported && is_executable_file(&executable) {
            valid.push((candidate, executable));
        }
    }
    if valid.len() > 1 {
        let choices = valid
            .iter()
            .map(|(candidate, _)| {
                candidate.path.file_name().map_or_else(
                    || candidate.path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(artifact_failure(format!(
            "multiple macOS app products found; choose one of: {choices}"
        )));
    }
    let Some((candidate, executable)) = valid.pop() else {
        return Err(artifact_failure(
            "Xcode product is missing or is not a macOS or Mac Catalyst app",
        ));
    };
    let name = candidate.path.file_name().map_or_else(
        || "<unknown>".to_owned(),
        |value| value.to_string_lossy().into_owned(),
    );
    let scheme = option_value(build.arguments(), "-scheme");
    Ok(BuiltProduct::XcodeApp {
        name,
        scheme,
        path: candidate.path,
        executable,
    })
}

struct AppCandidate {
    path: PathBuf,
    platform_supported: bool,
}

fn parse_build_settings(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut blocks = Vec::new();
    let mut current = BTreeMap::new();
    for line in text.lines() {
        if line.trim_start().starts_with("Build settings for action ") {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        }) {
            current.insert(key.to_owned(), value.trim().to_owned());
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn app_candidate(settings: &BTreeMap<String, String>) -> Option<AppCandidate> {
    let full_name = settings.get("FULL_PRODUCT_NAME").cloned().or_else(|| {
        let extension = settings.get("WRAPPER_EXTENSION")?;
        if !extension.eq_ignore_ascii_case("app") {
            return None;
        }
        settings
            .get("PRODUCT_NAME")
            .map(|name| format!("{name}.app"))
    })?;
    if !full_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("app"))
    {
        return None;
    }
    let directory = settings
        .get("TARGET_BUILD_DIR")
        .or_else(|| settings.get("BUILT_PRODUCTS_DIR"))?;
    let path = PathBuf::from(directory).join(full_name);
    let platform = settings.get("PLATFORM_NAME").map_or("", String::as_str);
    let effective = settings
        .get("EFFECTIVE_PLATFORM_NAME")
        .map_or("", String::as_str);
    let sdk = settings.get("SDKROOT").map_or("", String::as_str);
    let device_only = matches!(
        platform,
        "iphoneos"
            | "iphonesimulator"
            | "appletvos"
            | "appletvsimulator"
            | "watchos"
            | "watchsimulator"
            | "xros"
            | "xrsimulator"
    );
    let platform_supported = !device_only
        && (matches!(platform, "macosx" | "maccatalyst")
            || effective.eq_ignore_ascii_case("-maccatalyst")
            || sdk.to_ascii_lowercase().contains("macosx.sdk"));
    Some(AppCandidate {
        path,
        platform_supported,
    })
}

fn bundle_executable(app: &Path) -> Option<(PathBuf, bool)> {
    let plist_path = [app.join("Contents/Info.plist"), app.join("Info.plist")]
        .into_iter()
        .find(|path| path.is_file())?;
    let value = plist::Value::from_file(plist_path).ok()?;
    let dictionary = value.as_dictionary()?;
    let executable_name = dictionary.get("CFBundleExecutable")?.as_string()?;
    if executable_name.is_empty() || Path::new(executable_name).components().count() != 1 {
        return None;
    }
    let supported_platforms = dictionary
        .get("CFBundleSupportedPlatforms")
        .and_then(plist::Value::as_array)
        .map(|platforms| {
            platforms
                .iter()
                .filter_map(plist::Value::as_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let platform_name = dictionary
        .get("DTPlatformName")
        .and_then(plist::Value::as_string)
        .unwrap_or("");
    let platform_variant = dictionary
        .get("DTPlatformVariant")
        .and_then(plist::Value::as_string)
        .unwrap_or("");
    let metadata_supported = supported_platforms
        .iter()
        .any(|platform| matches!(*platform, "MacOSX" | "MacCatalyst"))
        || matches!(platform_name, "macosx" | "maccatalyst")
        || platform_variant == "maccatalyst";
    let executable = app.join("Contents/MacOS").join(executable_name);
    let executable = if executable.is_file() {
        executable
    } else {
        app.join("MacOS").join(executable_name)
    };
    Some((executable, metadata_supported))
}

fn option_value(arguments: &[OsString], name: &str) -> Option<String> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        let text = argument.to_str()?;
        if text == name {
            arguments
                .get(index.saturating_add(1))?
                .to_str()
                .map(str::to_owned)
        } else {
            text.strip_prefix(&format!("{name}=")).map(str::to_owned)
        }
    })
}
