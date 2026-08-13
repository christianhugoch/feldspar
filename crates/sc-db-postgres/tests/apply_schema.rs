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

/// The constraint changes, all five, applied and then read back.
///
/// The round trip is the point rather than the DDL (which is unit-tested in
/// `ddl.rs`): a constraint is **stored in the database and nowhere else**, so a
/// constraint that applied but did not introspect back would be invisible to
/// every screen that shows one, and the comment carrying its error message would
/// be a message nobody could read.
#[tokio::test]
async fn constraints_indexes_and_comments_round_trip_through_introspect() -> sc_error::Result<()> {
    use sc_db::{CommentTarget, IndexOn, PhysicalConstraintKind};

    let db = TestDb::new().await?;
    let driver = PgDriver::from_pool(db.pool().clone());
    driver
        .apply_schema(&SchemaChange::CreateTable {
            name: "sc_ct_thing".into(),
            columns: vec![
                ColumnDef::new("id", "int8").not_null().identity(),
                ColumnDef::new("org", "int8").not_null(),
                ColumnDef::new("email", "text").not_null(),
                ColumnDef::new("note", "text"),
            ],
            primary_key: vec!["id".into()],
            unlogged: false,
        })
        .await?;

    for change in [
        SchemaChange::AddUniqueConstraint {
            table: "sc_ct_thing".into(),
            name: "sc_uq_ct_org_email".into(),
            columns: vec!["org".into(), "email".into()],
        },
        SchemaChange::SetComment {
            target: CommentTarget::Constraint {
                table: "sc_ct_thing".into(),
                name: "sc_uq_ct_org_email".into(),
            },
            comment: Some("{\"message\":\"that's taken\"}".into()),
        },
        SchemaChange::CreateIndex {
            table: "sc_ct_thing".into(),
            name: "sc_ix_ct_note".into(),
            on: IndexOn::Columns(vec!["note".into()]),
            method: None,
        },
        SchemaChange::CreateIndex {
            table: "sc_ct_thing".into(),
            name: "sc_fts_ct".into(),
            on: IndexOn::Expression(
                "to_tsvector('english'::regconfig, coalesce(\"email\", ''))".into(),
            ),
            method: Some("gin".into()),
        },
    ] {
        driver.apply_schema(&change).await?;
    }

    let t = table(&driver, "sc_ct_thing").await?.expect("table");
    let by_name = |name: &str| t.constraints.iter().find(|c| c.name == name).cloned();

    let unique = by_name("sc_uq_ct_org_email").expect("unique constraint");
    assert_eq!(
        unique.kind,
        PhysicalConstraintKind::Unique {
            // In the order the key was declared: a jointly-unique key read back
            // with its columns swapped would compare unequal to the one the
            // admin asked for and be offered as a second constraint to add.
            columns: vec!["org".into(), "email".into()],
        }
    );
    assert_eq!(
        unique.comment.as_deref(),
        Some("{\"message\":\"that's taken\"}")
    );

    match by_name("sc_ix_ct_note").expect("index").kind {
        PhysicalConstraintKind::Index {
            columns,
            expression,
            method,
        } => {
            assert_eq!(columns, ["note"]);
            assert_eq!(expression, None);
            assert_eq!(method, "btree");
        }
        other => panic!("expected an index, got {other:?}"),
    }

    match by_name("sc_fts_ct").expect("fts index").kind {
        PhysicalConstraintKind::Index {
            expression, method, ..
        } => {
            assert_eq!(method, "gin");
            assert!(
                expression.unwrap_or_default().contains("to_tsvector"),
                "an expression index reports the expression it is over"
            );
        }
        other => panic!("expected an index, got {other:?}"),
    }

    // The primary key's own index is **not** reported as an index: it is the
    // primary key, already reported as that, and offering a `DROP INDEX` for it
    // would be offering something Postgres refuses.
    assert!(
        !t.constraints
            .iter()
            .any(|c| c.name == "sc_ct_thing_pkey" || c.name == "sc_uq_ct_org_email_key"),
        "constraint-owned indexes are not reported twice: {:?}",
        t.constraints
    );

    // Dropping takes them out again, comment and all.
    for change in [
        SchemaChange::DropConstraint {
            table: "sc_ct_thing".into(),
            name: "sc_uq_ct_org_email".into(),
            if_exists: true,
        },
        SchemaChange::DropIndex {
            name: "sc_ix_ct_note".into(),
            if_exists: true,
        },
        SchemaChange::DropIndex {
            name: "sc_fts_ct".into(),
            if_exists: true,
        },
    ] {
        driver.apply_schema(&change).await?;
    }
    let t = table(&driver, "sc_ct_thing").await?.expect("table");
    assert!(t.constraints.is_empty(), "{:?}", t.constraints);
    Ok(())
}
