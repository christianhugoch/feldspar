//! Phase 5: the `error` event's re-entrancy guard (§10.2, §16).
//!
//! The property: **an error raised while handling an error event does not fire
//! another one.** Without it, one misconfigured trigger becomes an infinite loop
//! at the single worst moment — the server is already failing, and the loop is
//! what turns a failing request into a failing process.
//!
//! It is asserted here rather than through the server because the loop needs an
//! action that raises an error event *from inside* one, which no built-in action
//! can do (and should not be able to). A purpose-built action can, so this test
//! writes one: it counts its runs and re-fires the same event. With the guard it
//! runs once; without it, it would run until the action's own escape hatch stops
//! it — which is why the action has one, rather than hanging the suite.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use sc_action::{
    Action, ActionContext, ActionRegistry, Event, EventKind, Trigger, TriggerDispatcher,
    bootstrap_triggers, save_trigger,
};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::{ErrorKind, Result};
use sc_test_harness::TestDb;
use sc_types::{BasicType, FormField};
use serde_json::{Value as Json, json};

/// How many times the action will re-fire before giving up. Without the guard the
/// recursion is unbounded, so the *test* must be bounded: this is the escape
/// hatch that turns "the guard is broken" into a failed assertion rather than a
/// hung suite.
const GIVE_UP_AFTER: usize = 4;

/// An action that raises an `error` event from inside one — the only way to
/// exercise re-entrancy, and deliberately not something a built-in action can do.
struct ReFire {
    runs: Arc<AtomicUsize>,
    /// Set after the dispatcher exists, because the dispatcher holds the registry
    /// that holds this action: the cycle is broken by filling it in afterwards.
    dispatcher: Arc<OnceLock<Arc<TriggerDispatcher>>>,
}

#[async_trait::async_trait]
impl Action for ReFire {
    fn name(&self) -> &str {
        "refire"
    }

    fn description(&self) -> &str {
        "Raise an error event from inside one (test only)"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![FormField::new("label", BasicType::Text).required()]
    }

    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        if run <= GIVE_UP_AFTER
            && let Some(dispatcher) = self.dispatcher.get()
        {
            let event = Event::error(ErrorKind::System, "and again", "GET", "/again");
            dispatcher.fire(ctx.catalog, &event).await;
        }
        Ok(json!({ "run": run }))
    }
}

async fn setup(db: &TestDb) -> Result<(Arc<Catalog>, Arc<TriggerDispatcher>, Arc<AtomicUsize>)> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Arc::new(Catalog::init(driver as Arc<dyn DatabaseDriver>).await?);
    bootstrap_triggers(&catalog).await?;

    let runs = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(OnceLock::new());
    let mut registry = ActionRegistry::new();
    registry.register(Arc::new(ReFire {
        runs: Arc::clone(&runs),
        dispatcher: Arc::clone(&slot),
    }))?;
    let registry = Arc::new(registry);

    let trigger = Trigger::new("on_error", EventKind::Error, "refire").config("label", "x");
    save_trigger(&catalog, &registry, &trigger).await?;

    let dispatcher = Arc::new(TriggerDispatcher::new(registry));
    dispatcher.reload(&catalog).await?;
    slot.set(Arc::clone(&dispatcher)).ok();
    Ok((catalog, dispatcher, runs))
}

#[tokio::test]
async fn an_error_raised_while_handling_one_does_not_fire_another() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, dispatcher, runs) = setup(&db).await?;

    let event = Event::error(
        ErrorKind::System,
        "the first failure",
        "POST",
        "/api/things",
    );
    dispatcher.fire(&catalog, &event).await;

    // Once. The action re-fired an error event and the guard dropped it, so the
    // trigger did not run again — and the recursion never reached its escape
    // hatch, which is what "does not loop" means here.
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn the_guard_is_only_for_the_error_that_is_being_handled() -> Result<()> {
    let db = TestDb::new().await?;
    let (catalog, dispatcher, runs) = setup(&db).await?;

    // Two errors, one after the other: each is its own occurrence and each fires.
    // A flag on the dispatcher (rather than on the task handling one error) would
    // be indistinguishable here and wrong in the case this stands in for — two
    // requests failing at once, which is two errors and two events.
    for message in ["the first failure", "an unrelated second failure"] {
        let event = Event::error(ErrorKind::Application, message, "GET", "/api/things");
        dispatcher.fire(&catalog, &event).await;
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);

    // And an event of another kind is not guarded at all: `fire` treats only the
    // error event specially, because only it can be raised by its own handling.
    let runs_before = runs.load(Ordering::SeqCst);
    dispatcher.fire(&catalog, &Event::startup()).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        runs_before,
        "no trigger listens for startup here"
    );
    Ok(())
}
