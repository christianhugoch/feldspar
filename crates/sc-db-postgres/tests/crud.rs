//! Phase 2 end-to-end integration test: the full lifecycle against a real
//! Postgres — create a table (no auto `id`), add fields, introspect, and run
//! every CRUD operation (INSERT/SELECT/UPDATE/DELETE) through the driver.
//!
//! The per-operation behaviour is covered in the sibling test files; this walks
//! the whole story in one place, and in particular exercises UPDATE and DELETE
//! against a live database (the other files only render them).

use sc_db::{ColumnDef, PhysicalTable, SchemaChange};
use sc_db_postgres::PgDriver;
use sc_query::{
    Assignment, Delete, Expr, Insert, Projection, Select, Source, Statement, Update, Value,
};
use sc_test_harness::TestDb;

/// `SELECT id, title, done, priority FROM sc_e2e_task [WHERE id = ?] ORDER BY id`.
fn select_tasks(where_id: Option<i64>) -> Statement {
    let mut select = Select::from(Source::table("sc_e2e_task")).columns(vec![
        Projection::expr(Expr::col("id")),
        Projection::expr(Expr::col("title")),
        Projection::expr(Expr::col("done")),
        Projection::expr(Expr::col("priority")),
    ]);
    if let Some(id) = where_id {
        select = select.filter(Expr::col("id").eq(Expr::lit(id)));
    }
    Statement::from(Select {
        order: vec![sc_query::OrderBy::asc(Expr::col("id"))],
        ..select
    })
}

async fn task_table(driver: &PgDriver) -> sc_error::Result<PhysicalTable> {
    driver
        .introspect()
        .await?
        .into_iter()
        .find(|t| t.name == "sc_e2e_task")
        .ok_or_else(|| sc_error::Error::msg("sc_e2e_task not found in introspection"))
}

#[tokio::test]
async fn full_table_and_crud_lifecycle() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());

    // --- create table (explicit key; no invented `id`) ---
    driver
        .apply_schema(&SchemaChange::CreateTable {
            name: "sc_e2e_task".into(),
            columns: vec![
                ColumnDef::new("id", "int8").not_null(),
                ColumnDef::new("title", "text").not_null(),
            ],
            primary_key: vec!["id".into()],
            unlogged: false,
        })
        .await?;

    // --- add fields ---
    driver
        .apply_schema(&SchemaChange::AddColumn {
            table: "sc_e2e_task".into(),
            column: ColumnDef::new("done", "bool").not_null().default("false"),
        })
        .await?;
    driver
        .apply_schema(&SchemaChange::AddColumn {
            table: "sc_e2e_task".into(),
            column: ColumnDef::new("priority", "int8"),
        })
        .await?;

    // --- introspect the resulting schema ---
    let table = task_table(&driver).await?;
    assert_eq!(
        table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["id", "title", "done", "priority"]
    );
    assert_eq!(table.primary_key, vec!["id"]);
    let priority = table.columns.iter().find(|c| c.name == "priority").unwrap();
    assert_eq!(priority.sql_type, "int8");
    assert!(priority.nullable);

    // --- CREATE: insert two rows, returning their ids ---
    let inserted = driver
        .query(&Statement::from(Insert {
            table: "sc_e2e_task".into(),
            columns: vec!["id".into(), "title".into()],
            rows: vec![
                vec![Expr::lit(1_i64), Expr::lit("write")],
                vec![Expr::lit(2_i64), Expr::lit("review")],
            ],
            returning: vec![Projection::expr(Expr::col("id"))],
        }))
        .await?
        .try_collect()
        .await?;
    assert_eq!(inserted.len(), 2);

    // --- READ: both rows, with the column default and NULL applied ---
    let rows = driver
        .query(&select_tasks(None))
        .await?
        .try_collect()
        .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(rows[0].get("title"), Some(&Value::Text("write".into())));
    assert_eq!(rows[0].get("done"), Some(&Value::Bool(false)));
    assert_eq!(rows[0].get("priority"), Some(&Value::Null));

    // --- UPDATE: mark task 1 done with a priority ---
    driver
        .query(&Statement::from(
            Update::new(
                "sc_e2e_task",
                vec![
                    Assignment::new("done", Expr::lit(true)),
                    Assignment::new("priority", Expr::lit(5_i64)),
                ],
            )
            .filter(Expr::col("id").eq(Expr::lit(1_i64))),
        ))
        .await?
        .try_collect()
        .await?;
    let one = driver
        .query(&select_tasks(Some(1)))
        .await?
        .try_collect()
        .await?;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].get("done"), Some(&Value::Bool(true)));
    assert_eq!(one[0].get("priority"), Some(&Value::Int(5)));
    // The other row is untouched.
    let two = driver
        .query(&select_tasks(Some(2)))
        .await?
        .try_collect()
        .await?;
    assert_eq!(two[0].get("done"), Some(&Value::Bool(false)));

    // --- DELETE: remove task 2, returning the deleted id ---
    let deleted = driver
        .query(&Statement::from(Delete {
            table: "sc_e2e_task".into(),
            filter: Some(Expr::col("id").eq(Expr::lit(2_i64))),
            returning: vec![Projection::expr(Expr::col("id"))],
        }))
        .await?
        .try_collect()
        .await?;
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].get("id"), Some(&Value::Int(2)));

    // --- READ back: only task 1 remains ---
    let remaining = driver
        .query(&select_tasks(None))
        .await?
        .try_collect()
        .await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].get("id"), Some(&Value::Int(1)));

    // --- introspect an existing table: schema still reflects the added fields ---
    let table = task_table(&driver).await?;
    assert_eq!(table.columns.len(), 4);

    Ok(())
}
