//! Phase 3.2 integration test: the `_sc_fields` overlay merged onto
//! introspection, against a real database (design §9, §3.2).
//!
//! The field-level twin of `table_meta_merge.rs`. The merge's promise is the
//! same — a field an admin configured comes out of the catalog carrying that
//! configuration, across reloads, and a field nobody configured comes out exactly
//! as `from_physical` produced it — with one thing genuinely new: the overlay can
//! contradict the column (a `text` configured as `Integer`, a `Key` with no
//! foreign key behind it), and the merge must **report** the contradiction while
//! leaving the field usable, never downgrade silently and never fail.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sc_catalog::{
    Catalog, DataField, DataFieldKind, FieldMeta, FileStoreId, bootstrap_field_meta,
    delete_field_meta, save_field_meta,
};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_test_harness::TestDb;
use sc_types::{BasicType, TypeRef};
use serde_json::json;

/// A catalog over a per-test database that has never seen Saltcorn, with the
/// field overlay table bootstrapped.
async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let cat = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_field_meta(&cat).await?;
    Ok(cat)
}

/// A `books` table: `title` and `cover`, both `text`, plus a numeric `pages`.
async fn create_books(cat: &Catalog) -> Result<()> {
    cat.create_table(
        "books",
        &[
            DataField::plain("id", TypeRef::Basic(BasicType::Uuid))
                .required()
                .primary_key(),
            DataField::plain("title", TypeRef::Basic(BasicType::Text)),
            DataField::plain("cover", TypeRef::Basic(BasicType::Text)),
            DataField::plain("pages", TypeRef::Basic(BasicType::Int)),
        ],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_field_with_no_overlay_is_exactly_what_introspection_produced() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    create_books(&cat).await?;

    // Not a single overlay row saved: every field must be identical to what a
    // catalog built straight from introspection would hold. This is the
    // zero-setup promise at the field level.
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let bare = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;

    assert_eq!(cat.require("books")?.fields, bare.require("books")?.fields);
    assert!(cat.field_overlay_issues()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_rich_type_and_a_file_field_survive_reloads() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    create_books(&cat).await?;

    // A rich `String` on `title` with an attribute, and a `File` on `cover`.
    let mut title = FieldMeta::new("books", "title").rich_type("string");
    title.attributes.insert("max_length".into(), json!(200));
    save_field_meta(&cat, &title).await?;

    let cover = FieldMeta::new("books", "cover").kind(DataFieldKind::File {
        store: FileStoreId("uploads".into()),
        folder: Some("covers".into()),
        mime_allow: vec!["image/png".into()],
    });
    save_field_meta(&cat, &cover).await?;

    // Both are visible on the merged table immediately (save reloaded the cache)…
    assert_cover_and_title(&cat)?;
    // …and again after an unrelated reload, proving they are re-merged, not a
    // one-off applied at save time.
    cat.reload().await?;
    assert_cover_and_title(&cat)?;
    assert!(cat.field_overlay_issues()?.is_empty());
    Ok(())
}

fn assert_cover_and_title(cat: &Catalog) -> Result<()> {
    let books = cat.require("books")?;

    let title = books.field("title").expect("title");
    assert_eq!(title.base.type_.name(), "string");
    assert_eq!(title.base.attributes.get("max_length"), Some(&json!(200)));

    let cover = books.field("cover").expect("cover");
    assert_eq!(
        cover.kind,
        DataFieldKind::File {
            store: FileStoreId("uploads".into()),
            folder: Some("covers".into()),
            mime_allow: vec!["image/png".into()],
        }
    );
    // `pages` — no overlay — stays a plain int.
    assert_eq!(
        books.field("pages").unwrap().base.type_,
        TypeRef::Basic(BasicType::Int)
    );
    Ok(())
}

#[tokio::test]
async fn a_type_column_mismatch_is_reported_and_the_table_still_serves() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    create_books(&cat).await?;

    // `title` is `text`; configure it as `integer`. Exactly what a hand-edited
    // database or a mistaken import produces.
    let meta = FieldMeta::new("books", "title").rich_type("integer");
    save_field_meta(&cat, &meta).await?;

    // The row is stored and the mismatch is reported…
    let issues = cat.field_overlay_issues()?;
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].table, "books");
    assert_eq!(issues[0].field, "title");
    assert!(issues[0].message.contains("integer"), "{:?}", issues[0]);

    // …but the table is untouched: `title` stays a usable `text` column, and the
    // table still lists all its fields.
    let books = cat.require("books")?;
    assert_eq!(
        books.field("title").unwrap().base.type_,
        TypeRef::Basic(BasicType::Text)
    );
    assert_eq!(books.fields.len(), 4);

    // Forgetting the overlay clears the reported issue on the next reload.
    delete_field_meta(&cat, meta.id).await?;
    assert!(cat.field_overlay_issues()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_overlay_for_a_dropped_column_is_reported_but_the_table_is_fine() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;
    create_books(&cat).await?;

    save_field_meta(&cat, &FieldMeta::new("books", "cover").rich_type("string")).await?;
    assert!(cat.field_overlay_issues()?.is_empty());

    // Drop the column underneath the overlay.
    db.client()
        .await?
        .execute("ALTER TABLE books DROP COLUMN cover", &[])
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    cat.reload().await?;

    let issues = cat.field_overlay_issues()?;
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
        issues[0].message.contains("does not exist"),
        "{:?}",
        issues[0]
    );
    // The table is otherwise intact.
    assert_eq!(cat.require("books")?.fields.len(), 3);
    Ok(())
}

#[tokio::test]
async fn a_system_table_ignores_a_field_overlay_inserted_behind_the_api() -> Result<()> {
    let db = TestDb::new().await?;
    let cat = catalog(&db).await?;

    // `save_field_meta` refuses a system table, so reach past it: insert a row
    // naming `_sc_fields` directly. The merge must ignore it (system tables are
    // hidden and unconfigurable) rather than apply or crash.
    db.client()
        .await?
        .execute(
            "INSERT INTO _sc_fields \
               (id, table_name, name, label, description, \"type\", kind, attributes) \
             VALUES (gen_random_uuid(), '_sc_fields', 'kind', 'Hacked', '', 'string', 'plain', \
                     '{}'::jsonb)",
            &[],
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    cat.reload().await?;

    let system = cat.require("_sc_fields")?;
    let kind = system.field("kind").expect("kind column");
    assert_eq!(kind.base.label, "kind", "the injected label did not apply");
    assert_eq!(
        kind.base.type_,
        TypeRef::Basic(BasicType::Text),
        "the injected rich type did not apply"
    );
    assert!(cat.field_overlay_issues()?.is_empty());
    Ok(())
}
