//! `_fd_llm_providers` against a **real Postgres** (principle 4): the row is the
//! provider's definition, so what a save writes and a load reads back is the
//! whole of whether a configured provider survives a restart.
//!
//! The secret round trip is here rather than in a unit test for the same reason.
//! The claim `FormField::secret` makes is that a *stored* key survives a
//! read-edit-save cycle in which the reader never saw it, and that claim spans
//! the redaction, the merge and the storage. Asserting it against the real table
//! is what makes it a claim about the system rather than about three functions.

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_llm::{
    ANTHROPIC_BACKEND, CFG_API_KEY, CFG_BASE_URL, CFG_MODEL, LlmProviderDef, LlmProviderDefId,
    OPENAI_RESPONSES_BACKEND, bootstrap_llm_providers, check_provider_saveable, connect_provider,
    delete_llm_provider, list_llm_providers, load_llm_provider, load_llm_provider_by_name,
    provider_config_spec, require_llm_provider, save_llm_provider,
};
use sc_test_harness::TestDb;
use sc_types::{SECRET_SENTINEL, merge_secrets, redact_attrs};

/// A catalog over a per-test database, with the providers table bootstrapped.
async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_llm_providers(&catalog).await?;
    Ok(catalog)
}

#[tokio::test]
async fn a_provider_round_trips_through_its_row() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let def = LlmProviderDef::anthropic("main", "sk-ant-secret", "claude-sonnet-4-5")
        .description("The house Anthropic key");
    save_llm_provider(&catalog, &def).await?;

    let loaded = load_llm_provider(&catalog, def.id)
        .await?
        .expect("the row that was just written");
    assert_eq!(loaded, def);

    // And by name, which is how an agent resolves it.
    assert_eq!(
        load_llm_provider_by_name(&catalog, "main").await?.as_ref(),
        Some(&def)
    );
    assert_eq!(require_llm_provider(&catalog, "main").await?, def);

    // A definition that survived the round trip is one that still connects.
    let provider = connect_provider(&loaded, None)?;
    assert_eq!(provider.model(), "claude-sonnet-4-5");
    Ok(())
}

#[tokio::test]
async fn bootstrap_is_idempotent_and_saving_twice_updates_in_place() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    // Called again exactly as a second boot would.
    bootstrap_llm_providers(&catalog).await?;

    let mut def = LlmProviderDef::openai("gateway", "sk-1", "gpt-5.1");
    save_llm_provider(&catalog, &def).await?;
    def.config.insert(
        CFG_BASE_URL.to_owned(),
        "https://gateway.internal/v1".into(),
    );
    def.description = "Through the gateway".to_owned();
    save_llm_provider(&catalog, &def).await?;

    let all = list_llm_providers(&catalog).await?;
    assert_eq!(all.len(), 1, "a second save must update, not insert");
    assert_eq!(
        all[0].setting(CFG_BASE_URL),
        Some("https://gateway.internal/v1")
    );
    assert_eq!(all[0].description, "Through the gateway");
    Ok(())
}

#[tokio::test]
async fn a_stored_key_survives_a_read_edit_save_cycle_that_never_saw_it() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let def = LlmProviderDef::anthropic("main", "sk-ant-secret", "claude-sonnet-4-5");
    save_llm_provider(&catalog, &def).await?;

    // The admin opens the form. What the API serialises is redacted, so this is
    // everything the browser could possibly know.
    let stored = load_llm_provider(&catalog, def.id).await?.expect("the row");
    let spec = provider_config_spec(&stored.backend)?;
    let shown = redact_attrs(&spec, &stored.config);
    assert_eq!(
        shown.get(CFG_API_KEY),
        Some(&serde_json::json!(SECRET_SENTINEL))
    );
    assert!(
        !serde_json::Value::Object(shown.clone())
            .to_string()
            .contains("sk-ant"),
        "no part of the key may leave, not even its prefix"
    );

    // They change the model and save, sending the sentinel back untouched.
    let mut submitted = shown;
    submitted.insert(CFG_MODEL.to_owned(), "claude-opus-4-1".into());
    let saved = LlmProviderDef {
        config: merge_secrets(&spec, &stored.config, &submitted),
        ..stored.clone()
    };
    save_llm_provider(&catalog, &saved).await?;

    // The stored key is untouched and the model changed.
    let after = load_llm_provider(&catalog, def.id).await?.expect("the row");
    assert_eq!(after.setting(CFG_API_KEY), Some("sk-ant-secret"));
    assert_eq!(after.setting(CFG_MODEL), Some("claude-opus-4-1"));

    // And the provider it produces is one that would actually authenticate —
    // the failure this whole mechanism exists to prevent is a saved mask.
    assert_ne!(after.setting(CFG_API_KEY), Some(SECRET_SENTINEL));
    Ok(())
}

