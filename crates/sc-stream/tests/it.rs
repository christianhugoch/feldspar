//! Every integration test in this crate, in one binary.
//!
//! Each file below is still an ordinary test file — it is pulled in as a module
//! rather than compiled as its own target. The workspace statically links V8
//! into every test binary, so a target per file cost ~400 MB of disk and a link
//! each. Files stay where they are, so paths relative to a test file are
//! unaffected.
//!
//! Add a new test file and it is picked up here — the list is the whole wiring.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "scripted_provider.rs"]
mod scripted_provider;

#[path = "stream_store.rs"]
mod stream_store;
