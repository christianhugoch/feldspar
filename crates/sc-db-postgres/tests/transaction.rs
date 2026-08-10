//! Integration test: `begin()` → commit/rollback against real Postgres, and
//! driving `PgDriver` through the `DatabaseDriver` trait object.

use std::sync::Arc;

use sc_db::{ColumnDef, DatabaseDriver, SchemaChange};
use sc_db_postgres::PgDriver;
use sc_query::{Expr, Insert, Projection, Select, Source, Statement, Value};
use sc_test_harness::TestDb;

/// A `CREATE TABLE name (id int8 PRIMARY KEY)` change.
fn create_table(name: &str) -> SchemaChange {
    SchemaChange::CreateTable {
        name: name.into(),
        columns: vec![ColumnDef::new("id", "int8").not_null()],
        primary_key: vec!["id".into()],
        unlogged: false,
    }
}

/// An `INSERT INTO name (id) VALUES (v)` statement.
fn insert(name: &str, v: i64) -> Statement {
    Statement::from(Insert {
        table: name.into(),
        columns: vec!["id".into()],
        rows: vec![vec![Expr::lit(v)]],
        returning: vec![],
    })
}

/// A `SELECT id FROM name` statement.
fn select_all(name: &str) -> Statement {
    Statement::from(
        Select::from(Source::table(name)).columns(vec![Projection::expr(Expr::col("id"))]),
    )
}

async fn has_table(driver: &PgDriver, name: &str) -> sc_error::Result<bool> {
    Ok(driver.introspect().await?.iter().any(|t| t.name == name))
}

#[tokio::test]
async fn commit_persists_and_rollback_discards() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());

    // --- commit path ---
    let mut tx = driver.begin().await?;
    tx.apply_schema(&create_table("sc_tx_a")).await?;
    tx.query(&insert("sc_tx_a", 1)).await?;

    // A separate pooled connection cannot see the uncommitted table (DDL is
    // transactional in Postgres).
    assert!(!has_table(&driver, "sc_tx_a").await?);

    tx.commit().await?;

    // After commit, both the table and its row are visible elsewhere.
    assert!(has_table(&driver, "sc_tx_a").await?);
    let rows = driver
        .query(&select_all("sc_tx_a"))
        .await?
        .try_collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::Int(1)));

    // --- rollback path ---
    let mut tx = driver.begin().await?;
    tx.apply_schema(&create_table("sc_tx_b")).await?;
    tx.query(&insert("sc_tx_b", 2)).await?;
    // Within the transaction its own writes are visible.
    let seen = tx
        .query(&select_all("sc_tx_b"))
        .await?
        .try_collect()
        .await?;
    assert_eq!(seen.len(), 1);

    tx.rollback().await?;

    // After rollback the table never existed.
    assert!(!has_table(&driver, "sc_tx_b").await?);

    Ok(())
}

#[tokio::test]
async fn drops_without_finishing_roll_back() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());

    // A committed table so the connection is definitely reusable afterwards.
    driver.apply_schema(&create_table("sc_tx_base")).await?;

    {
        let mut tx = driver.begin().await?;
        tx.apply_schema(&create_table("sc_tx_abandoned")).await?;
        // Drop `tx` here without commit/rollback → its Drop rolls back.
    }

    // Give the drop-spawned rollback a moment, then confirm the abandoned table
    // did not survive and the pool is still usable.
    for _ in 0..50 {
        if !has_table(&driver, "sc_tx_abandoned").await? {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!has_table(&driver, "sc_tx_abandoned").await?);
    assert!(has_table(&driver, "sc_tx_base").await?);

    Ok(())
}

#[tokio::test]
async fn usable_through_the_database_driver_trait() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver: Arc<dyn DatabaseDriver> = Arc::new(PgDriver::from_pool(db.pool().clone()));

    // Capabilities and dialect are reachable through the trait object.
    assert!(driver.capabilities().returning);
    let (sql, _) = driver
        .dialect()
        .render(&select_all("whatever"))
        .expect("render");
    assert_eq!(sql, "SELECT \"id\" FROM \"whatever\"");

    // Full round-trip via the trait: create, insert directly and in a committed
    // transaction, then read both rows back.
    driver.apply_schema(&create_table("sc_tx_obj")).await?;
    driver.query(&insert("sc_tx_obj", 1)).await?;

    let mut tx = driver.begin().await?;
    tx.query(&insert("sc_tx_obj", 2)).await?;
    tx.commit().await?;

    let rows = driver
        .query(&select_all("sc_tx_obj"))
        .await?
        .try_collect()
        .await?;
    assert_eq!(rows.len(), 2);

    Ok(())
}
