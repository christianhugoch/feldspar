//! Every integration test in this crate, in one binary.
//!
//! Each file below is still an ordinary test file — it is pulled in as a module
//! rather than compiled as its own target. The workspace statically links V8 into
//! every test binary, so a target per file cost ~400 MB of disk and a link each;
//! CI ran out of disk on the link (`ld terminated with signal 7`) before it ran
//! out of patience. Files stay where they are, so paths relative to a test file
//! (fixtures, `include_str!`, `#[path]`) are unaffected.
//!
//! Add a new test file and it is picked up here — the list is the whole wiring.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "action_context.rs"]
mod action_context;
#[path = "error_reentrancy.rs"]
mod error_reentrancy;
#[path = "event_template.rs"]
mod event_template;
#[path = "scheduler.rs"]
mod scheduler;
#[path = "trigger_store.rs"]
mod trigger_store;
#[path = "workflow_body.rs"]
mod workflow_body;
