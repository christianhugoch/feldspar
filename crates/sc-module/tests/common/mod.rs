//! Shared setup for the module tests: the fixture packages, a throwaway modules
//! root, and the toolchain check.
//!
//! Most tests here need `npm`, which is what *installs* a module. None of them
//! needs `node`: a module runs on a Deno worker inside the test binary. A
//! Rust-only checkout must stay green, so a test that needs npm **skips** rather
//! than fails — the same contract the `tsc` type-check tests have.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use sc_module::{Installer, ModuleHost, ModuleSource};

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

/// The same, but reaching the pool directly rather than through
/// [`sc_module::ModuleHost`] — which is what a test that wants to say a bound
/// needs.
///
/// `bounds` is spelled out by the caller because the tests are what want to say
/// a JS slice in milliseconds; `PoolBounds::default()` is what a server runs.
#[cfg(feature = "deno-host")]
pub async fn installed_on_deno(
    tag: &str,
    fixtures: &[&str],
    workers: usize,
    bounds: sc_module::PoolBounds,
) -> (Installer, Arc<sc_module::DenoModuleHost>, Vec<String>) {
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
    let host = Arc::new(sc_module::DenoModuleHost::build(root, workers, bounds));
    (installer, host, names)
}
