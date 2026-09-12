//! Saltcorn UI's **view runtime** on the module worker (TODO "Saltcorn UI"
//! Phase 3): the built-in `@feldspar/saltcorn-ui`, the library a plugin's
//! `require` reaches, and the view snapshot crossing once per generation.
//!
//! Every test here needs the built bundle (`ui/saltcorn-ui/dist/view-runtime.js`)
//! and skips, saying so, without it. The two that install a fixture also need
//! npm. None needs `node`.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{closed, have_npm, installed, temp_root};
use sc_app::AppId;
use sc_error::ErrorKind;
use sc_module::{BUILTIN_VIEW_RUNTIME, CallHosts, ModuleHost, ModuleViewRuntime};
use sc_viewpattern::{
    BUILTIN_PATTERNS, Page, View, ViewContext, ViewRequest, ViewRuntime, ViewSnapshot,
};
use serde_json::json;

/// The built view runtime, if this checkout has one.
fn bundle() -> Option<PathBuf> {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(sc_viewpattern::BUNDLE_DIR_IN_CHECKOUT)
        .join(sc_viewpattern::VIEW_RUNTIME_FILE);
    file.is_file().then_some(file)
}

const NO_BUNDLE: &str =
    "the Saltcorn UI bundle is not built (run `npm ci && npm run build` in ui/saltcorn-ui)";

/// A snapshot of `json` for `application` at `generation`.
fn snapshot(application: AppId, generation: u64, json: &serde_json::Value) -> ViewSnapshot {
    ViewSnapshot::from_json(application, generation, json.to_string())
}

