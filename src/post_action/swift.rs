use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::cli::Invocation;
use crate::process::{CancellationToken, CommandSpec, Execution};

use super::products::{
    BuiltProduct, artifact_failure, is_executable_file, metadata_spec, parse_json,
    parse_option_values, query_metadata, single_selector,
};

pub(super) fn resolve(
    invocation: &Invocation,
    build: &CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<BuiltProduct, super::products::Failure> {
    let root = build
        .cwd()
        .ok_or_else(|| artifact_failure("build root is unavailable"))?;
    let selectors = parse_option_values(invocation.forwarded_args(), "--product", None)?;
    let selector = single_selector(&selectors)?;

    let describe = metadata_spec(
        "swift",
        [
            OsString::from("package"),
            OsString::from("describe"),
            OsString::from("--type"),
            OsString::from("json"),
        ],
        root,
    );
    let text = query_metadata(
        execution,
        &describe,
        deadline,
        cancellation,
        "Swift product metadata failed",
    )?;
    let json = parse_json(&text, "Swift product metadata was invalid")?;
    let products = json
        .get("products")
        .and_then(Value::as_array)
        .ok_or_else(|| artifact_failure("Swift product metadata was incomplete"))?;
    let names = products
        .iter()
        .filter(|product| is_executable_product(product))
        .filter_map(|product| {
            product
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    let selected = match selector {
        Some(name) if names.iter().any(|candidate| candidate == name) => name.to_owned(),
        Some(_name) => return Err(artifact_failure("selected Swift product is not executable")),
        None if names.len() == 1 => names.first().cloned().unwrap_or_default(),
        None => {
            return Err(artifact_failure(if names.is_empty() {
                "no runnable Swift executable product found".to_owned()
            } else {
                format!(
                    "multiple Swift executable products found; choose one of: {}",
                    names.join(", ")
                )
            }));
        }
    };

    let mut arguments = vec![OsString::from("build"), OsString::from("--show-bin-path")];
    arguments.extend(build.arguments().iter().skip(1).cloned());
    let bin_path_spec = metadata_spec("swift", arguments, root);
    let path_text = query_metadata(
        execution,
        &bin_path_spec,
        deadline,
        cancellation,
        "Swift binary path lookup failed",
    )?;
    let bin_path = PathBuf::from(path_text.trim());
    let executable = bin_path.join(&selected);
    if !is_executable_file(&executable) {
        return Err(artifact_failure(
            "selected Swift product executable is missing",
        ));
    }
    Ok(BuiltProduct::SwiftExecutable {
        name: selected,
        path: executable,
    })
}

fn is_executable_product(product: &Value) -> bool {
    product.get("type").is_some_and(|kind| {
        kind.get("executable").is_some() || kind.as_str().is_some_and(|name| name == "executable")
    })
}
