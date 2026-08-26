//! [`SharedTx`] against a real database (§10.3, decision 6).
//!
//! The transaction a **workflow step** runs in is shared by everything the step
//! does — its action, the row layer beneath it, and the triggers its writes
//! cascade into — so the three properties that make that safe are pinned here,
//! at the layer that owns them rather than through the engine that uses them:
//!
//! - it **begins when it is first used**, so a step that writes nothing holds no
//!   transaction while it waits;
//! - what it writes is **invisible until it commits** and gone if it rolls back,
//!   through every handle on it;
//! - and each statement carries **its own caller** to the policies, because the
//!   writers sharing it are not the same caller and a `SET LOCAL` outlives its
//!   statement.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sc_catalog::{
    CallerContext, Catalog, SharedTx, TableMeta, bootstrap_table_meta, enable_rls, save_table_meta,
};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_query::{Insert, Projection, Select, Source, Statement};
use sc_test_harness::TestDb;

async fn catalog(db: &TestDb, schema: &str) -> Result<Catalog> {
    db.client()
        .await?
        .batch_execute(schema)
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_table_meta(&catalog).await?;
    catalog.reload().await?;
    Ok(catalog)
}

/// `SELECT * FROM posts`.
fn select() -> Statement {
    Statement::Select(Box::new(
        Select::from(Source::table("posts")).columns(vec![Projection::all()]),
    ))
}

/// `INSERT INTO posts VALUES (id, owner)`.
fn insert(id: i64, owner: &str) -> Statement {
    Statement::from(Insert::row(
        "posts".to_owned(),
        vec!["id".to_owned(), "owner".to_owned()],
        vec![
            sc_query::Expr::lit(sc_query::Value::Int(id)),
            sc_query::Expr::lit(sc_query::Value::Text(owner.to_owned())),
        ],
    ))
}

/// How many rows a pooled reader — anybody but this transaction — can see.
async fn committed(db: &TestDb) -> usize {
    let client = db.client().await.unwrap();
    let rows = client.query("SELECT id FROM posts", &[]).await.unwrap();
    rows.len()
}

/// Nothing is begun until something needs it, and committing a transaction that
/// never began is the success it describes.
#[tokio::test]
async fn a_shared_transaction_begins_when_it_is_first_used() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(
        &db,
        "CREATE TABLE posts (id bigint primary key, owner text)",
    )
    .await?;

    let tx = SharedTx::begin_primary(&catalog)?;
    assert!(tx.is_open().await);
    assert!(!tx.has_begun().await, "nothing has needed the database yet");
    tx.commit().await?;

    // And a handle whose transaction is finished refuses by name rather than
    // quietly writing somewhere else.
    let err = tx.run(None, &select()).await.expect_err("finished");
    assert!(err.to_string().contains("already been committed"), "{err}");
    Ok(())
}

/// A write on a shared transaction is invisible outside it, visible to every
/// handle on it, and undone by a rollback.
#[tokio::test]
async fn what_it_writes_is_private_until_it_commits() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(
        &db,
        "CREATE TABLE posts (id bigint primary key, owner text)",
    )
    .await?;

    let tx = SharedTx::begin_primary(&catalog)?;
    tx.run(None, &insert(1, "alice@example.com")).await?;
    assert!(tx.has_begun().await);
    // A clone is a handle on the *same* transaction: it sees the row, and what
    // it writes joins it — which is what lets the row layer, an action and a
    // cascading trigger share one.
    let also = tx.clone();
    assert_eq!(also.run(None, &select()).await?.len(), 1);
    also.run(None, &insert(2, "bob@example.com")).await?;
    assert_eq!(tx.run(None, &select()).await?.len(), 2);
    // Nobody else can see either of them yet.
    assert_eq!(committed(&db).await, 0);

    tx.rollback().await?;
    assert_eq!(committed(&db).await, 0, "a rollback undid both writes");

    // The same again, committed: both land at once.
    let tx = SharedTx::begin_primary(&catalog)?;
    tx.run(None, &insert(1, "alice@example.com")).await?;
    tx.run(None, &insert(2, "bob@example.com")).await?;
    tx.commit().await?;
    assert_eq!(committed(&db).await, 2);
    Ok(())
}

/// Each statement reaches the policies as **its own** caller, and a statement
/// with no caller reaches them as no access — even when the statement before it
/// on the same transaction was an admin's.
#[tokio::test]
async fn every_statement_carries_its_own_caller_to_the_policies() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(
        &db,
        "CREATE TABLE posts (id bigint primary key, owner text); \
         INSERT INTO posts VALUES (1, 'alice@example.com'), (2, 'bob@example.com')",
    )
    .await?;
    let mut meta = TableMeta::new("posts");
    meta.set_ownership_formula(Some("owner === user.email"));
    meta.set_rls_enabled(true);
    save_table_meta(&catalog, &meta).await?;
    let table = catalog.require("posts")?;
    enable_rls(&catalog, &table).await?;

    let tx = SharedTx::begin_primary(&catalog)?;
    let admin = CallerContext::anonymous(1);
    assert_eq!(tx.run(Some(&admin), &select()).await?.len(), 2);

    // Alice, on the same transaction the admin just used: her row only. Without
    // re-applying the GUCs per statement this would still be the admin's read.
    let alice = CallerContext::new(
        80,
        Some(serde_json::json!({ "email": "alice@example.com" })),
    );
    let rows = tx.run(Some(&alice), &select()).await?;
    assert_eq!(rows.len(), 1);

    // And no caller at all is no access, not "whoever went last".
    assert!(tx.run(None, &select()).await?.is_empty());
    tx.rollback().await?;
    Ok(())
}
