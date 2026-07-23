//! Phase 6 integration test: RLS policy generation and the caller-context
//! runner, against a real database (§7.3, §6).
//!
//! The server-level test proves the end-to-end behaviour; this proves the
//! storage-layer primitives directly — that `enable_rls` actually emits the
//! four policies with `FORCE`, that `disable_rls` removes them cleanly, and
//! that `run_in_context` sets the GUCs a policy reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use sc_catalog::{
    Catalog, CallerContext, TableMeta, bootstrap_table_meta, disable_rls, enable_rls,
    run_in_context, save_table_meta,
};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_query::{Projection, Select, Source, Statement};
use sc_test_harness::TestDb;

async fn catalog(db: &TestDb) -> Result<Catalog> {
    // No `users` table here (that lives above this crate, in `sc-auth`): with no
    // user fields declared, a `user.email` reference validates unchecked, which
    // is all this storage-layer test needs.
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_table_meta(&catalog).await?;
    Ok(catalog)
}

/// The policy names Postgres reports for a table, sorted.
async fn policies(db: &TestDb, table: &str) -> Vec<String> {
    let client = db.client().await.unwrap();
    let rows = client
        .query(
            "SELECT policyname FROM pg_policies WHERE tablename = $1 ORDER BY policyname",
            &[&table],
        )
        .await
        .unwrap();
    rows.iter().map(|r| r.get::<_, String>(0)).collect()
}

/// Whether the table has RLS enabled and forced.
async fn rls_flags(db: &TestDb, table: &str) -> (bool, bool) {
    let client = db.client().await.unwrap();
    let row = client
        .query_one(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = $1",
            &[&table],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1))
}

async fn configure_formula(catalog: &Catalog, table: &str, formula: &str) -> Result<()> {
    let mut meta = TableMeta::new(table);
    meta.set_ownership_formula(Some(formula));
    meta.set_rls_enabled(true);
    save_table_meta(catalog, &meta).await
}

#[tokio::test]
async fn enable_creates_four_forced_policies_and_disable_removes_them() -> Result<()> {
    let db = TestDb::new().await?;
    db.client()
        .await?
        .batch_execute("CREATE TABLE posts (id bigint primary key, owner text)")
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    let catalog = catalog(&db).await?;
    catalog.reload().await?;
    configure_formula(&catalog, "posts", "owner === user.email").await?;

    let table = catalog.require("posts")?;
    enable_rls(&catalog, &table).await?;

    // Four policies, one per command, and both RLS flags set (FORCE is what
    // makes the policies bite for the owning connection at all).
    assert_eq!(
        policies(&db, "posts").await,
        vec![
            "sc_owner_delete",
            "sc_owner_insert",
            "sc_owner_select",
            "sc_owner_update",
        ]
    );
    assert_eq!(rls_flags(&db, "posts").await, (true, true));

    // Enabling again is idempotent — the drop-then-create recreates, never
    // duplicates (a formula edit takes this path).
    let table = catalog.require("posts")?;
    enable_rls(&catalog, &table).await?;
    assert_eq!(policies(&db, "posts").await.len(), 4);

    // Disable leaves no policy debris and clears both flags.
    disable_rls(&catalog, "posts").await?;
    assert!(policies(&db, "posts").await.is_empty());
    assert_eq!(rls_flags(&db, "posts").await, (false, false));
    Ok(())
}

#[tokio::test]
async fn run_in_context_sets_the_caller_gucs_a_policy_reads() -> Result<()> {
    let db = TestDb::new().await?;
    db.client()
        .await?
        .batch_execute(
            "CREATE TABLE posts (id bigint primary key, owner text); \
             INSERT INTO posts VALUES (1, 'alice@example.com'), (2, 'bob@example.com')",
        )
        .await
        .map_err(|e| sc_error::Error::database(e.to_string()))?;
    let catalog = catalog(&db).await?;
    catalog.reload().await?;
    configure_formula(&catalog, "posts", "owner === user.email").await?;
    let table = catalog.require("posts")?;
    enable_rls(&catalog, &table).await?;

    let select =
        Statement::Select(Box::new(Select::from(Source::table("posts")).columns(vec![
            Projection::all(),
        ])));

    // Alice's context: only her row is visible through the SELECT policy.
    let alice = CallerContext {
        role: 80,
        user_json: Some(r#"{"email":"alice@example.com"}"#.to_owned()),
    };
    let rows = run_in_context(&catalog, &alice, &select).await?;
    assert_eq!(rows.len(), 1);

    // An admin context (role 1) clears the role floor and sees both rows even
    // though the table is FORCE'd.
    let admin = CallerContext::anonymous(1);
    let rows = run_in_context(&catalog, &admin, &select).await?;
    assert_eq!(rows.len(), 2);

    // An anonymous context (role 100, no user) sees nothing — the policy fails
    // closed when the user GUC is unset.
    let anon = CallerContext::anonymous(100);
    let rows = run_in_context(&catalog, &anon, &select).await?;
    assert!(rows.is_empty());
    Ok(())
}
