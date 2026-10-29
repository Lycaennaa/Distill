use std::path::Path;

use crate::cli::{Action, Invocation};
use crate::discovery::{Discovery, DiscoveryContext, RootKind, choose_one, marker_candidates};
use crate::discovery_error::DiscoveryError;

#[cfg(windows)]
const WRAPPER_MARKER: &str = "gradlew.bat";
#[cfg(not(windows))]
const WRAPPER_MARKER: &str = "gradlew";

pub fn discover_gradle(
    invocation: &Invocation,
    action: Action,
    start: &Path,
    context: DiscoveryContext<'_>,
) -> Result<Discovery, DiscoveryError> {
    let marker = choose_one(
        invocation.tool(),
        action,
        start,
        marker_candidates(start, WRAPPER_MARKER, context)?,
    )?;
    let root = marker
        .parent()
        .map_or_else(|| marker.clone(), Path::to_path_buf);
    Ok(Discovery::new(
        root,
        Some(marker),
        RootKind::GradleWrapper,
        false,
    ))
}
