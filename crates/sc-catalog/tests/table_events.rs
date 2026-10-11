//! The emit seam holds **more than one** listener (TODO.md "Live updates" L3.2,
//! done with L2.6, which needed it first).
//!
//! Before live updates the catalog held one `TableEvents` and installing a
//! second replaced the first. The trigger dispatcher is installed there, so a
//! live hub installed after it would have silently switched off every table
//! trigger. What is claimed here, against a real catalog:
//!
//! - listeners under different keys **both** hear a write they observe;
//! - `observes_writes` is the OR over listeners, and a listener that does not
//!   observe a write is not handed it;
//! - installing under a key that is taken **replaces** — two trigger
//!   dispatchers would fire every trigger twice;
//! - one listener failing does not starve the next, and the error still comes
//!   back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sc_catalog::{
    Catalog, DataField, LIVE_EVENTS, TRIGGER_EVENTS, TableEvents, TableWrite, WriteOp,
};
use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};
use sc_test_harness::TestDb;
use sc_types::{BasicType, TypeRef};
use serde_json::json;

/// A listener that records what it was handed, observing only `table`.
struct Recorder {
    table: &'static str,
    heard: Mutex<Vec<String>>,
    fail: bool,
}

impl Recorder {
    fn on(table: &'static str) -> Arc<Recorder> {
        Arc::new(Recorder {
            table,
            heard: Mutex::new(Vec::new()),
            fail: false,
        })
    }

    fn failing(table: &'static str) -> Arc<Recorder> {
        Arc::new(Recorder {
            table,
            heard: Mutex::new(Vec::new()),
            fail: true,
        })
    }

    fn heard(&self) -> Vec<String> {
        self.heard.lock().unwrap().clone()
    }
}

#[async_trait]
impl TableEvents for Recorder {
    fn observes(&self, table: &str, _op: WriteOp) -> bool {
        table == self.table
    }

    async fn emit(&self, _catalog: &Catalog, write: TableWrite<'_>) -> Result<()> {
        self.heard
            .lock()
            .unwrap()
            .push(format!("{} {}", write.op, write.table.name));
        if self.fail {
            return Err(Error::msg("this listener always fails"));
        }
        Ok(())
    }
}

async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver).await?;
    for name in ["cards", "boards"] {
        catalog
            .create_table(
                name,
                &[DataField::plain("title", TypeRef::Basic(BasicType::Text))],
            )
            .await?;
    }
    Ok(catalog)
}

fn write<'a>(table: &'a sc_catalog::Table, op: WriteOp) -> TableWrite<'a> {
    TableWrite {
        table,
        op,
        row: json!({ "id": 1, "title": "x" }),
        old_row: None,
        caller: None,
        tx: None,
    }
}

#[tokio::test]
async fn two_listeners_under_two_keys_both_hear_what_they_observe() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    let triggers = Recorder::on("cards");
    let live = Recorder::on("boards");
    catalog.add_table_events(TRIGGER_EVENTS, triggers.clone() as Arc<dyn TableEvents>)?;
    catalog.add_table_events(LIVE_EVENTS, live.clone() as Arc<dyn TableEvents>)?;

    assert!(catalog.observes_writes("cards", WriteOp::Insert));
    assert!(catalog.observes_writes("boards", WriteOp::Update));
    assert!(!catalog.observes_writes("notes", WriteOp::Insert));

    let cards = catalog.require("cards")?;
    let boards = catalog.require("boards")?;
    catalog.emit_write(write(&cards, WriteOp::Insert)).await?;
    catalog.emit_write(write(&boards, WriteOp::Delete)).await?;
    assert_eq!(triggers.heard(), vec!["insert cards"]);
    assert_eq!(live.heard(), vec!["delete boards"], "only what it observes");
    Ok(())
}

#[tokio::test]
async fn a_second_install_under_the_same_key_replaces_the_first() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    let first = Recorder::on("cards");
    let second = Recorder::on("cards");
    catalog.add_table_events(TRIGGER_EVENTS, first.clone() as Arc<dyn TableEvents>)?;
    catalog.add_table_events(TRIGGER_EVENTS, second.clone() as Arc<dyn TableEvents>)?;
    let cards = catalog.require("cards")?;
    catalog.emit_write(write(&cards, WriteOp::Update)).await?;
    assert!(first.heard().is_empty(), "replaced, so never fired");
    assert_eq!(second.heard(), vec!["update cards"]);

    assert!(catalog.remove_table_events(TRIGGER_EVENTS));
    assert!(!catalog.remove_table_events(TRIGGER_EVENTS));
    assert!(!catalog.observes_writes("cards", WriteOp::Update));
    Ok(())
}

#[tokio::test]
async fn one_failing_listener_does_not_starve_the_next() -> Result<()> {
    let db = TestDb::new().await?;
    let catalog = catalog(&db).await?;
    let failing = Recorder::failing("cards");
    let after = Recorder::on("cards");
    catalog.add_table_events(TRIGGER_EVENTS, failing.clone() as Arc<dyn TableEvents>)?;
    catalog.add_table_events(LIVE_EVENTS, after.clone() as Arc<dyn TableEvents>)?;
    let cards = catalog.require("cards")?;
    let error = catalog
        .emit_write(write(&cards, WriteOp::Insert))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("always fails"), "{error}");
    assert_eq!(
        after.heard(),
        vec!["insert cards"],
        "still handed the write"
    );
    Ok(())
}
