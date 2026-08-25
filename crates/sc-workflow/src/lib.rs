//! `sc-workflow` — durable workflows (design §10.3).
//!
//! Layer 7, beside `sc-agent` and above `sc-action`. A **workflow is a trigger
//! body**, not a new top-level entity (decision 1): the trigger supplies the
//! event, the `only_if`, the role floor, the enabled flag, the periodic timing
//! and the exposure through an application, and this crate supplies the only
//! thing that is different — a *program* instead of one action, and an engine
//! that advances it durably.
//!
//! ## What is here
//!
//! - The **program** ([`workflow`]): [`Workflow`], [`Step`], [`StepKind`],
//!   [`Next`] and [`ErrorPolicy`] — pure data, whose serde shape *is* the stored
//!   shape, the API shape and the visual editor's shape.
//! - The **versions** ([`versions`]): `_sc_workflow_versions`, append-only, so a
//!   run pins the version it started on and finishes on it however many times the
//!   workflow is edited meanwhile.
//! - The **trace** ([`traces`]): `_sc_run_traces`, one row per completed step
//!   attempt, written only when the workflow asks for it.
//! - The **scope** ([`scope`]): [`workflow_shape`], the one place a step's
//!   formulas' scope is decided.
//! - The **machine** ([`machine`]): [`WorkflowRun`], the whole resumable state of
//!   one run as a single serialisable value, and the [`Decision`] it hands a
//!   driver. Sans-IO — it owns every decision and performs none of the work — so
//!   the engine's rules are testable synchronously, with no database, no runtime
//!   and no clock.
//!
//! The driver that does the IO and the queue that claims runnable runs are the
//! next phases of §10.3; the [`WorkflowEngine`](sc_action::WorkflowEngine) seam
//! they install themselves into already exists in `sc-action`.
//!
//! ## The two decisions worth knowing before reading
//!
//! **Control flow is data.** [`Next`] is an enum — a step, a branch, a formula,
//! the end — because a visual editor cannot round-trip an arbitrary expression
//! into edges. The formula variant keeps v1's expressiveness for the case that
//! needs it, drawn as one dashed edge to a computed marker.
//!
//! **A version is a row.** Editing a workflow mints a new one; nothing is
//! rewritten. That is the whole implementation of "a suspended run finishes with
//! its version of the workflow".

pub mod machine;
pub mod scope;
pub mod traces;
pub mod versions;
pub mod workflow;

pub use machine::{Conclusion, Decision, PendingForm, WorkflowRun};
pub use scope::{WORKFLOW_SCOPE, workflow_shape};
pub use traces::{
    RunTrace, TRACES_TABLE, TraceOutcome, bootstrap_run_traces, delete_run_traces, list_run_traces,
    save_run_trace,
};
pub use versions::{
    VERSIONS_TABLE, WorkflowVersion, bootstrap_workflow_versions, current_workflow,
    delete_workflow_versions, list_workflow_versions, load_workflow_version, max_version,
    require_current_workflow, require_workflow_version, save_workflow,
};
pub use workflow::{
    Assignment, Backoff, BranchArm, DEFAULT_MAX_STEPS, ErrorPolicy, FieldDecl, Next, Step,
    StepKind, Workflow,
};

/// The context key an [`ErrorPolicy::Handler`] jump puts the failure under, so a
/// handler step can read what went wrong (`context.error.message`).
///
/// Reserved: a `Set` writing this key would make the handler read its own
/// workflow's invention instead of the engine's report, so validation refuses it.
pub const ERROR_KEY: &str = "error";
