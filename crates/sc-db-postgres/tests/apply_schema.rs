//! Integration test: `PgDriver::apply_schema` creates/drops tables and columns
//! against a real Postgres, verified by introspecting the result. Also asserts
//! the "no invented `id` column" rule and `if_exists` semantics.

use sc_db::{ColumnDef, SchemaChange};
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
    assert!(active.default.as_deref().unwrap_or("").contains("true"));

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
