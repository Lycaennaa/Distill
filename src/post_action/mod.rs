mod cargo;
mod move_app;
mod products;
mod runtime;
mod swift;
mod xcode;

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::artifact::Artifact;
use crate::cli::{Invocation, PostAction, PostActionKind, Tool};
use crate::process::{CancellationToken, CommandSpec, Execution, ProcessError};
use crate::redaction::RedactionPolicy;
use crate::status::{Status, StatusClass};

use self::move_app::MoveOutcome;
use self::products::{BuiltProduct, Failure};

/// Product resolution and post-action state after one requested operation.
#[derive(Debug)]
pub enum PostActionOutcome {
    ProductNotResolved {
        action: PostActionKind,
    },
    ProductResolved {
        artifact: Artifact,
        actions: Vec<ActionRecord>,
    },
}

/// One typed action event emitted after the product has resolved.
#[derive(Debug)]
pub enum ActionRecord {
    MoveFailed,
    MoveCompleted {
        destination: PathBuf,
    },
    MoveCommitted {
        destination: PathBuf,
        backup: Option<PathBuf>,
    },
    OpenStarted,
    OpenFailed,
    RuntimeCompleted {
        status: Status,
    },
}

impl ActionRecord {
    /// Render the documented ACTION payload for this state transition.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::MoveFailed => "move state=failed".to_owned(),
            Self::MoveCompleted { destination } => {
                format!(
                    "move state=complete path={} backup=none",
                    destination.display()
                )
            }
            Self::MoveCommitted {
                destination,
                backup,
            } => format!(
                "move state=committed path={} backup={}",
                destination.display(),
                backup
                    .as_deref()
                    .map_or_else(|| "none".to_owned(), |path| path.display().to_string()),
            ),
            Self::OpenStarted => "open state=started".to_owned(),
            Self::OpenFailed => "open state=failed".to_owned(),
            Self::RuntimeCompleted { status } => format!(
                "runtime state={} exit={}",
                if status.is_success() {
                    "complete"
                } else {
                    "failed"
                },
                status.code()
            ),
        }
    }
}

struct ActionContext<'a> {
    invocation: &'a Invocation,
    build: &'a CommandSpec,
    root: &'a Path,
    post_action: &'a PostAction,
    deadline: Option<Instant>,
    cancellation: &'a CancellationToken,
    policy: RedactionPolicy,
}

/// Resolve and perform a requested post-action after a successful build.
///
/// # Errors
/// Returns a process error when status aggregation fails.
pub fn run(
    invocation: &Invocation,
    post_action: &PostAction,
    build: &CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
    policy: RedactionPolicy,
) -> Result<PostActionOutcome, ProcessError> {
    let root = build.cwd().ok_or(ProcessError::SupervisorFailure)?;
    let context = ActionContext {
        invocation,
        build,
        root,
        post_action,
        deadline,
        cancellation,
        policy,
    };
    let mut product = match resolve_product(invocation, build, execution, deadline, cancellation) {
        Ok(product) => product,
        Err(failure) => {
            fail(execution, failure)?;
            return Ok(PostActionOutcome::ProductNotResolved {
                action: post_action.kind(),
            });
        }
    };
    let mut actions = Vec::new();
    if post_action.moves_app() && !perform_move(&mut product, &context, &mut actions, execution)? {
        return Ok(PostActionOutcome::ProductResolved {
            artifact: resolved_artifact(&product, &context),
            actions,
        });
    }
    if post_action.opens_app() {
        perform_runtime(&product, &context, &mut actions, execution)?;
    }
    Ok(PostActionOutcome::ProductResolved {
        artifact: resolved_artifact(&product, &context),
        actions,
    })
}

fn resolve_product(
    invocation: &Invocation,
    build: &CommandSpec,
    execution: &mut Execution,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<BuiltProduct, Failure> {
    match invocation.tool() {
        Tool::Xcode => xcode::resolve(build, execution, deadline, cancellation),
        Tool::Swift => swift::resolve(invocation, build, execution, deadline, cancellation),
        Tool::Cargo => cargo::resolve(invocation, build, execution, deadline, cancellation),
    }
}

fn perform_move(
    product: &mut BuiltProduct,
    context: &ActionContext<'_>,
    actions: &mut Vec<ActionRecord>,
    execution: &mut Execution,
) -> Result<bool, ProcessError> {
    let moved = match move_app::move_to_applications(
        product.path(),
        context.deadline,
        context.cancellation,
    ) {
        Ok(moved) => moved,
        Err(failure) => {
            fail(execution, failure)?;
            actions.push(ActionRecord::MoveFailed);
            return Ok(false);
        }
    };
    match moved {
        MoveOutcome::Published { destination } => {
            product.relocate_app(destination.clone());
            actions.push(ActionRecord::MoveCompleted { destination });
            Ok(true)
        }
        MoveOutcome::PublishedWithFailure {
            destination,
            backup,
            failure,
        } => {
            product.relocate_app(destination.clone());
            actions.push(ActionRecord::MoveCommitted {
                destination,
                backup,
            });
            fail(execution, failure)?;
            Ok(false)
        }
    }
}

fn perform_runtime(
    product: &BuiltProduct,
    context: &ActionContext<'_>,
    actions: &mut Vec<ActionRecord>,
    execution: &mut Execution,
) -> Result<(), ProcessError> {
    match runtime::run(
        context.invocation,
        context.post_action,
        context.build,
        product,
        context.deadline,
        context.cancellation,
        context.policy,
    ) {
        Ok(Some(runtime)) => {
            actions.push(ActionRecord::RuntimeCompleted {
                status: runtime.report().final_status(),
            });
            execution.absorb_output(&runtime)
        }
        Ok(None) => {
            actions.push(ActionRecord::OpenStarted);
            Ok(())
        }
        Err(failure) => {
            fail(execution, failure)?;
            actions.push(ActionRecord::OpenFailed);
            Ok(())
        }
    }
}

fn resolved_artifact(product: &BuiltProduct, context: &ActionContext<'_>) -> Artifact {
    Artifact::resolved(
        product.kind(),
        context.root,
        product.name().to_owned(),
        product.scheme().map(str::to_owned),
        product.path().to_owned(),
        Some(context.post_action.label().to_owned()),
    )
}

fn fail(execution: &mut Execution, failure: Failure) -> Result<(), ProcessError> {
    execution.add_wrapper_status(failure.class, failure.message)
}

fn wrapper_failure(class: StatusClass, message: impl Into<String>) -> Failure {
    Failure {
        class,
        message: message.into(),
    }
}

pub(super) fn status_from_stop(
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Option<StatusClass> {
    if deadline.is_some_and(|value| Instant::now() >= value) {
        Some(StatusClass::Timeout)
    } else if cancellation.is_cancelled() {
        Some(StatusClass::Interrupt)
    } else {
        None
    }
}
