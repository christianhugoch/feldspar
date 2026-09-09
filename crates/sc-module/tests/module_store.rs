//! `_fd_modules` against a real database: the round trip, the uniqueness of a
//! package name, and what a row nobody can read says.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_module::{
    COL_LANGUAGE, COL_PERMISSIONS, MODULES_TABLE, Module, ModuleLanguage, ModulePermissions,
    ModuleSource, bootstrap_modules, delete_module, list_modules, load_module, load_module_by_name,
    save_module,
};
use sc_test_harness::TestDb;
use serde_json::json;

async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    Catalog::init(driver as Arc<dyn DatabaseDriver>).await
}

#[tokio::test]
async fn a_module_round_trips_through_its_row() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let mut module = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt@0.2.0");
    module.version = Some("0.2.0".into());
    module
        .configuration
        .insert("broker_url".into(), json!("mqtt://localhost"));
    save_module(&cat, &module).await.unwrap();

    let read = load_module(&cat, module.id).await.unwrap().unwrap();
    assert_eq!(read.name, "@saltcorn/mqtt");
    assert_eq!(read.source, ModuleSource::Npm);
    assert_eq!(read.location, "@saltcorn/mqtt@0.2.0");
    assert_eq!(read.version.as_deref(), Some("0.2.0"));
    assert_eq!(
        read.configuration.get("broker_url"),
        Some(&json!("mqtt://localhost"))
    );

    // By name, which is how the loader and the API address it.
    let by_name = load_module_by_name(&cat, "@saltcorn/mqtt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_name.id, module.id);

    // Saving again updates in place rather than inserting a second row.
    let mut edited = read;
    edited
        .configuration
        .insert("broker_url".into(), json!("mqtt://elsewhere"));
    save_module(&cat, &edited).await.unwrap();
    assert_eq!(list_modules(&cat).await.unwrap().len(), 1);
    assert_eq!(
        load_module(&cat, module.id)
            .await
            .unwrap()
            .unwrap()
            .configuration
            .get("broker_url"),
        Some(&json!("mqtt://elsewhere"))
    );

    assert!(delete_module(&cat, module.id).await.unwrap());
    assert!(!delete_module(&cat, module.id).await.unwrap());
    assert!(list_modules(&cat).await.unwrap().is_empty());
}

#[tokio::test]
async fn two_modules_cannot_claim_one_package_name() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let first = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt");
    save_module(&cat, &first).await.unwrap();

    // A *different* module (a different id) with the same package name: refused
    // where the admin can read it, rather than as a constraint violation.
    let second = Module::new("@saltcorn/mqtt", ModuleSource::Local, "/srv/checkout/mqtt");
    let err = save_module(&cat, &second).await.unwrap_err();
    assert!(err.to_string().contains("@saltcorn/mqtt"), "{err}");
    assert!(err.to_string().contains("already installed"), "{err}");
    assert_eq!(list_modules(&cat).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_row_with_an_unreadable_source_names_the_module_and_the_column() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let module = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt");
    save_module(&cat, &module).await.unwrap();
    // Something wrote a source nothing understands — a hand-edited row, a
    // restore from a version that had a third kind. Reading is strict: the
    // module is reported, not silently defaulted to `npm` and reinstalled from
    // the registry.
    db.client()
        .await
        .unwrap()
        .execute(
            &format!("update {MODULES_TABLE} set source = 'github'"),
            &[],
        )
        .await
        .unwrap();

    let err = list_modules(&cat).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("@saltcorn/mqtt"), "{msg}");
    assert!(msg.contains("github"), "{msg}");
}

#[tokio::test]
async fn a_module_needs_a_name_and_a_location() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let nameless = Module::new("  ", ModuleSource::Npm, "@saltcorn/mqtt");
    assert!(
        save_module(&cat, &nameless)
            .await
            .unwrap_err()
            .to_string()
            .contains("package name")
    );

    let nowhere = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "   ");
    assert!(
        save_module(&cat, &nowhere)
            .await
            .unwrap_err()
            .to_string()
            .contains("location")
    );
}

