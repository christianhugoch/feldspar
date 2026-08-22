//! Shared setup for the module tests: the fixture packages, a throwaway modules
//! root, and the two toolchain checks.
//!
//! Every test here needs `node`, and most need `npm`. A Rust-only checkout must
//! stay green, so a test that needs either **skips** rather than fails — the
//! same contract the `tsc` type-check tests have.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use sc_module::{Installer, ModuleHost, ModuleSource};

/// Whether `node` is on the PATH.
pub fn have_node() -> bool {
    which("node")
}

/// Whether `npm` is on the PATH.
pub fn have_npm() -> bool {
    which("npm")
}

fn which(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Say why a test did nothing, and return, so a skip is visible in `-- --nocapture`
/// rather than looking like a pass.
#[macro_export]
macro_rules! skip_without {
    ($cond:expr, $why:expr) => {
        if !$cond {
            eprintln!("skipping: {}", $why);
            return;
        }
    };
}

/// One of the fixture packages under `tests/fixtures`.
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A throwaway modules root, removed first so a rerun starts clean.
pub fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sc-module-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// An installer over a fresh modules root with `fixtures` installed into it, and
/// a host over the same root.
///
/// Panics on an install failure rather than returning it: a test whose *setup*
/// broke has nothing to assert, and npm's own message is what the panic carries.
pub async fn installed(tag: &str, fixtures: &[&str]) -> (Installer, Arc<ModuleHost>, Vec<String>) {
    let root = temp_root(tag);
    let installer = Installer::new(&root);
    let mut names = Vec::new();
    for name in fixtures {
        let package = installer
            .install(ModuleSource::Local, &fixture(name).display().to_string())
            .await
            .unwrap_or_else(|e| panic!("installing the {name} fixture: {e}"));
        names.push(package.name);
    }
    let host = Arc::new(ModuleHost::new(&root));
    (installer, host, names)
}
