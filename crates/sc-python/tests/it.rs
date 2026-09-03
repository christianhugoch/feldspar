//! Every integration test in this crate, in one binary.
//!
//! Each file below is still an ordinary test file — it is pulled in as a module
//! rather than compiled as its own target, for the reason the rest of the
//! workspace does it: a target per file is a link per file, and the tree these
//! link is large.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(feature = "python-host")]
#[path = "python_db.rs"]
mod python_db;

#[cfg(feature = "python-host")]
#[path = "python_db_live.rs"]
mod python_db_live;

#[cfg(feature = "python-host")]
#[path = "python_imports.rs"]
mod python_imports;

#[cfg(feature = "python-host")]
#[path = "python_fetch.rs"]
mod python_fetch;

#[cfg(feature = "python-host")]
#[path = "python_files.rs"]
mod python_files;

#[cfg(feature = "python-host")]
#[path = "python_modfn.rs"]
mod python_modfn;

#[cfg(feature = "python-host")]
#[path = "python_runtime.rs"]
mod python_runtime;

#[cfg(feature = "python-host")]
#[path = "python_triggers.rs"]
mod python_triggers;

#[cfg(not(feature = "python-host"))]
#[path = "without_python.rs"]
mod without_python;
