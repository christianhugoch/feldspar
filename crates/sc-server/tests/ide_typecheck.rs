//! `ui/ide` must type-check, and it must type-check against the *generated* client
//! it will call the file-store API through (design §12.1).
//!
//! The IDE is a second bundle with its own `tsconfig.json`, so nothing about
//! `ui/admin`'s type-check covers it. Like the SPA's equivalent, this **skips**
//! rather than fails when the Node toolchain has not been installed under
//! `ui/ide`, so a Rust-only checkout stays green; run it after `npm install` in
//! `ui/ide`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn ide_type_checks() {
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/ide");
    let tsc = ui.join("node_modules/.bin/tsc");
    if !tsc.exists() {
        eprintln!(
            "skipping: {} not found. Run `npm install` in ui/ide first.",
            tsc.display()
        );
        return;
    }

    let output = Command::new(&tsc)
        .args(["--noEmit", "-p", "tsconfig.json"])
        .current_dir(&ui)
        .output()
        .unwrap_or_else(|e| panic!("failed to run tsc at {}: {e}", tsc.display()));

    assert!(
        output.status.success(),
        "ui/ide failed to type-check:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The bundle's entry points are a contract with the server, not a build detail:
/// `IDE_BOOTSTRAP_HTML` references `/ide/main.js` and `/ide/main.css` by name, so
/// the Vite config has to keep emitting exactly those.
#[test]
fn the_bundle_pins_the_entry_points_the_server_serves() {
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/ide");
    let config = std::fs::read_to_string(ui.join("vite.config.ts")).expect("read vite.config.ts");
    assert!(config.contains("base: \"/ide/\""));
    assert!(config.contains("entryFileNames: \"main.js\""));
    assert!(sc_server::IDE_BOOTSTRAP_HTML.contains("/ide/main.js"));
    assert!(sc_server::IDE_BOOTSTRAP_HTML.contains("/ide/main.css"));
}
