//! Workspace invariants from the GOALS pivot to a React admin UI over a typed
//! API (technical design §12–§13): the `sc-markup` symbolic-HTML crate is dropped
//! entirely, and the MVP catch-all fieldview path emits plain text, not a markup
//! tree. Asserted here so the decision cannot silently regress as later phases
//! build on `sc-server` / `sc-api` / `sc-app`.

use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root, derived from this crate's manifest dir (`tests/harness`,
/// two levels below the root).
fn workspace_root() -> PathBuf {
    match Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) {
        Some(p) => p.to_path_buf(),
        None => panic!("tests/harness should be two levels below the workspace root"),
    }
}

fn read(root: &Path, rel: &str) -> String {
    match fs::read_to_string(root.join(rel)) {
        Ok(s) => s,
        Err(e) => panic!("read {rel}: {e}"),
    }
}

/// Task 1: the `sc-markup` crate is dropped — no directory, no manifest reference.
#[test]
fn no_sc_markup_crate() {
    let root = workspace_root();

    assert!(
        !root.join("crates/sc-markup").exists(),
        "crates/sc-markup must not exist: the symbolic-HTML crate is dropped (design §12)"
    );

    let manifest = read(&root, "Cargo.toml");
    assert!(
        !manifest.contains("sc-markup"),
        "workspace Cargo.toml must not reference sc-markup"
    );
}

/// Tasks 2 & 3: the reconciled sources describe no symbolic-markup / server-HTML
/// model — the admin UI renders in React and the catch-all path returns plain
/// text (design §6.3, §12).
#[test]
fn reconciled_sources_have_no_markup_language() {
    let root = workspace_root();
    for rel in [
        "crates/sc-types/src/catchall.rs",
        "crates/sc-server/src/lib.rs",
        "crates/sc-api/src/lib.rs",
        "crates/sc-app/src/lib.rs",
    ] {
        let low = read(&root, rel).to_lowercase();
        assert!(
            !low.contains("markup") && !low.contains("symbolic"),
            "{rel} still contains dropped symbolic-markup language (design §6.3/§12)"
        );
    }
}
