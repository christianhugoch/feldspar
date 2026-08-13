//! Integration test: `PgDriver::apply_schema` creates/drops tables and columns
//! against a real Postgres, verified by introspecting the result. Also asserts
//! the "no invented `id` column" rule and `if_exists` semantics.

use sc_db::{ColumnDef, ColumnGenerator, SchemaChange};
use sc_db_postgres::PgDriver;
use sc_test_harness::TestDb;

/// Introspect and return the named table, or `None` if it does not exist.
async fn table(driver: &PgDriver, name: &str) -> sc_error::Result<Option<sc_db::PhysicalTable>> {
    Ok(driver
        .introspect()
        .await?
        .into_iter()
        .find(|t| t.name == name))
}

#[tokio::test]
async fn create_alter_and_drop_round_trip_through_introspect() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());

    // Create a table with an explicit composite primary key and no id column.
    driver
        .apply_schema(&SchemaChange::CreateTable {
            name: "sc_as_thing".into(),
            columns: vec![
                ColumnDef::new("code", "text").not_null(),
                ColumnDef::new("kind", "int8").not_null(),
                ColumnDef::new("label", "text"),
            ],
            primary_key: vec!["code".into(), "kind".into()],
            unlogged: false,
        })
        .await?;

    let t = table(&driver, "sc_as_thing").await?.expect("table created");
    // Exactly the declared columns, in order — crucially, no invented `id`.
    assert_eq!(
        t.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["code", "kind", "label"]
    );
    assert!(t.columns.iter().all(|c| c.name != "id"));
    assert_eq!(t.primary_key, vec!["code", "kind"]);
    let label = t.columns.iter().find(|c| c.name == "label").unwrap();
    assert!(label.nullable);
    let code = t.columns.iter().find(|c| c.name == "code").unwrap();
    assert!(!code.nullable);

    // Add a column with a default.
    driver
        .apply_schema(&SchemaChange::AddColumn {
            table: "sc_as_thing".into(),
            column: ColumnDef::new("active", "bool").not_null().default("true"),
        })
        .await?;
    let t = table(&driver, "sc_as_thing").await?.unwrap();
    let active = t
        .columns
        .iter()
        .find(|c| c.name == "active")
        .expect("column added");
    assert_eq!(active.sql_type, "bool");
    assert!(matches!(&active.generated, Some(ColumnGenerator::Default(d)) if d.contains("true")));

    // Drop a column.
    driver
        .apply_schema(&SchemaChange::DropColumn {
            table: "sc_as_thing".into(),
            column: "label".into(),
            if_exists: false,
        })
        .await?;
    let t = table(&driver, "sc_as_thing").await?.unwrap();
    assert!(t.columns.iter().all(|c| c.name != "label"));

    // Dropping a missing column errors without `if_exists`, succeeds with it.
    assert!(
        driver
            .apply_schema(&SchemaChange::DropColumn {
                table: "sc_as_thing".into(),
                column: "label".into(),
                if_exists: false,
            })
            .await
            .is_err()
    );
    driver
        .apply_schema(&SchemaChange::DropColumn {
            table: "sc_as_thing".into(),
            column: "label".into(),
            if_exists: true,
        })
        .await?;

    // Drop the table.
    driver
        .apply_schema(&SchemaChange::DropTable {
            name: "sc_as_thing".into(),
            if_exists: false,
        })
        .await?;
    assert!(table(&driver, "sc_as_thing").await?.is_none());

    // Dropping a missing table errors without `if_exists`, succeeds with it.
    assert!(
        driver
            .apply_schema(&SchemaChange::DropTable {
                name: "sc_as_thing".into(),
                if_exists: false,
            })
            .await
            .is_err()
    );
    driver
        .apply_schema(&SchemaChange::DropTable {
            name: "sc_as_thing".into(),
            if_exists: true,
        })
        .await?;

    Ok(())
}

/// `render_ddl` is the DDL `apply_schema` would run — **and does not run it**.
///
/// It is what writes an application's generated `schema.sql` (§13.3): a
/// description of the tables a coding agent may write SQL against. A renderer
/// that quietly applied what it rendered would turn opening a project into a
/// migration, and a renderer that disagreed with `apply_schema` would describe a
/// schema the database does not have — which is the whole reason this is the
/// driver's job rather than a second DDL writer's.
#[tokio::test]
async fn render_ddl_is_what_apply_schema_would_run_and_applies_nothing() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());
    let change = SchemaChange::CreateTable {
        name: "sc_rd_thing".into(),
        columns: vec![
            ColumnDef::new("code", "text").not_null(),
            ColumnDef::new("label", "text"),
        ],
        primary_key: vec!["code".into()],
        unlogged: false,
    };

    let sql = driver.render_ddl(&change)?;
    assert_eq!(
        sql,
        "CREATE TABLE \"sc_rd_thing\" (\"code\" text NOT NULL, \"label\" text, \
         PRIMARY KEY (\"code\"))"
    );
    assert!(
        table(&driver, "sc_rd_thing").await?.is_none(),
        "rendering must not apply"
    );

    // The rendered text is exactly what the database accepts: applying the same
    // change produces the table the statement describes.
    driver.apply_schema(&change).await?;
    let created = table(&driver, "sc_rd_thing").await?.expect("created");
    assert_eq!(created.primary_key, ["code"]);
    Ok(())
}
