//! Wire the admin UI into the server binary's build: the `ui/admin` SPA **and**
//! the `ui/ide` file-store IDE (design §12.1), which are one thing to an operator.
//!
//! The build is **on by default**: `cargo build -p sc-cli` runs both production
//! builds (`npm ci && npm run build`) and records the output directories in the
//! `SC_ADMIN_BUNDLE_DIR` and `SC_IDE_BUNDLE_DIR` compile-time envs, so the binary
//! serves both with no flags at all (see `main.rs`). A binary that does not serve
//! its own admin UI is the surprising outcome, not the expected one, which is why
//! it is the default rather than something to remember.
//!
//! That default needs a Node toolchain, and the Rust-only paths that do not have
//! one — CI's clippy/test jobs, a container without npm — turn it off with
//! **`SC_BUILD_ADMIN`** set to `0`, `false`, `False` or `FALSE`. Any other value
//! (`1`, `true`, unset) builds. Turned off, the script is a no-op beyond its
//! `rerun-if-changed` lines.
//!
//! The path recorded is the one in this checkout, which is the right answer for a
//! binary run from the tree it was built in and the wrong one for a binary that is
//! *packaged* — copied to another machine, where the checkout does not exist.
//! **`SC_BUNDLE_PREFIX`** is that case: set it to the directory the artifact will
//! be installed under (`scripts/build-static.sh` sets it to the install prefix) and
//! the recorded paths become `$SC_BUNDLE_PREFIX/ui/admin/dist` and
//! `$SC_BUNDLE_PREFIX/ui/ide/dist` — the same `ui/<name>/dist` layout, rooted where
//! the bundles will actually be. The bundles are still built here; only the path
//! compiled into the binary moves.
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
    println!("cargo:rerun-if-env-changed=SC_BUNDLE_PREFIX");
    let build = build_requested(std::env::var("SC_BUILD_ADMIN").ok().as_deref());
    build_bundle("ui/admin", "SC_ADMIN_BUNDLE_DIR", "admin UI", build);
    build_bundle("ui/ide", "SC_IDE_BUNDLE_DIR", "file-store IDE", build);
}

/// Decide whether to build the UI bundles from `SC_BUILD_ADMIN`'s value.
///
/// Opt **out**, not in: unset means build. Only the four spellings of "no" that
/// a shell or a CI file would plausibly carry — `0`, `false`, `False`, `FALSE` —
/// disable it. Everything else, including `1` and `true`, builds; a typo'd value
/// therefore fails towards the complete binary rather than silently producing one
/// with no admin UI, which is the failure that is hard to notice.
///
/// `pub` because `tests/build_script.rs` pulls this file in as a module to assert
/// the table above — a build script has no other way to be tested.
pub fn build_requested(value: Option<&str>) -> bool {
    !matches!(value, Some("0" | "false" | "False" | "FALSE"))
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
            "the {label} build is on (SC_BUILD_ADMIN is not 0/false) but {} has no package.json",
            ui.display()
        );
    }

    run(&ui, ["ci"]);
    run(&ui, ["run", "build"]);

    // `index.html` is what is checked for, rather than an entry script: entry
    // filenames carry a content hash, so `index.html` is the one output whose
    // name this script can know — and it is also the file the server serves, so
    // its absence is exactly the failure worth catching here.
    let dist = ui.join("dist");
    if !dist.join("index.html").exists() {
        panic!(
            "the {label} build did not produce {}",
            dist.join("index.html").display()
        );
    }
    // Canonicalize so the embedded path is absolute regardless of run-time CWD.
    let dist = dist.canonicalize().unwrap_or(dist);
    let recorded = recorded_dir(
        std::env::var("SC_BUNDLE_PREFIX").ok().as_deref(),
        subdir,
        &dist,
    );
    println!("cargo:rustc-env={env_var}={}", recorded.display());
}

/// The path to compile into the binary for a bundle built at `dist`.
///
/// Without `SC_BUNDLE_PREFIX` this is `dist` itself — the bundle in this checkout,
/// which is where a binary run from its own tree should look. With it, the same
/// `ui/<name>/dist` tail is re-rooted at the prefix the artifact will be installed
/// under, so the recorded path describes the *target* machine rather than this one.
/// A relative prefix is an operator error worth failing on rather than recording a
/// path that resolves against whatever directory the service happens to start in.
///
/// `pub` for the same reason `build_requested` is: `tests/build_script.rs` pulls
/// this file in as a module, which is a build script's only way to be tested.
pub fn recorded_dir(prefix: Option<&str>, subdir: &str, dist: &std::path::Path) -> PathBuf {
    match prefix {
        None => dist.to_path_buf(),
        Some(prefix) => {
            let prefix = PathBuf::from(prefix);
            assert!(
                prefix.is_absolute(),
                "SC_BUNDLE_PREFIX must be an absolute path, got {}",
                prefix.display()
            );
            prefix.join(subdir).join("dist")
        }
    }
}

/// Run `npm <args>` in `dir`, failing the build loudly on any error.
fn run<const N: usize>(dir: &std::path::Path, args: [&str; N]) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "failed to launch `npm {}`: {e}\n\
                 (set SC_BUILD_ADMIN=0 to build the binary without the admin UI)",
                args.join(" ")
            )
        });
    if !status.success() {
        panic!("`npm {}` failed with {status}", args.join(" "));
    }
}
