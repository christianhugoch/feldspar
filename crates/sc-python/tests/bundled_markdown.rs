//! `plugins/markdown` — the bundled Python module — installed from the
//! directory it ships in and asked for the functions it supplies.
//!
//! The Python half of the bundled catalog (`plugins/README.md`,
//! `sc_module::bundled`). `python_modules.rs` proves the module *tier* with a
//! fixture built to have no requirements; this proves the **shipped** one, and
//! the difference is exactly the part that matters here: `feldspar-markdown`
//! declares a dependency on `markdown`, which is deliberately not in the release
//! tarball, so installing it is the moment pip fetches something.
//!
//! `#[ignore]`, therefore: this reaches PyPI, twice — once for pip's build
//! isolation (setuptools) and once for the dependency. Run it with
//! `cargo test -p sc-python --features python-host --test it bundled_markdown -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sc_error::Result;
use sc_module::{BundledModules, ModuleLanguage};
use sc_python::pymodule::PyModuleHost;
use sc_python::{PythonEnv, PythonEnvironment, PythonRuntime, PythonSource};
use serde_json::json;

/// Say why a test did nothing, so a skip is visible rather than looking like a
/// pass.
macro_rules! skip_without {
    ($cond:expr, $why:expr) => {
        if !$cond {
            eprintln!("skipping: {}", $why);
            return Ok(());
        }
    };
}

async fn have_toolchain() -> bool {
    let bin = PathBuf::from(sc_python::DEFAULT_PYTHON_BIN);
    sc_python::have_python(&bin).await && sc_python::have_pip(&bin).await
}

fn environment_at(dir: &Path) -> PythonEnvironment {
    PythonEnvironment::new(
        &PythonEnv {
            dir: Some(dir.to_path_buf()),
            bin: None,
        },
        None,
    )
    .expect("an explicit --python-dir needs no default")
}

#[tokio::test]
#[ignore = "installs the bundled module, which downloads markdown from PyPI"]
async fn the_bundled_markdown_module_installs_from_the_catalog_and_supplies_its_functions()
-> Result<()> {
    skip_without!(
        have_toolchain().await,
        "python3 with pip is not on the PATH"
    );
    skip_without!(
        cfg!(feature = "python-host"),
        "built without an interpreter"
    );

    // Installed the way the Install button installs it: the catalog resolves the
    // id to the directory it ships in, and pip is handed a local directory.
    let catalog = BundledModules::discover(None);
    let entry = catalog.get("markdown").expect("the Markdown module ships");
    assert_eq!(entry.language, ModuleLanguage::Python);

    let dir = std::env::temp_dir().join(format!("sc-bundled-markdown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let installed = environment_at(&dir)
        .install(PythonSource::Local, &entry.directory.display().to_string())
        .await?;
    // The name the row is keyed by is the distribution's own, and the manifest
    // agrees with it — which is what makes "already installed" answerable.
    assert_eq!(installed.name, entry.name);

    let python = Arc::new(PythonRuntime::new().with_env(PythonEnv {
        dir: Some(dir.clone()),
        bin: None,
    }));
    let host = Arc::new(PyModuleHost::new(python));
    let manifest = host.load(&entry.name, &json!({})).await?;
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);

    // Two functions, reached through the distribution's `saltcorn.plugins` entry
    // point — which is the thing `feldspar_markdown/__init__.py` is empty to
    // prove.
    let mut names: Vec<&str> = manifest.functions.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["markdown_to_html", "markdown_to_text"]);
    assert!(manifest.actions.is_empty(), "{:?}", manifest.actions);

    // And they work, which is the whole reason the dependency was downloaded.
    let html = host
        .call(
            &entry.name,
            "markdown_to_html",
            vec![json!("# Title\n\nA *word*.")],
        )
        .await?;
    let html = html.as_str().unwrap_or_default();
    assert!(html.contains("<h1>Title</h1>"), "{html}");
    assert!(html.contains("<em>word</em>"), "{html}");
    // Nothing in is nothing out, rather than a rendered empty document.
    assert_eq!(
        host.call(&entry.name, "markdown_to_html", vec![json!("")])
            .await?,
        json!("")
    );

    // The plain-text one: the markup gone, the words on one line, and a cut that
    // ends on a word.
    let text = host
        .call(
            &entry.name,
            "markdown_to_text",
            vec![
                json!("# Title\n\nA longer sentence than the limit."),
                json!(0),
            ],
        )
        .await?;
    let text = text.as_str().unwrap_or_default().to_owned();
    assert!(!text.contains('<'), "{text}");
    assert!(text.starts_with("Title A longer sentence"), "{text}");

    let cut = host
        .call(
            &entry.name,
            "markdown_to_text",
            vec![json!("Title A longer sentence than the limit."), json!(12)],
        )
        .await?;
    let cut = cut.as_str().unwrap_or_default().to_owned();
    assert!(cut.ends_with('…'), "{cut}");
    assert!(cut.len() <= "Title A longer".len() + 3, "{cut}");

    host.unload(&entry.name).await;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
