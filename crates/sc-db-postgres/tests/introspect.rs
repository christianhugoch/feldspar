//! Integration test: `PgDriver::introspect` reads a real database's live schema
//! into `PhysicalTable`s, including composite primary keys and foreign keys to
//! non-primary-key columns.
//!
//! The test asserts on its own distinctively named tables rather than the exact
//! set the database contains, so it holds whether or not the database has other
//! tables — which is itself the point of "no discovery step": every reachable
//! table is returned without any registration.

use sc_db_postgres::PgDriver;
use sc_test_harness::TestDb;

#[tokio::test]
async fn introspects_columns_keys_and_foreign_keys() -> sc_error::Result<()> {
    let db = TestDb::new().await?;
    let client = db.client().await?;

    // `sc_it_org` has a single-column PK plus a UNIQUE non-key column (`code`)
    // that a foreign key can target. `sc_it_member` has a composite PK and two
    // foreign keys: one to the PK, one to the non-PK `code` column. Its `active`
    // column carries a default so default capture is exercised.
    client
        .batch_execute(
            "CREATE TABLE sc_it_org (id int8 PRIMARY KEY, code text UNIQUE NOT NULL);
             CREATE TABLE sc_it_member (
                 org       int8 NOT NULL,
                 user_id   int8 NOT NULL,
                 email     text,
                 org_code  text,
                 active    bool NOT NULL DEFAULT true,
                 PRIMARY KEY (org, user_id),
                 FOREIGN KEY (org) REFERENCES sc_it_org (id),
                 FOREIGN KEY (org_code) REFERENCES sc_it_org (code)
             );",
        )
        .await
        .map_err(|e| sc_error::Error::database(format!("create schema: {e}")))?;

    let driver = PgDriver::from_pool(db.pool().clone());
    let tables = driver.introspect().await?;

    let org = tables
        .iter()
        .find(|t| t.name == "sc_it_org")
        .expect("sc_it_org table");
    assert_eq!(org.schema.as_deref(), Some("public"));
    assert_eq!(org.primary_key, vec!["id"]);
    let id = org.columns.iter().find(|c| c.name == "id").expect("id col");
    assert_eq!(id.sql_type, "int8");
    assert!(!id.nullable);
    let code = org.columns.iter().find(|c| c.name == "code").expect("code");
    assert_eq!(code.sql_type, "text");
    assert!(!code.nullable);
    // `sc_it_org` is only referenced; it declares no foreign keys of its own.
    assert!(org.foreign_keys.is_empty());

    let member = tables
        .iter()
        .find(|t| t.name == "sc_it_member")
        .expect("sc_it_member table");

    // Composite primary key, in declaration order.
    assert_eq!(member.primary_key, vec!["org", "user_id"]);

    // Column types, nullability, and a captured default.
    let email = member.columns.iter().find(|c| c.name == "email").unwrap();
    assert_eq!(email.sql_type, "text");
    assert!(email.nullable);
    let org_col = member.columns.iter().find(|c| c.name == "org").unwrap();
    assert!(!org_col.nullable);
    let active = member.columns.iter().find(|c| c.name == "active").unwrap();
    assert_eq!(active.sql_type, "bool");
    assert!(
        active.default.as_deref().unwrap_or("").contains("true"),
        "expected a default containing `true`, got {:?}",
        active.default
    );

    // Two foreign keys; look them up by their local column so the assertions do
    // not depend on constraint ordering.
    assert_eq!(member.foreign_keys.len(), 2);
    let to_pk = member
        .foreign_keys
        .iter()
        .find(|f| f.columns == vec!["org".to_string()])
        .expect("fk on org");
    assert_eq!(to_pk.referenced_table, "sc_it_org");
    assert_eq!(to_pk.referenced_columns, vec!["id"]);

    // The foreign key that targets a *non-primary-key* column.
    let to_non_pk = member
        .foreign_keys
        .iter()
        .find(|f| f.columns == vec!["org_code".to_string()])
        .expect("fk on org_code");
    assert_eq!(to_non_pk.referenced_table, "sc_it_org");
    assert_eq!(to_non_pk.referenced_columns, vec!["code"]);

    Ok(())
}