#[tokio::test]
async fn the_built_in_view_runtime_loads_with_no_modules_root_at_all() {
    skip_without!(bundle().is_some(), NO_BUNDLE);
    let root = temp_root("view-runtime-no-root");
    assert!(!root.exists());
    let host = Arc::new(ModuleHost::new(&root).with_view_runtime(bundle()));
    let runtime = ModuleViewRuntime::new(&host);

    // The manifest: v1's six patterns, described as data.
    let patterns = runtime.patterns().await.unwrap();
    let mut names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    let mut builtin = BUILTIN_PATTERNS.to_vec();
    builtin.sort_unstable();
    assert_eq!(names, builtin);
    let list = patterns.iter().find(|p| p.name == "List").unwrap();
    assert_eq!(list.label, "List");
    assert!(list.table_required);
    assert_eq!(list.view_quantity.as_deref(), Some("Many"));
    assert!(list.routes.iter().any(|r| r == "run_action"), "{list:?}");
    // The step *names*, which need a `req` and nothing else.
    assert_eq!(
        list.steps.first().map(String::as_str),
        Some("Columns"),
        "{list:?}"
    );
    // Nothing was installed to get here.
    assert!(!root.join("node_modules").exists());

    // One configuration step, as a call: List's first is a builder, whose layout
    // this version shows read-only, so it has no form.
    let app = AppId::new();
    let views = snapshot(
        app,
        1,
        &json!({ "application": { "name": "Books" }, "views": [], "pages": [] }),
    );
    let request = ViewRequest::default();
    let step = runtime
        .config_step(
            "List",
            Some("books"),
            0,
            &json!({}),
            ViewContext::bare(&views, &request),
        )
        .await
        .unwrap();
    assert_eq!(step.name, "Columns");
    assert!(step.builder);
    assert!(step.count >= 1, "{step:?}");
    assert!(step.form.is_null(), "{step:?}");

    // And a page, through `@saltcorn/markup`'s own `renderLayout`.
    let views = snapshot(
        app,
        2,
        &json!({
            "application": { "name": "Books" },
            "views": [],
            "pages": [{ "name": "Hello", "layout": { "type": "blank", "contents": "Hello, books" } }],
        }),
    );
    let page = Page::new(app, "Hello");
    let out = runtime
        .render_page(&page, ViewContext::bare(&views, &request))
        .await
        .unwrap();
    assert!(
        out.body
            .as_str()
            .is_some_and(|html| html.contains("Hello, books")),
        "{:?}",
        out.body
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// §4: the JSON crosses when the worker does not hold the generation, and not
/// otherwise. Told by lying: the second call carries different JSON at the same
/// generation, and the worker must still answer from the first.
#[tokio::test]
async fn a_view_snapshot_crosses_once_per_generation_and_a_throw_names_the_view() {
    skip_without!(bundle().is_some(), NO_BUNDLE);
    let root = temp_root("view-runtime-snapshot");
    let host = Arc::new(ModuleHost::new(&root).with_view_runtime(bundle()));
    let runtime = ModuleViewRuntime::new(&host);
    let app = AppId::new();
    let view = View::new(app, "List Books", "List", "books");
    let request = ViewRequest::default();
    let with_view = json!({
        "application": { "name": "BooksDB" },
        "views": [{
            "name": "List Books", "viewtemplate": "List",
            "table_id": "books", "table_name": "books", "configuration": {},
        }],
        "pages": [],
    });
    let without = json!({ "application": { "name": "BooksDB" }, "views": [], "pages": [] });

    // Generation 5, with the view. The pattern runs and fails — it was given no
    // database — and the failure is the application's, naming the view, with no
    // stack in it.
    let first = snapshot(app, 5, &with_view);
    let err = runtime
        .render(&view, &json!({}), ViewContext::bare(&first, &request))
        .await
        .unwrap_err();
    let message = err.to_string();
    assert_eq!(err.kind(), ErrorKind::Application, "{message}");
    assert!(
        message.contains("the view `List Books` (List)"),
        "{message}"
    );
    assert!(!message.contains("has no view named"), "{message}");
    assert!(
        !message.contains("    at "),
        "a stack reached the message: {message}"
    );

    // Generation 5 again, carrying JSON without the view. Not sent, so the worker
    // still has the view.
    let same = snapshot(app, 5, &without);
    let message = runtime
        .render(&view, &json!({}), ViewContext::bare(&same, &request))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        !message.contains("has no view named"),
        "the worker was sent a generation it already held: {message}"
    );

    // Generation 6: sent, and the view is gone.
    let moved = snapshot(app, 6, &without);
    let message = runtime
        .render(&view, &json!({}), ViewContext::bare(&moved, &request))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("has no view named List Books"),
        "{message}"
    );

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

/// §3.2: the runtime's name is reserved, and the reservation costs an installed
/// package that claims it nothing but the claim.
#[tokio::test]
async fn an_installed_module_claiming_the_runtimes_name_keeps_its_actions_and_takes_nothing() {
    skip_without!(have_npm(), "npm is not on the PATH");
    skip_without!(bundle().is_some(), NO_BUNDLE);
    let (installer, _, names) = installed("view-runtime-reserved", &["reserved-name-module"]).await;
    let name = &names[0];
    assert_eq!(name, BUILTIN_VIEW_RUNTIME);
    let host = Arc::new(ModuleHost::new(installer.root()).with_view_runtime(bundle()));

    let manifest = host
        .load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();
    assert!(
        manifest
            .issues
            .iter()
            .any(|issue| issue.contains("built-in Saltcorn UI view runtime")),
        "{:?}",
        manifest.issues
    );
    let actions: Vec<&str> = manifest.actions.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(actions, ["still_here"]);

    // The built-in is still the runtime…
    let runtime = ModuleViewRuntime::new(&host);
    assert_eq!(
        runtime.patterns().await.unwrap().len(),
        BUILTIN_PATTERNS.len()
    );
    // …and the package is still a module, before and after the runtime started.
    let value = host
        .run(name, "still_here", json!({}), CallHosts::default())
        .await
        .unwrap();
    assert_eq!(value["still"], json!("here"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}

/// §3.3a and §5: one `require` table. A plugin's `@saltcorn/markup/tags` is v1's
/// own, and the names a plugin feature-detects are absent rather than truthy
/// stubs.
#[tokio::test]
async fn a_plugin_requires_the_real_library_and_feature_detects_what_is_absent() {
    skip_without!(have_npm(), "npm is not on the PATH");
    skip_without!(bundle().is_some(), NO_BUNDLE);
    let (installer, without_runtime, names) =
        installed("view-runtime-library", &["library-module"]).await;
    let name = &names[0];

    let host = Arc::new(ModuleHost::new(installer.root()).with_view_runtime(bundle()));
    host.load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();
    let value = host
        .run(name, "reach_the_library", json!({}), CallHosts::default())
        .await
        .unwrap();
    assert_eq!(value["html"], json!("<div class=\"card\">hi</div>"));
    // `features?.public_user_role || 10`: absent, so the default.
    assert_eq!(value["public_user_role"], json!(10));
    // Present-and-undefined, not missing.
    assert_eq!(value["features_present"], json!(true));
    // `runCollabEvents ? … : []`: absent, so the fallback.
    assert_eq!(value["collab"], json!("absent"));
    // And a name that is neither implemented nor absent is still a stub.
    assert_eq!(value["get_state"], json!("function"));
    host.shutdown().await;

    // The same plugin on a host with no view runtime: stubs, which throw naming
    // what was called.
    without_runtime
        .load(name, &installer.package_dir(name), &json!({}), &closed())
        .await
        .unwrap();
    let err = without_runtime
        .run(name, "reach_the_library", json!({}), CallHosts::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("markup/tags.div"), "{err}");
    without_runtime.shutdown().await;
    let _ = std::fs::remove_dir_all(installer.root());
}
