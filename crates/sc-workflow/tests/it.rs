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

#[path = "driver.rs"]
mod driver;
#[path = "queue_seam.rs"]
mod queue_seam;
#[path = "step_transaction.rs"]
mod step_transaction;
#[path = "suspension.rs"]
mod suspension;
#[path = "versions_store.rs"]
mod versions_store;
#[path = "wakeup_cache.rs"]
mod wakeup_cache;
#[path = "workflow_scope.rs"]
mod workflow_scope;
