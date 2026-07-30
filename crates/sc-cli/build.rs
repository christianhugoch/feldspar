//! Wire the `ui/admin` SPA and `ui/ide` builds into the server binary's build.
//!
//! Building either bundle needs a Node toolchain, which the Rust-only build and CI
//! paths must not require. So each build is **opt-in**: set `SC_BUILD_ADMIN=1` when
//! building the `saltcorn` binary and this script runs the `ui/admin` production
//! build (`npm ci && npm run build`) and records the output directory in the
//! `SC_ADMIN_BUNDLE_DIR` compile-time env; `SC_BUILD_IDE=1` does the same for the
//! file-store IDE (design §12.1) and `SC_IDE_BUNDLE_DIR`. The binary then defaults
//! `--static-dir` and `--ide-dir` to those paths (see `main.rs`), so a bundle-built
//! `saltcorn serve` serves them out of the box. Without the flags the script is a
//! no-op and the server falls back to the minimal bootstrap documents.
//!
//! The two are separate flags, not one, because they cost very differently: the IDE
//! bundles VS Code, so building it is far slower than building the SPA and an
//! operator who does not want the IDE should not pay for it.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    build_bundle("ui/admin", "SC_BUILD_ADMIN", "SC_ADMIN_BUNDLE_DIR", "admin");
    build_bundle("ui/ide", "SC_BUILD_IDE", "SC_IDE_BUNDLE_DIR", "IDE");
}

/// Build one UI bundle when its opt-in flag is set, and export its `dist` path.
fn build_bundle(subdir: &str, flag: &str, env_var: &str, label: &str) {
    println!("cargo:rerun-if-env-changed={flag}");

    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(subdir);

    // Rebuild the bundle when its source (or its build config) changes.
    for entry in [
        "src",
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "index.html",
    ] {
        println!("cargo:rerun-if-changed={}", ui.join(entry).display());
    }

    if std::env::var_os(flag).is_none() {
        return;
    }

    if !ui.join("package.json").exists() {
        panic!("{flag} set but {} has no package.json", ui.display());
    }

    run(&ui, ["ci"]);
    run(&ui, ["run", "build"]);

    let dist = ui.join("dist");
    if !dist.join("main.js").exists() {
        panic!(
            "{label} build did not produce {}",
            dist.join("main.js").display()
        );
    }
    // Canonicalize so the embedded path is absolute regardless of run-time CWD.
    let dist = dist.canonicalize().unwrap_or(dist);
    println!("cargo:rustc-env={env_var}={}", dist.display());
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
