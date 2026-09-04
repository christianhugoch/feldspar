//! The module runtime's **startup snapshot**, built here so that the binary
//! carries every byte of JavaScript it will ever need.
//!
//! ## Why a build script at all
//!
//! `deno_core` does not embed the JavaScript its extensions are made of. An
//! `include_js_files!` entry records the **absolute path the file had on the
//! build machine** and the bytes are read from there — either while a snapshot
//! is being created, or, with no snapshot, at every runtime start
//! (`deno_core`'s `ExtensionFileSource::load`). `deno_runtime` declares all of
//! its own `esm` and `lazy_loaded_js` that way, and so do the twenty-odd
//! `deno_*` extension crates under it.
//!
//! A server built on one machine and run on another therefore has nothing to
//! read: the release tarball carries a binary, not a cargo registry. What that
//! looked like in production was a module worker that died in
//! `JsRuntime::new` with `Permission denied (os error 13)` — the unit's
//! `ProtectHome=true` masking the build machine's `/home`, so the missing path
//! could not even be reported as missing — and an install that reported
//! "supplying nothing this version of Saltcorn loads".
//!
//! So the snapshot is not an optimisation here. It is what makes the tarball
//! portable, and `crates/sc-module/src/deno/wiring.rs` will not compile without
//! the two files this script writes into `OUT_DIR`:
//!
//! - `MODULE_RUNTIME_SNAPSHOT.bin` — the V8 startup snapshot, `include_bytes!`d.
//! - `residual_lazy_sources.rs` — the `lazy_loaded_*` sources the snapshot did
//!   **not** consume, `include_str!`d as `(specifier, source)` pairs. Deno's
//!   own `CreateRuntimeSnapshotOutput` documents this residue; a file left out
//!   of it is a `core.loadExtScript()` that fails on a host with no build tree,
//!   which is the bug this script exists to prevent, so the set is checked
//!   rather than assumed.
//!
//! Nothing here runs unless the `deno-host` feature is on: a build without the
//! module runtime has no snapshot to make and must not pay for one.

// A build script is the one place in this workspace where panicking is the
// correct behaviour: there is no caller to return an error to, and a snapshot
// that could not be made must stop the build rather than produce a binary whose
// module runtime is silently broken on every host but this one.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a failed snapshot has to fail the build"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "deno-host")]
    snapshot::write();
}

#[cfg(feature = "deno-host")]
mod snapshot {
    use std::path::{Path, PathBuf};

    use deno_runtime::snapshot::{LazyExtensionFileKind, create_runtime_snapshot};

    pub fn write() {
        let out = PathBuf::from(
            std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR for a build script"),
        );
        let target = std::env::var("TARGET").expect("cargo sets TARGET for a build script");
        let host = std::env::var("HOST").expect("cargo sets HOST for a build script");
        // **A snapshot is machine code's cousin**: V8 serialises it for the
        // architecture the snapshotting process runs on, and a build script runs
        // on the *host*. Cross-compiling would therefore bake an x86-64 snapshot
        // into an aarch64 binary, which fails at the first module — late, on
        // somebody else's machine, with a message about V8 internals. Refused
        // here instead, where the message can name the remedy.
        // (`scripts/build-static.sh --target` is the way to hit this.)
        if architecture(&target) != architecture(&host) {
            panic!(
                "cannot cross-compile the module runtime: its V8 startup snapshot is built by \
                 this build script, which runs on {host}, and would not load on {target}. Build \
                 on a {target} machine — `scripts/build-static.sh --docker` with a matching \
                 --platform does this — or turn the `deno-host` feature off for this target."
            );
        }
        let output = create_runtime_snapshot(
            out.join("MODULE_RUNTIME_SNAPSHOT.bin"),
            // What the worker answers `Deno.build.target` with. `ts_version` is
            // "n/a" because no TypeScript is compiled at runtime.
            deno_runtime::ops::bootstrap::SnapshotOptions {
                target: target.clone(),
                ..Default::default()
            },
            // No extension of ours goes into the snapshot: the seam
            // (`__scDone`, `__scFail`, `__scLog`) is installed on the global
            // *after* the worker is built, and the host script is an ordinary
            // main module.
            Vec::new(),
        );

        // What the snapshot swallowed does not need embedding again; what it
        // left behind does, or the worker reads it off the build machine.
        let consumed: std::collections::HashSet<&str> = output
            .consumed_lazy_specifiers
            .iter()
            .map(String::as_str)
            .collect();
        let mut js = Vec::new();
        let mut esm = Vec::new();
        for file in &output.lazy_extension_files {
            if consumed.contains(file.specifier.as_str()) {
                continue;
            }
            let entry = residual(&file.specifier, &file.path);
            match file.kind {
                LazyExtensionFileKind::Js => js.push(entry),
                LazyExtensionFileKind::Esm => esm.push(entry),
            }
        }

        let header = "// Generated by build.rs: the `lazy_loaded_*` sources the startup \
                      snapshot did not\n// consume, embedded so that no host needs the build \
                      machine's files.\n";
        let table = format!(
            "{header}static RESIDUAL_LAZY_JS: &[(&str, &str)] = &[\n{}];\n\
             static RESIDUAL_LAZY_ESM: &[(&str, &str)] = &[\n{}];\n",
            js.concat(),
            esm.concat(),
        );
        std::fs::write(out.join("residual_lazy_sources.rs"), table)
            .expect("writing the residual lazy-source table");

        for path in &output.files_loaded_during_snapshot {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    /// The architecture half of a target triple — the part a V8 snapshot is tied
    /// to. `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl` share a
    /// snapshot; `x86_64` and `aarch64` do not.
    fn architecture(triple: &str) -> &str {
        triple.split('-').next().unwrap_or(triple)
    }

    /// One `(specifier, source)` pair, with the source read here rather than
    /// `include_str!`d: a `.ts` lazy file has to be transpiled, and the
    /// transpiler is only in the build script.
    fn residual(specifier: &str, path: &Path) -> String {
        let source = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "the module runtime's snapshot needs {} and cannot read it: {e}",
                path.display()
            )
        });
        let (code, _map) = deno_runtime::transpile::maybe_transpile_source(
            deno_runtime::deno_core::ModuleName::from(specifier.to_owned()),
            deno_runtime::deno_core::ModuleCodeString::from(source),
        )
        .unwrap_or_else(|e| panic!("transpiling {specifier} for the snapshot residue: {e}"));
        format!("    ({:?}, {:?}),\n", specifier, code.as_str())
    }
}
