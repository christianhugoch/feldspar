//! Wire the admin UI into the server binary's build: the `ui/admin` SPA **and**
//! the `ui/ide` file-store IDE (design §12.1), which are one thing to an operator.
//!
//! Building them needs a Node toolchain, which the Rust-only build and CI paths
//! must not require. So the build is **opt-in**: set `SC_BUILD_ADMIN=1` when
//! building the `saltcorn` binary and this script runs both production builds
//! (`npm ci && npm run build`) and records the output directories in the
//! `SC_ADMIN_BUNDLE_DIR` and `SC_IDE_BUNDLE_DIR` compile-time envs. The binary
//! then serves both with no flags at all (see `main.rs`). Without the variable the
//! script is a no-op.
//!
//! **One variable, not two.** The IDE is not a separate product an operator
//! chooses: it is where they edit an application's source, reached from the admin
//! UI, and a build that produced the admin UI without it would leave a button
//! leading nowhere. It costs a slower build, which is the right price for not
//! having a half-built admin UI as a state anyone can be in.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SC_BUILD_ADMIN");
    let build = std::env::var_os("SC_BUILD_ADMIN").is_some();
    build_bundle("ui/admin", "SC_ADMIN_BUNDLE_DIR", "admin UI", build);
    build_bundle("ui/ide", "SC_IDE_BUNDLE_DIR", "file-store IDE", build);
}

/// Build one UI bundle and export its `dist` path, when asked to.
///
/// The `rerun-if-changed` lines are printed either way: they are what tells cargo
/// a bundle needs rebuilding, and they must not depend on whether this particular
/// build was the one that built it.
fn build_bundle(subdir: &str, env_var: &str, label: &str, build: bool) {
    let ui = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(subdir);

    for entry in [
        "src",
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "index.html",
    ] {
        println!("cargo:rerun-if-changed={}", ui.join(entry).display());
    }

    if !build {
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
            "the {label} build did not produce {}",
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
