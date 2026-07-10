//! Guards the Phase 0 "repo hygiene" artifacts so they cannot silently
//! disappear or lose their required gates. This does not run rustfmt/clippy
//! themselves (those toolchain components may be absent locally and are
//! exercised in CI); it asserts the configuration that drives them exists and
//! declares the pieces the workspace depends on.

use std::fs;
use std::path::{Path, PathBuf};

/// Walk up from this crate's manifest dir to the workspace root (the ancestor
/// whose `Cargo.toml` declares `[workspace]`).
fn workspace_root() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            let contents = fs::read_to_string(&manifest).unwrap_or_default();
            if contents.contains("[workspace]") {
                return dir;
            }
        }
        assert!(
            dir.pop(),
            "reached the filesystem root without finding a [workspace] Cargo.toml"
        );
    }
}

fn read(root: &Path, rel: &str) -> String {
    let path = root.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing {rel}: {e}"))
}

#[test]
fn rustfmt_config_present_and_pins_edition() {
    let root = workspace_root();
    let cfg = read(&root, "rustfmt.toml");
    // Edition must be pinned so `cargo fmt` on a stable toolchain matches the
    // 2024-edition workspace.
    assert!(cfg.contains("edition"), "rustfmt.toml must pin an edition");
    assert!(cfg.contains("2024"), "rustfmt.toml edition should be 2024");
}

#[test]
fn clippy_config_exempts_tests_from_unwrap_lints() {
    let root = workspace_root();
    let cfg = read(&root, "clippy.toml");
    assert!(cfg.contains("allow-unwrap-in-tests"));
    assert!(cfg.contains("allow-expect-in-tests"));
}

#[test]
fn ci_workflow_runs_fmt_clippy_and_test() {
    let root = workspace_root();
    let ci = read(&root, ".github/workflows/ci.yml");
    // The three required gates from the TODO must be wired.
    assert!(ci.contains("cargo fmt"), "CI must gate on rustfmt");
    assert!(ci.contains("cargo clippy"), "CI must gate on clippy");
    assert!(ci.contains("cargo test"), "CI must gate on tests");
}

#[test]
fn gitignore_covers_rust_and_node() {
    let root = workspace_root();
    let ignore = read(&root, ".gitignore");
    // Rust build output.
    assert!(ignore.contains("target"), ".gitignore must ignore target/");
    // Node / frontend output for the React apps served by sc-server.
    assert!(
        ignore.contains("node_modules"),
        ".gitignore must ignore node_modules/"
    );
}
