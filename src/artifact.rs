use std::fmt;
use std::path::{Path, PathBuf};

use crate::cli::{Action, Tool};

/// Kind of local product associated with a build action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    App,
    Binary,
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::App => "app",
            Self::Binary => "binary",
        })
    }
}

/// Resolution state reported before metadata-dependent product lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    PlannedUnresolved,
    Unresolved,
    Resolved,
}

impl fmt::Display for ArtifactState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlannedUnresolved => "planned/unresolved",
            Self::Unresolved => "unresolved",
            Self::Resolved => "resolved",
        })
    }
}

/// Filesystem-resolved artifact intent emitted by build and plan records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    kind: ArtifactKind,
    root: PathBuf,
    product: Option<String>,
    scheme: Option<String>,
    path: Option<PathBuf>,
    action: Option<String>,
    state: ArtifactState,
}
impl Artifact {
    #[must_use]
    pub fn for_command(
        tool: Tool,
        action: Action,
        root: &Path,
        planned: bool,
        post_action: Option<&str>,
    ) -> Option<Self> {
        let kind = match (tool, action) {
            (Tool::Xcode, Action::Build) => ArtifactKind::App,
            (Tool::Swift | Tool::Cargo, Action::Build) => ArtifactKind::Binary,
            _ => return None,
        };
        Some(Self {
            kind,
            root: root.to_owned(),
            product: None,
            scheme: None,
            path: None,
            action: post_action.map(str::to_owned),
            state: if planned {
                ArtifactState::PlannedUnresolved
            } else {
                ArtifactState::Unresolved
            },
        })
    }
    pub(crate) fn resolved(
        kind: ArtifactKind,
        root: &Path,
        product: String,
        scheme: Option<String>,
        path: PathBuf,
        action: Option<String>,
    ) -> Self {
        Self {
            kind,
            root: root.to_owned(),
            product: Some(product),
            scheme,
            path: Some(path),
            action,
            state: ArtifactState::Resolved,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        self.kind
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn product(&self) -> Option<&str> {
        self.product.as_deref()
    }

    #[must_use]
    pub fn scheme(&self) -> Option<&str> {
        self.scheme.as_deref()
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub const fn state(&self) -> ArtifactState {
        self.state
    }

    /// Render fields without forwarding command or runtime argument values.
    #[must_use]
    pub fn render(&self) -> String {
        let mut fields = format!("kind={}", self.kind);
        if let Some(product) = self.product.as_deref().filter(|value| !value.is_empty()) {
            fields.push_str(" product=");
            fields.push_str(product);
        }
        if let Some(scheme) = self.scheme.as_deref().filter(|value| !value.is_empty()) {
            fields.push_str(" scheme=");
            fields.push_str(scheme);
        }
        if let Some(path) = self
            .path
            .as_deref()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fields.push_str(" path=");
            fields.push_str(&path.display().to_string());
        }
        if let Some(action) = self.action.as_deref().filter(|value| !value.is_empty()) {
            fields.push_str(" action=");
            fields.push_str(action);
        }
        if self.state != ArtifactState::Resolved {
            fields.push_str(" state=");
            fields.push_str(&self.state.to_string());
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_intents_omit_absent_fields_and_root() {
        let planned = Artifact::for_command(
            Tool::Xcode,
            Action::Build,
            Path::new("/root"),
            true,
            Some("move_and_open"),
        )
        .expect("xcode build artifact");
        assert_eq!(
            planned.render(),
            "kind=app action=move_and_open state=planned/unresolved"
        );

        let unresolved =
            Artifact::for_command(Tool::Cargo, Action::Build, Path::new("/root"), false, None)
                .expect("cargo build artifact");
        assert_eq!(unresolved.render(), "kind=binary state=unresolved");

        let resolved = Artifact::resolved(
            ArtifactKind::App,
            Path::new("/root"),
            "Demo.app".to_owned(),
            Some("Demo".to_owned()),
            PathBuf::from("/Applications/Demo.app"),
            Some("open".to_owned()),
        );
        assert_eq!(
            resolved.render(),
            "kind=app product=Demo.app scheme=Demo path=/Applications/Demo.app action=open"
        );
    }
}
