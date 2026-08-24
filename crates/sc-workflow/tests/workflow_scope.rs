//! What a workflow step's formulas may name (§10.3, phase 1.8).
//!
//! Against a real catalog, because the claim is about the scope a *stored* table
//! gives a formula: a workflow on `orders` sees `row.total` because `orders` has
//! a `total`, and would be told about `row.totl` on save.
//!
//! The one thing a workflow adds to an action's scope is the ambient `context`,
//! and the two assertions that matter are that it *is* there inside a workflow
//! and *is not* there outside one.

use std::sync::Arc;

use sc_action::action_shape;
use sc_catalog::{Catalog, DataField};
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::Result;
use sc_expr::{Ambient, Formula};
use sc_test_harness::TestDb;
use sc_types::{BasicType, TypeRef};
use sc_workflow::{WORKFLOW_SCOPE, workflow_shape};

async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    catalog
        .create_table(
            "orders",
            &[
                DataField::plain("id", TypeRef::Basic(BasicType::Int))
                    .required()
                    .primary_key(),
                DataField::plain("total", TypeRef::Basic(BasicType::Int)),
            ],
        )
        .await?;
    Ok(catalog)
}

#[tokio::test]
async fn a_step_reads_the_run_context_and_the_event_that_started_it() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    let shape = workflow_shape(&catalog, Some("orders"))?;

    // Everything an action's configuration sees, plus the context.
    let analysis = Formula::parse("context.total > row.total && user.id !== null")?
        .validate(&shape, WORKFLOW_SCOPE)?;
    assert!(analysis.uses(Ambient::Context));
    assert!(analysis.uses(Ambient::Row) && analysis.uses(Ambient::User));

    // `row` is still checked against the table's real fields: a workflow does
    // not loosen the scope, it adds to it.
    let err = Formula::parse("row.totl > 1")?
        .validate(&shape, WORKFLOW_SCOPE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("totl"), "{err}");

    // Bare identifiers still mean "a field of the row this formula ranges
    // over", which for a step is nothing: writing `total` where `context.total`
    // was meant is refused by name rather than read as the context.
    let err = Formula::parse("total > 1")?
        .validate(&shape, WORKFLOW_SCOPE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("total"), "{err}");
    Ok(())
}

#[tokio::test]
async fn outside_a_run_there_is_no_context_to_read() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;

    // The same formula in an ordinary action's configuration: `context` is not
    // an identifier there, and the admin is told so while they are still looking
    // at the form.
    let action = action_shape(&catalog, Some("orders"))?;
    assert!(!action.declares_ambient(Ambient::Context));
    let err = Formula::parse("context.total > 1")?
        .validate(&action, WORKFLOW_SCOPE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown identifier `context`"), "{err}");

    // A workflow on a channel-less event has a context but no row, and says so
    // about each: one is in scope, the other is not.
    let channel_less = workflow_shape(&catalog, None)?;
    assert!(channel_less.declares_ambient(Ambient::Context));
    assert!(!channel_less.declares_ambient(Ambient::Row));
    Ok(())
}
