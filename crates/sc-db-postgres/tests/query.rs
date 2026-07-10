//! Integration test: `PgDriver` runs rendered statements against a real
//! Postgres, decoding results back into `Value`s.
//!
//! Uses the shared harness, which hands each test its own throwaway database.

use sc_db_postgres::PgDriver;
use sc_query::{Expr, Insert, OrderBy, Projection, Select, Source, Statement, Value};
use sc_test_harness::TestDb;

#[tokio::test]
async fn runs_statements_and_decodes_rows() -> sc_error::Result<()> {
    let db = TestDb::new().await?;

    // Schema is created with a raw connection — `apply_schema` is a later item.
    let client = db.client().await?;
    client
        .batch_execute("CREATE TABLE item (id int8, name text, active bool, note text)")
        .await
        .map_err(|e| sc_error::Error::database(format!("create table: {e}")))?;

    let driver = PgDriver::from_pool(db.pool().clone());

    // Postgres advertises the full basic capability set.
    let caps = driver.capabilities();
    assert!(caps.returning && caps.composite_pk && caps.listen_notify);

    // INSERT ... RETURNING flows through the query path; a NULL bind lands in a
    // text column (proving NULL is typeless), and both bool values round-trip.
    let insert = Insert {
        table: "item".into(),
        columns: vec!["id".into(), "name".into(), "active".into(), "note".into()],
        rows: vec![
            vec![
                Expr::lit(1_i64),
                Expr::lit("apple"),
                Expr::lit(true),
                Expr::lit(Value::Null),
            ],
            vec![
                Expr::lit(2_i64),
                Expr::lit("pear"),
                Expr::lit(false),
                Expr::lit("ok"),
            ],
        ],
        returning: vec![Projection::expr(Expr::col("id"))],
    };
    let returned = driver
        .query(&Statement::from(insert))
        .await?
        .try_collect()
        .await?;
    assert_eq!(returned.len(), 2);
    assert_eq!(returned[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(returned[1].get("id"), Some(&Value::Int(2)));

    // SELECT the rows back, ordered, and check every decoded Value including the
    // NULL and both booleans.
    let select = Select {
        order: vec![OrderBy::asc(Expr::col("id"))],
        ..Select::from(Source::table("item")).columns(vec![
            Projection::expr(Expr::col("id")),
            Projection::expr(Expr::col("name")),
            Projection::expr(Expr::col("active")),
            Projection::expr(Expr::col("note")),
        ])
    };
    let rows = driver
        .query(&Statement::from(select))
        .await?
        .try_collect()
        .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("id"), Some(&Value::Int(1)));
    assert_eq!(rows[0].get("name"), Some(&Value::Text("apple".into())));
    assert_eq!(rows[0].get("active"), Some(&Value::Bool(true)));
    assert_eq!(rows[0].get("note"), Some(&Value::Null));
    assert_eq!(rows[1].get("active"), Some(&Value::Bool(false)));
    assert_eq!(rows[1].get("note"), Some(&Value::Text("ok".into())));

    // A parameterised WHERE keeps an injection payload inert: it matches no row
    // and, crucially, does not execute the embedded `DROP TABLE`.
    let payload = "apple'; DROP TABLE item; --";
    let danger = Select::from(Source::table("item"))
        .columns(vec![Projection::expr(Expr::col("id"))])
        .filter(Expr::col("name").eq(Expr::lit(payload)));
    let none = driver
        .query(&Statement::from(danger))
        .await?
        .try_collect()
        .await?;
    assert!(none.is_empty());

    // The table still has its two rows — the injection did not run.
    let survivors = driver
        .query(&Statement::from(
            Select::from(Source::table("item")).columns(vec![Projection::expr(Expr::col("id"))]),
        ))
        .await?
        .try_collect()
        .await?;
    assert_eq!(survivors.len(), 2);

    Ok(())
}
