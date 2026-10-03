use super::{Action, CliOptions, Tool};
/// Runtime mode selected for a built product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Detached,
    Wait,
    Stream,
}

/// Action identity independent of wait mode and runtime argument values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostActionKind {
    Move,
    Open,
    MoveAndOpen,
}

impl PostActionKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Open => "open",
            Self::MoveAndOpen => "move_and_open",
        }
    }
}

/// One normalized post-build action; invalid flag combinations cannot exist here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostAction {
    Move,
    Open { mode: RuntimeMode },
    MoveAndOpen { mode: RuntimeMode },
}

impl PostAction {
    #[must_use]
    pub const fn kind(&self) -> PostActionKind {
        match self {
            Self::Move => PostActionKind::Move,
            Self::Open { .. } => PostActionKind::Open,
            Self::MoveAndOpen { .. } => PostActionKind::MoveAndOpen,
        }
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.kind().label()
    }

    #[must_use]
    pub const fn moves_app(&self) -> bool {
        matches!(self, Self::Move | Self::MoveAndOpen { .. })
    }

    #[must_use]
    pub const fn opens_app(&self) -> bool {
        matches!(self, Self::Open { .. } | Self::MoveAndOpen { .. })
    }

    #[must_use]
    pub const fn runtime_mode(&self) -> Option<RuntimeMode> {
        match self {
            Self::Move => None,
            Self::Open { mode } | Self::MoveAndOpen { mode } => Some(*mode),
        }
    }

    #[must_use]
    pub const fn waits_for_runtime(&self) -> bool {
        matches!(
            self.runtime_mode(),
            Some(RuntimeMode::Wait | RuntimeMode::Stream)
        )
    }
}
pub(super) fn normalize_post_action(
    tool: Tool,
    action: Action,
    options: &CliOptions,
) -> Result<Option<PostAction>, String> {
    let selection_count = [options.open, options.mv, options.omv]
        .into_iter()
        .filter(|selected| *selected)
        .count();
    if selection_count > 1 {
        return Err("--mv, --open, and --omv are mutually exclusive".to_owned());
    }

    let is_xcode_build = tool == Tool::Xcode && action == Action::Build;
    if (options.mv || options.omv) && !is_xcode_build {
        return Err("--mv and --omv are valid only for xcode build".to_owned());
    }
    if options.open && !is_xcode_build {
        return Err("--open is valid only for xcode build".to_owned());
    }
    if (options.wait || options.stream) && !options.open && !options.omv {
        return Err("--wait and --stream require --open or --omv".to_owned());
    }

    let mode = if options.stream {
        RuntimeMode::Stream
    } else if options.wait {
        RuntimeMode::Wait
    } else {
        RuntimeMode::Detached
    };
    let action = if options.omv {
        Some(PostAction::MoveAndOpen { mode })
    } else if options.mv {
        Some(PostAction::Move)
    } else if options.open {
        Some(PostAction::Open { mode })
    } else {
        None
    };
    Ok(action)
}
