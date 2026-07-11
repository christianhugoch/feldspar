//! Wire the `ui/admin` SPA build into the server binary's build.
//!
//! Building the admin bundle needs a Node toolchain, which the Rust-only build
//! and CI paths must not require. So the SPA build is **opt-in**: set
//! `SC_BUILD_ADMIN=1` when building the `saltcorn` binary and this script runs
//! the `ui/admin` production build (`npm ci && npm run build`) and records the
//! output directory in the `SC_ADMIN_BUNDLE_DIR` compile-time env. The binary
//! then defaults `--static-dir` to that path (see `main.rs`), so a bundle-built
//! `saltcorn serve` serves the SPA out of the box. Without the flag the script
//! is a no-op and the server falls back to the minimal bootstrap document.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SC_BUILD_ADMIN");

    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/admin");

    // Rebuild the bundle when the SPA source (or its build config) changes.
    for entry in [
        "src",
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "index.html",
    ] {
        println!("cargo:rerun-if-changed={}", ui.join(entry).display());
    }

    if std::env::var_os("SC_BUILD_ADMIN").is_none() {
        return;
    }

    if !ui.join("package.json").exists() {
        panic!(
            "SC_BUILD_ADMIN set but {} has no package.json",
            ui.display()
        );
    }

    run(&ui, ["ci"]);
    run(&ui, ["run", "build"]);

    let dist = ui.join("dist");
    if !dist.join("main.js").exists() {
        panic!(
            "admin build did not produce {}",
            dist.join("main.js").display()
        );
    }
    // Canonicalize so the embedded path is absolute regardless of run-time CWD.
    let dist = dist.canonicalize().unwrap_or(dist);
    println!("cargo:rustc-env=SC_ADMIN_BUNDLE_DIR={}", dist.display());
}

/// Run `npm <args>` in `dir`, failing the build loudly on any error.
fn run<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to launch `npm {}`: {e}", args.join(" ")));
    if !status.success() {
        panic!("`npm {}` failed with {status}", args.join(" "));
    }
}
