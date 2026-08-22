//! The `deno_runtime` startup snapshot, built once here rather than at every
//! worker start (specification §6).
//!
//! Without it a worker evaluates — and *transpiles*, because the runtime's own
//! extension sources are TypeScript — the whole of `deno_runtime`'s JavaScript
//! before it can run a line of `module-host.mjs`. Phase 0 measured that at
//! 159 ms of worker construction against 9.7 ms with a snapshot, and a worker
//! restart is on the `process.exit()` road: the difference is between a restart
//! nobody notices and a hiccup.
//!
//! `create_runtime_snapshot` cannot put everything in the V8 blob. What it
//! leaves — the `lazy_loaded_js`/`lazy_loaded_esm` sources it did not consume,
//! which is where `node:console` and friends live — has to be embedded in the
//! binary and handed back through `WorkerOptions::residual_lazy_*_sources`.
//! That is the generated table below, and the sources go in **transpiled**: the
//! snapshot transpiles what it consumes and nothing transpiles the rest, so
//! skipping this step is a `SyntaxError` on the first `node:` import.
//!
//! Nothing happens here without the `deno-host` feature: a default build of this
//! crate does not link `deno_runtime` at all, which is what keeps it out of
//! every other crate's test link.

fn main() {
    // Cargo re-runs a build script whenever *any* file in the package changes
    // unless it is told what to watch. The snapshot depends only on the
    // `deno_runtime` version, which is a rebuild of the build script itself.
    println!("cargo:rerun-if-changed=build.rs");
    snapshot();
}

#[cfg(not(feature = "deno-host"))]
fn snapshot() {}

#[cfg(feature = "deno-host")]
fn snapshot() {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use deno_runtime::snapshot::LazyExtensionFileKind;

    /// Stop the build with a sentence, rather than a panic's backtrace: a build
    /// script's failure is read by whoever is trying to compile the server, and
    /// what they need is the file that would not transpile.
    fn fail(what: &str) -> ! {
        println!("cargo:warning=building the module runtime snapshot: {what}");
        eprintln!("sc-module: building the module runtime snapshot: {what}");
        std::process::exit(1)
    }

    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        fail("cargo did not set OUT_DIR")
    };
    let output = deno_runtime::snapshot::create_runtime_snapshot(
        out_dir.join("RUNTIME_SNAPSHOT.bin"),
        Default::default(),
        vec![],
    );

    let consumed: HashSet<String> = output.consumed_lazy_specifiers.into_iter().collect();
    let mut js = Vec::new();
    let mut esm = Vec::new();
    for file in output.lazy_extension_files {
        if consumed.contains(&file.specifier) {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&file.path) else {
            fail(&format!("{} could not be read", file.path.display()))
        };
        let transpiled = match deno_runtime::transpile::maybe_transpile_source(
            file.specifier.clone().into(),
            source.into(),
        ) {
            Ok((transpiled, _map)) => transpiled,
            Err(e) => fail(&format!("{} would not transpile: {e}", file.specifier)),
        };
        let emitted = out_dir
            .join("residual")
            .join(file.specifier.replace([':', '/'], "_"));
        let written = emitted
            .parent()
            .ok_or(())
            .and_then(|dir| std::fs::create_dir_all(dir).map_err(|_| ()))
            .and_then(|()| std::fs::write(&emitted, transpiled.as_str()).map_err(|_| ()));
        if written.is_err() {
            fail(&format!("{} could not be written", emitted.display()))
        }
        let entry = format!(
            "  ({:?}, include_str!({:?})),\n",
            file.specifier,
            emitted.display().to_string()
        );
        match file.kind {
            LazyExtensionFileKind::Js => js.push((file.specifier, entry)),
            LazyExtensionFileKind::Esm => esm.push((file.specifier, entry)),
        }
    }
    // The runtime looks these up by binary search and debug-asserts the table is
    // sorted, so sorting here is not tidiness.
    js.sort_by(|a, b| a.0.cmp(&b.0));
    esm.sort_by(|a, b| a.0.cmp(&b.0));

    let table = |name: &str, rows: &[(String, String)]| {
        let mut out = format!("pub static {name}: &[(&str, &str)] = &[\n");
        for (_, row) in rows {
            out.push_str(row);
        }
        out.push_str("];\n");
        out
    };
    let mut generated = String::new();
    generated.push_str(&table("RESIDUAL_LAZY_JS", &js));
    generated.push_str(&table("RESIDUAL_LAZY_ESM", &esm));
    if std::fs::write(out_dir.join("residual_lazy.rs"), generated).is_err() {
        fail("the residual source table could not be written")
    }
}