/// Phase 3: a module's permission set is on its row, and a row that has never
/// had one reaches nothing.
#[tokio::test]
async fn a_permission_set_round_trips_and_a_row_without_one_is_closed() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let mut module = Module::new("@saltcorn/mqtt", ModuleSource::Npm, "@saltcorn/mqtt");
    // Installed with nothing granted, which is the whole default (§2).
    save_module(&cat, &module).await.unwrap();
    assert!(
        load_module(&cat, module.id)
            .await
            .unwrap()
            .unwrap()
            .permissions
            .is_closed()
    );

    module.permissions = ModulePermissions {
        net: vec!["broker.example:1883".into()],
        env: vec!["MQTT_PASSWORD".into()],
        ..ModulePermissions::closed()
    };
    save_module(&cat, &module).await.unwrap();
    let read = load_module_by_name(&cat, "@saltcorn/mqtt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.permissions, module.permissions);

    // A module installed before the column existed has NULL there, and NULL is
    // the closed set rather than a row nobody can read — the one lenient read in
    // the store, and it fails in the safe direction.
    db.client()
        .await
        .unwrap()
        .execute(
            &format!("update {MODULES_TABLE} set {COL_PERMISSIONS} = NULL"),
            &[],
        )
        .await
        .unwrap();
    assert!(
        load_module(&cat, module.id)
            .await
            .unwrap()
            .unwrap()
            .permissions
            .is_closed()
    );

    // But a set nobody can *parse* is an error, not a guess: the shape is still
    // strict, and a stored `net: "everything"` must not become a grant.
    db.client()
        .await
        .unwrap()
        .execute(
            &format!("update {MODULES_TABLE} set {COL_PERMISSIONS} = '{{\"net\": \"all\"}}'"),
            &[],
        )
        .await
        .unwrap();
    let err = list_modules(&cat).await.unwrap_err().to_string();
    assert!(err.contains("net"), "{err}");
}

/// Phase 5: a module's **language** is on its row, a row that predates the
/// column reads as JavaScript, and a language that disagrees with its source is
/// refused before it can become a row nothing could reinstall (§8).
#[tokio::test]
async fn a_language_round_trips_and_a_row_without_one_is_javascript() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    let mut module = Module::new(
        "saltcorn-weather",
        ModuleSource::Pypi,
        "saltcorn-weather>=0.2",
    )
    .in_language(ModuleLanguage::Python);
    module.version = Some("0.2.1".into());
    save_module(&cat, &module).await.unwrap();

    let read = load_module_by_name(&cat, "saltcorn-weather")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.language, ModuleLanguage::Python);
    assert_eq!(read.source, ModuleSource::Pypi);

    // A row written before there was a second language has NULL here, and NULL
    // is `javascript` — the only language there was — rather than a row nobody
    // can read.
    db.client()
        .await
        .unwrap()
        .execute(
            &format!("update {MODULES_TABLE} set {COL_LANGUAGE} = null"),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        load_module(&cat, module.id)
            .await
            .unwrap()
            .unwrap()
            .language,
        ModuleLanguage::JavaScript
    );

    // And a language nothing understands is reported, naming the module — the
    // strictness the source column already has.
    db.client()
        .await
        .unwrap()
        .execute(
            &format!("update {MODULES_TABLE} set {COL_LANGUAGE} = 'ruby'"),
            &[],
        )
        .await
        .unwrap();
    let msg = list_modules(&cat).await.unwrap_err().to_string();
    assert!(msg.contains("saltcorn-weather"), "{msg}");
    assert!(msg.contains("ruby"), "{msg}");
}

#[tokio::test]
async fn a_language_and_a_source_that_disagree_are_refused() {
    let db = TestDb::new().await.unwrap();
    let cat = catalog(&db).await.unwrap();
    bootstrap_modules(&cat).await.unwrap();

    // npm cannot fetch from PyPI and pip cannot fetch from npm, so neither pair
    // is a row this server could act on.
    let wrong = Module::new("saltcorn-weather", ModuleSource::Npm, "saltcorn-weather")
        .in_language(ModuleLanguage::Python);
    let msg = save_module(&cat, &wrong).await.unwrap_err().to_string();
    assert!(msg.contains("python") && msg.contains("npm"), "{msg}");
    assert!(msg.contains("pypi"), "it names what would work: {msg}");

    let backwards = Module::new("@saltcorn/mqtt", ModuleSource::Pypi, "saltcorn-mqtt");
    let msg = save_module(&cat, &backwards).await.unwrap_err().to_string();
    assert!(msg.contains("javascript") && msg.contains("pypi"), "{msg}");

    assert!(list_modules(&cat).await.unwrap().is_empty());
}
