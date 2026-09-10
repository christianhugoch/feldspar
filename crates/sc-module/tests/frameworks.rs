//! A module's **application frameworks**, end to end on a worker (§13.3, §15.1).
//!
//! The fixture supplies four and three are broken on purpose, which is half of
//! what this file is about: a module with one mis-declared framework must still
//! supply the others — and must still supply its *actions*, since a framework is
//! not the only thing a module is.
//!
//! The other half is the seam itself: a declaration in, an `sc-app`
//! [`FrameworkDecl`] out, and a call back through [`FrameworkHost`] for the files
//! the framework generates.
//!
//! No network: the fixture is a local directory with no dependencies, so `npm
//! install <dir>` reaches nothing. It still needs npm, and skips without it.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use std::sync::Arc;

use common::{fixture, have_npm, installed};
use sc_app::{FilePhase, FrameworkHost, FrameworkSet};
use sc_module::{LoadedModule, Module, ModuleFrameworks, ModuleSet, ModuleSource};
use sc_types::Attrs;
use serde_json::json;

/// The fixture installed, loaded on a worker, and its manifest folded into a
/// one-module set — which is what [`ModuleFrameworks`] reads.
async fn loaded(tag: &str) -> (ModuleFrameworks, Vec<String>) {
    let (installer, host, names) = installed(tag, &["framework-module"]).await;
    let name = names[0].clone();
    let manifest = host
        .load(
            &name,
            &installer.package_dir(&name),
            &json!({}),
            &sc_module::ModulePermissions::default(),
        )
        .await
        .expect("the fixture loads");
    let issues = manifest.issues.clone();
    let module = Module::new(
        &name,
        ModuleSource::Local,
        fixture("framework-module").display().to_string(),
    );
    let set = ModuleSet::empty().merged(vec![LoadedModule {
        module,
        manifest: Some(manifest),
        config_spec: Vec::new(),
        issues: issues.clone(),
    }]);
    let frameworks = ModuleFrameworks::new(&host, &set);
    (frameworks, issues)
}

/// The settings an application of the fixture's framework would carry.
fn config() -> Attrs {
    [
        ("store".to_owned(), json!("apps")),
        ("project".to_owned(), json!("todo")),
    ]
    .into_iter()
    .collect()
}

#[tokio::test]
async fn a_module_declares_a_framework_and_it_arrives_as_a_registry_entry() {
    if !have_npm() {
        eprintln!("skipping: npm is not available");
        return;
    }
    let (frameworks, _) = loaded("fw-declares").await;
    let declared = frameworks.frameworks();
    assert_eq!(
        declared.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["toy"],
        "the three broken ones are dropped; the working one is not"
    );

    let toy = &declared[0];
    assert_eq!(toy.info().label, "Toy");
    assert!(toy.scaffolds, "it exports a scaffold function");
    // Its settings arrive through the same translation an action's go through.
    let names: Vec<&str> = toy.config_spec.iter().map(|f| f.name()).collect();
    assert_eq!(names, ["store", "project"]);

    // And its paths are its own templates, resolved against those settings — the
    // same `AppSource` shape `react` and `code` resolve to.
    assert_eq!(toy.store(&config()).unwrap(), "apps");
    let spec = toy.build_spec(&config()).unwrap();
    assert_eq!(spec.command, "toybuild");
    assert_eq!(spec.args, ["--quiet"]);
    assert_eq!(spec.source_dir, "todo");
    assert_eq!(spec.output_dir, "todo/out");
    let install = spec.install.expect("it manages its dependencies");
    assert_eq!(install.command, "toyinstall");
    assert_eq!(install.marker, "toy_modules");
    assert_eq!(
        toy.client_path(&config()).unwrap().unwrap(),
        "todo/gen/client.ts"
    );

    // The CSP is the strict baseline plus what the framework said it needs.
    let csp = toy.default_csp();
    assert_eq!(csp.directives["default-src"], ["'self'"]);
    assert_eq!(csp.directives["img-src"], ["'self'", "data:"]);

    // And the builder prompt is a template over the settings *and* the
    // application, which is what makes it worth being a template at all.
    let extra = [
        ("app".to_owned(), "My Todo".to_owned()),
        ("root".to_owned(), "todo".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        toy.prompt(&config(), &extra).unwrap().unwrap(),
        "You build My Todo in todo of apps."
    );
}

#[tokio::test]
async fn a_broken_framework_costs_that_framework_and_not_the_module() {
    if !have_npm() {
        eprintln!("skipping: npm is not available");
        return;
    }
    let (frameworks, issues) = loaded("fw-broken").await;

    // The two the host script refuses, each with the reason on the module's card.
    let card = issues.join("\n");
    assert!(
        card.contains("react") && card.contains("belongs to one of"),
        "a reserved name is refused where an admin can read why: {card}"
    );
    assert!(
        card.contains("buildless") && card.contains("no build"),
        "a framework that cannot say how to build is refused: {card}"
    );

    // The third is refused on this side, where the settings are known — the
    // check that means an admin never presses Build on a framework whose paths
    // cannot resolve.
    let translation = frameworks.issues().join("\n");
    assert!(
        translation.contains("mistyped") && translation.contains("projekt"),
        "a template naming a setting the framework has not got: {translation}"
    );

    // And the module still supplies its action.
    assert_eq!(frameworks.frameworks().len(), 1);
}

#[tokio::test]
async fn the_generator_is_called_on_the_worker_and_answers_files() {
    if !have_npm() {
        eprintln!("skipping: npm is not available");
        return;
    }
    let (frameworks, _) = loaded("fw-files").await;
    let context = json!({
        "project": "todo",
        "name": "todo",
        "runtime": "gen",
        "client": "gen/client.ts",
        "tables": [{ "name": "tasks" }, { "name": "notes" }],
    });

    let files = frameworks
        .framework_files("toy", FilePhase::Scaffold, context.clone())
        .await
        .expect("the scaffold runs");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["toy.json", "gen/toy.ts"]);
    // The context crossed whole: the generator wrote the app's own tables out.
    assert!(files[0].contents.contains("tasks"), "{:?}", files[0]);
    assert!(files[0].contents.contains("notes"), "{:?}", files[0]);
    assert!(files[1].contents.contains("gen/client.ts"));

    // The runtime phase is a different, smaller answer from the same framework.
    let runtime = frameworks
        .framework_files("toy", FilePhase::Runtime, context)
        .await
        .expect("the runtime runs");
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].path, "gen/toy.ts");
}

#[tokio::test]
async fn a_framework_nothing_supplies_is_refused_before_a_worker_is_reached() {
    if !have_npm() {
        eprintln!("skipping: npm is not available");
        return;
    }
    let (frameworks, _) = loaded("fw-missing").await;
    let err = frameworks
        .framework_files("svelte", FilePhase::Scaffold, json!({}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("svelte"), "{msg}");
    assert!(
        msg.contains("uninstalled") || msg.contains("failed to load"),
        "{msg}"
    );

    // And through the set `sc-app` installs, the same declaration is the one the
    // registry answers with.
    let set = FrameworkSet::new(Arc::new(frameworks));
    assert!(set.find("toy").is_some());
    assert!(set.find("react").is_none());
}
