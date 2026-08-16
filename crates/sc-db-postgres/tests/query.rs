//! Integration test: `PgDriver` runs rendered statements against a real
//! Postgres, decoding results back into `Value`s.
//!
//! Uses the shared harness, which hands each test its own throwaway database.

use sc_db_postgres::PgDriver;
use sc_query::{Expr, Insert, OrderBy, Projection, Select, Source, Statement, Value};
use sc_test_harness::TestDb;

/// A failed query surfaces the real Postgres cause *and* the offending SQL, so an
/// operator reading the log knows both what broke and which statement did it — a
/// bare `tokio_postgres::Error` renders as the useless string "db error".
#[tokio::test]
async fn query_error_reports_cause_and_sql() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());

    // Select from a table that was never created.
    let select = Select::from(Source::table("no_such_table"))
        .columns(vec![Projection::expr(Expr::col("id"))]);
    let msg = match driver.query(&Statement::from(select)).await {
        Ok(_) => panic!("query over a missing table must fail"),
        Err(e) => e.to_string(),
    };
    // The real server message (not the terse "db error") is present...
    assert!(
        msg.contains("does not exist"),
        "expected the Postgres cause, got: {msg}"
    );
    // ...along with the failing SQL, naming the table.
    assert!(
        msg.contains("sql:") && msg.contains("no_such_table"),
        "expected the failing SQL, got: {msg}"
    );
    // ...and the bind-parameter count, without leaking any values.
    assert!(
        msg.contains("bind parameter"),
        "expected the bind count, got: {msg}"
    );
    Ok(())
}

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

/// The SQL echo (Settings → Development, §16): with the switch on, every
/// statement this driver sends is printed to **stdout** with its binds; with it
/// off, nothing is.
///
/// Driven through the real driver rather than the renderer, because the claim
/// is about what a *running* server prints — a statement rendered but never
/// echoed, or echoed but never run, would both pass a test of the formatter.
#[tokio::test]
async fn the_sql_echo_prints_every_statement_with_its_binds() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());
    driver
        .apply_schema(&sc_db::SchemaChange::CreateTable {
            name: "sc_log_book".into(),
            columns: vec![
                sc_db::ColumnDef::new("id", "int8").not_null(),
                sc_db::ColumnDef::new("title", "text").not_null(),
            ],
            primary_key: vec!["id".into()],
            unlogged: false,
        })
        .await?;

    let insert = Insert::row(
        "sc_log_book",
        vec!["id".into(), "title".into()],
        vec![
            Expr::lit(Value::Int(1)),
            Expr::lit(Value::Text("The Log Book".into())),
        ],
    );

    // Off: the statement runs and says nothing. The guard holds the switches for
    // the duration, so a test beside this one cannot turn the echo on halfway.
    let _log = sc_log::capture::guard(sc_log::DEFAULT_VERBOSITY);
    driver.query(&Statement::from(insert.clone())).await?;
    let silent = sc_log::capture::take();
    assert!(silent.is_empty(), "{silent:?}");

    // On: the SQL, its bind values, and stdout — the stream somebody redirects.
    sc_log::set_log_sql(true);
    let select = Select::from(Source::table("sc_log_book"))
        .columns(vec![Projection::expr(Expr::col("title"))])
        .filter(Expr::col("id").eq(Expr::lit(Value::Int(1))));
    driver.query(&Statement::from(select)).await?;
    let echoed = sc_log::capture::take_stdout().join("\n");

    assert!(echoed.contains("SELECT"), "{echoed}");
    assert!(echoed.contains("sc_log_book"), "{echoed}");
    // The values, not just their count: a statement without its parameters does
    // not say what ran.
    assert!(echoed.contains("Int(1)"), "{echoed}");
    Ok(())
}