#[tokio::test]
async fn a_name_clash_is_refused_where_the_admin_can_fix_it() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    save_llm_provider(
        &catalog,
        &LlmProviderDef::anthropic("main", "sk-1", "claude-sonnet-4-5"),
    )
    .await?;

    // A *different* definition claiming the same name.
    let clash = LlmProviderDef::openai("main", "sk-2", "gpt-5.1");
    let err = save_llm_provider(&catalog, &clash)
        .await
        .expect_err("two providers cannot share a name");
    let text = err.to_string();
    assert!(text.contains("main"), "{text}");

    // Only one row exists, so the failed save wrote nothing.
    assert_eq!(list_llm_providers(&catalog).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_structurally_wrong_config_is_refused_on_save() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    // No API key: knowable without a network, so the save is refused rather
    // than deferred to the first chat.
    let no_key = LlmProviderDef::new("main", ANTHROPIC_BACKEND).with(CFG_MODEL, "claude-opus-4-1");
    let err = save_llm_provider(&catalog, &no_key)
        .await
        .expect_err("a provider with no key cannot be saved");
    assert!(err.to_string().contains(CFG_API_KEY), "{err}");

    // A backend nothing implements, likewise.
    let unknown = LlmProviderDef::new("main", "bedrock").with(CFG_API_KEY, "x");
    assert!(save_llm_provider(&catalog, &unknown).await.is_err());

    // And a nameless one.
    let nameless = LlmProviderDef::anthropic("   ", "sk-1", "claude-opus-4-1");
    assert!(check_provider_saveable(&catalog, &nameless).await.is_err());

    assert!(list_llm_providers(&catalog).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn listing_is_by_name_and_deleting_removes_exactly_one_row() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    for name in ["zeta", "alpha", "mid"] {
        save_llm_provider(
            &catalog,
            &LlmProviderDef::anthropic(name, "sk-1", "claude-sonnet-4-5"),
        )
        .await?;
    }
    let names: Vec<String> = list_llm_providers(&catalog)
        .await?
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["alpha", "mid", "zeta"]);

    let alpha = require_llm_provider(&catalog, "alpha").await?;
    assert!(delete_llm_provider(&catalog, alpha.id, &[]).await?);
    // Deleting again reports "there was nothing there" rather than failing.
    assert!(!delete_llm_provider(&catalog, alpha.id, &[]).await?);
    // And an id nothing ever used likewise.
    assert!(!delete_llm_provider(&catalog, LlmProviderDefId::new(), &[]).await?);

    let names: Vec<String> = list_llm_providers(&catalog)
        .await?
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["mid", "zeta"]);
    Ok(())
}

#[tokio::test]
async fn a_provider_something_still_references_is_not_deleted() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let def = LlmProviderDef::openai("main", "sk-1", "gpt-5.1");
    save_llm_provider(&catalog, &def).await?;

    // An agent lives a layer above, so its reference is passed in — the same
    // arrangement `delete_file_store` uses for applications.
    let err = delete_llm_provider(&catalog, def.id, &["agent `librarian`".to_owned()])
        .await
        .expect_err("a referenced provider must not be deleted out from under it");
    let text = err.to_string();
    assert!(text.contains("librarian"), "{text}");
    assert!(load_llm_provider(&catalog, def.id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn a_renamed_provider_keeps_its_row_identity() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let mut def = LlmProviderDef::openai("old-name", "sk-1", "gpt-5.1");
    save_llm_provider(&catalog, &def).await?;
    let id = def.id;

    def.name = "new-name".to_owned();
    save_llm_provider(&catalog, &def).await?;

    // The name is what everything references; the id is the row's identity, and
    // a rename is an edit, not a new provider.
    assert_eq!(
        load_llm_provider(&catalog, id)
            .await?
            .map(|d| d.name)
            .as_deref(),
        Some("new-name")
    );
    assert!(
        load_llm_provider_by_name(&catalog, "old-name")
            .await?
            .is_none()
    );
    assert_eq!(list_llm_providers(&catalog).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn both_backends_store_and_reconnect() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    let openai = LlmProviderDef::openai("oai", "sk-1", "gpt-5.1")
        .with(CFG_BASE_URL, "http://127.0.0.1:9/v1");
    let anthropic = LlmProviderDef::anthropic("ant", "sk-2", "claude-sonnet-4-5");
    save_llm_provider(&catalog, &openai).await?;
    save_llm_provider(&catalog, &anthropic).await?;

    // What boot does: read every row and turn it back into something callable.
    for def in list_llm_providers(&catalog).await? {
        let provider = connect_provider(&def, None)?;
        assert_eq!(Some(provider.model()), def.default_model());
    }
    assert_eq!(
        list_llm_providers(&catalog)
            .await?
            .iter()
            .map(|d| d.backend.clone())
            .collect::<Vec<_>>(),
        [ANTHROPIC_BACKEND, OPENAI_RESPONSES_BACKEND]
    );
    Ok(())
}
