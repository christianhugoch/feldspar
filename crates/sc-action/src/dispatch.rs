//! Dispatch: turning one event into the runs of the triggers that listen for it
//! (design §10.2, TODO Phases 4 and 5).
//!
//! For a **table write** this is the other side of the [emit
//! seam](sc_catalog::TableEvents): the row layer says *a write happened*, and
//! [`TriggerDispatcher`] decides what that means. It is installed into the
//! catalog once at boot, so the layering stays one-way — nothing below layer 6
//! names a trigger, and the writer above it names only the catalog.
//!
//! Every **other** event is raised by something that already sits above this
//! crate and can simply hold the dispatcher: a successful login and an error
//! becoming a response are fired by the server (one place each, so no handler has
//! to remember to), `startup` by the boot path, and a `none` trigger by whoever
//! asks for it ([`run_trigger`](TriggerDispatcher::run_trigger)). Those need no
//! seam — the seam exists for the row layer's sake, not for the layering's.
//!
//! Three entry points, differing only in what happens to a failure:
//! [`dispatch`](TriggerDispatcher::dispatch) returns every outcome,
//! [`fire`](TriggerDispatcher::fire) reports them (nobody is waiting), and
//! [`run_trigger`](TriggerDispatcher::run_trigger) returns the one result
//! (somebody is).
//!
//! ## What one dispatch does
//!
//! Look up the triggers for the event's `(kind, channel)` — a **lookup**, from
//! the cached [`Triggers`] set, never a query — and for each, in name order:
//!
//! 1. **Check the depth** ([`Event::firing`]). A trigger past `MAX_DEPTH` does not
//!    run, and the error names the whole chain.
//! 2. **Evaluate the `only_if`** against the affected row, reified. An evaluator
//!    error means the trigger does **not** run — the same fail-closed contract an
//!    ownership formula has, for the same reason: a predicate that could not be
//!    decided has not said yes.
//! 3. **Run the action**, with the chain [`firing`](Event::firing) returned, so
//!    any write *it* makes carries what led there and the next level down knows
//!    how deep it is.
//!
//! ## One trigger's failure is one trigger's failure
//!
//! Every run is independent: a failing action is reported and the next trigger
//! still runs, and none of it reaches the write that caused it — which has
//! already committed (decision 1). A request that inserted a row gets its row
//! back even when the audit trigger it fired is misconfigured, and the reason is
//! reported rather than lost. Where "reported" lands is §16's error log, which is
//! not this milestone; for now it is stderr, like every other server-side
//! diagnostic.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sc_catalog::{CallerContext, Catalog, TableEvents, TableWrite, WriteOp, prefetch_bindings};
use sc_error::{Error, Result};
use sc_expr::{Ambient, Formula, JsEvaluator, Operation, value_from_json};
use sc_query::Value;
use serde_json::Value as Json;

use crate::action::ActionContext;
use crate::event::{Event, EventKind, ROLE_PUBLIC};
use crate::registry::ActionRegistry;
use crate::scope::typed_value;
use crate::trigger::Trigger;
use crate::triggers::Triggers;
use crate::validate::trigger_shape;

/// What one trigger did about one event.
#[derive(Debug)]
pub struct TriggerRun {
    /// The trigger's name.
    pub trigger: String,
    /// `Ok(Some(result))` — it ran, and this is what its action returned;
    /// `Ok(None)` — its `only_if` did not select this row, so nothing ran;
    /// `Err` — it could not run, or its action failed.
    pub outcome: Result<Option<Json>>,
}

impl TriggerRun {
    /// Whether the action actually ran.
    pub fn fired(&self) -> bool {
        matches!(self.outcome, Ok(Some(_)))
    }
}

/// The live trigger set, the actions it can run, and the engine its formulas
/// evaluate in — everything one dispatch needs, held in one place a server
/// installs once.
///
/// Cheap to share (`Arc` it): the trigger set is behind an `RwLock` so a save can
/// swap it in ([`set_triggers`](TriggerDispatcher::set_triggers)) while events
/// keep firing.
pub struct TriggerDispatcher {
    registry: Arc<ActionRegistry>,
    triggers: RwLock<Arc<Triggers>>,
    evaluator: Option<Arc<dyn JsEvaluator>>,
}

impl TriggerDispatcher {
    /// A dispatcher over `registry`, with no triggers yet — a server calls
    /// [`reload`](TriggerDispatcher::reload) once the catalog is up.
    pub fn new(registry: Arc<ActionRegistry>) -> TriggerDispatcher {
        TriggerDispatcher {
            registry,
            triggers: RwLock::new(Arc::new(Triggers::empty())),
            evaluator: None,
        }
    }

    /// Supply the JavaScript engine (the server's one isolate, §7.3), which an
    /// `only_if` and most actions' configuration need.
    pub fn with_evaluator(mut self, evaluator: Arc<dyn JsEvaluator>) -> TriggerDispatcher {
        self.evaluator = Some(evaluator);
        self
    }

    /// The actions this dispatcher can run.
    pub fn registry(&self) -> &Arc<ActionRegistry> {
        &self.registry
    }

    /// The live trigger set, including the ones that failed validation and why.
    pub fn triggers(&self) -> Result<Arc<Triggers>> {
        let guard = self
            .triggers
            .read()
            .map_err(|_| Error::msg("trigger set lock poisoned"))?;
        Ok(Arc::clone(&guard))
    }

    /// Replace the live trigger set.
    pub fn set_triggers(&self, triggers: Triggers) -> Result<()> {
        let mut guard = self
            .triggers
            .write()
            .map_err(|_| Error::msg("trigger set lock poisoned"))?;
        *guard = Arc::new(triggers);
        Ok(())
    }

    /// Load and validate every stored trigger into the live set — at boot, and
    /// after any change to `_sc_triggers`.
    pub async fn reload(&self, catalog: &Catalog) -> Result<()> {
        let triggers = Triggers::load(catalog, &self.registry).await?;
        self.set_triggers(triggers)
    }

    /// Fire every trigger listening for `event`, in name order, and report what
    /// each of them did.
    ///
    /// Never fails as a whole: a trigger that cannot run contributes an `Err`
    /// outcome and the rest still run. The caller decides what to do with those —
    /// the emit path logs them, a test asserts on them.
    pub async fn dispatch(&self, catalog: &Catalog, event: &Event) -> Vec<TriggerRun> {
        // Cloned out of the lock: an action writes rows, which fire events, which
        // arrive back here — so nothing may be held across the await.
        let matched: Vec<Trigger> = match self.triggers() {
            Ok(triggers) => triggers.for_event(event).cloned().collect(),
            Err(e) => {
                return vec![TriggerRun {
                    trigger: String::new(),
                    outcome: Err(e),
                }];
            }
        };
        let mut runs = Vec::with_capacity(matched.len());
        for trigger in matched {
            let outcome = fire_trigger(
                catalog,
                &self.registry,
                self.evaluator.as_ref(),
                &trigger,
                event,
            )
            .await;
            runs.push(TriggerRun {
                trigger: trigger.name,
                outcome,
            });
        }
        runs
    }

    /// Fire `event` and **report** each trigger's failure rather than returning
    /// it — the fire-and-forget form every event that is not a request uses
    /// (login, startup, error, and the row layer's writes through
    /// [`emit`](TableEvents::emit)).
    ///
    /// The caller of a login is being logged in, and the caller of an error is
    /// already being told about a different failure; neither has any use for "and
    /// also your audit trigger is misconfigured", and neither should be made to
    /// fail for it. So the failure goes where a server-side failure goes, which
    /// is the log until §16's error log exists.
    ///
    /// **The `error` event is guarded against re-entrancy here**, and here only,
    /// so it cannot be bypassed by firing one another way: while this task is
    /// dispatching an error event, another error event on the same task is
    /// dropped. A misconfigured trigger must not become an infinite loop at the
    /// worst possible moment — which is precisely when an error event fires.
    pub async fn fire(&self, catalog: &Catalog, event: &Event) {
        if event.kind != EventKind::Error {
            self.report(catalog, event).await;
            return;
        }
        if HANDLING_ERROR.try_with(|()| ()).is_ok() {
            // Already inside an error event's dispatch on this task: the error
            // this one describes came from handling the last one.
            return;
        }
        HANDLING_ERROR
            .scope((), async { self.report(catalog, event).await })
            .await;
    }

    /// [`dispatch`](TriggerDispatcher::dispatch), logging what failed.
    async fn report(&self, catalog: &Catalog, event: &Event) {
        for run in self.dispatch(catalog, event).await {
            if let Err(e) = run.outcome {
                eprintln!(
                    "saltcorn: trigger `{}` on the {} event: {}",
                    run.trigger,
                    event.kind,
                    sc_error::format_chain(&e)
                );
            }
        }
    }

    /// Run **one trigger by name**, with `payload` as the event's payload, and
    /// return what its action returned.
    ///
    /// This is the `none` event's whole story — a trigger with no intrinsic
    /// occurrence runs when something asks it to — and it is the same call the
    /// admin's "run this now" button and an application's exposed trigger
    /// endpoint make. Unlike [`fire`](TriggerDispatcher::fire), the failure is
    /// **returned**: someone asked, so someone is waiting for the answer.
    ///
    /// The event is built from the trigger's own kind, so what a directly-run
    /// trigger sees matches what it would see when it fired by itself, minus what
    /// only the occurrence can supply: run a table trigger this way and there is
    /// no row, which its `only_if` will say so about rather than guess.
    /// `Json::Null` comes back when an `only_if` declines.
    pub async fn run_trigger(
        &self,
        catalog: &Catalog,
        name: &str,
        payload: Json,
        caller: Option<&CallerContext>,
    ) -> Result<Json> {
        let triggers = self.triggers()?;
        // `require` distinguishes "no such trigger" from "stored but not usable,
        // and here is why" — the second is the answer the asker needs.
        let trigger = triggers.require(name)?;
        // Disabled means disabled however it is asked. `require` resolves one
        // (that is how it stays fixable), so the check belongs here: an admin who
        // switched a trigger off and then pressed Run expects the switch to win.
        if !trigger.is_enabled() {
            return Err(Error::invalid(format!("trigger `{name}` is disabled")));
        }
        let mut event = Event::new(trigger.when).payload(payload);
        if let Some(channel) = &trigger.channel {
            event = event.on(channel.clone());
        }
        if let Some(caller) = caller {
            event = event
                .caller(caller.role, caller.user.clone())
                .chained(caller.chain.clone());
        }
        let result = fire_trigger(
            catalog,
            &self.registry,
            self.evaluator.as_ref(),
            trigger,
            &event,
        )
        .await?;
        Ok(result.unwrap_or(Json::Null))
    }
}

tokio::task_local! {
    /// Set for the duration of an error event's dispatch, so an error raised
    /// while handling one does not fire another (see
    /// [`fire`](TriggerDispatcher::fire)).
    ///
    /// A **task**-local rather than a flag on the dispatcher: the guard is about
    /// this error's own handling, and a shared flag would drop a genuinely
    /// unrelated error that happened to overlap with it. Two requests failing at
    /// once are two errors, and both deserve their event.
    static HANDLING_ERROR: ();
}

#[async_trait::async_trait]
impl TableEvents for TriggerDispatcher {
    /// Whether any enabled trigger listens for this write — the lookup that keeps
    /// the feature free for the writes that do not use it.
    fn observes(&self, table: &str, op: WriteOp) -> bool {
        match self.triggers() {
            Ok(triggers) => triggers
                .matching(event_kind(op), Some(table))
                .next()
                .is_some(),
            Err(_) => false,
        }
    }

    async fn emit(&self, catalog: &Catalog, write: TableWrite<'_>) -> Result<()> {
        // The write has committed, so a trigger that failed is reported rather
        // than returned — the same rule every non-request event follows.
        self.fire(catalog, &table_event(&write)).await;
        Ok(())
    }
}

/// The event a committed write raises.
///
/// The caller travels with the write ([`CallerContext`](sc_catalog::CallerContext)),
/// which is where the role, the user and the **chain** come from: a write made by
/// a request carries an empty chain (depth 0), and one made by a trigger's action
/// carries what `Event::firing` handed that action.
fn table_event(write: &TableWrite<'_>) -> Event {
    let mut event = Event::new(event_kind(write.op))
        .on(write.table.name.clone())
        .row(write.row.clone());
    if let Some(old) = &write.old_row {
        event = event.old_row(old.clone());
    }
    match write.caller {
        Some(caller) => event
            .caller(caller.role, caller.user.clone())
            .chained(caller.chain.clone()),
        None => event.caller(ROLE_PUBLIC, None),
    }
}

/// The event kind a write op raises. The two enums are deliberately separate —
/// one is a fact about a statement, the other about what can be listened for —
/// and this is the single place they meet.
fn event_kind(op: WriteOp) -> EventKind {
    match op {
        WriteOp::Insert => EventKind::Insert,
        WriteOp::Update => EventKind::Update,
        WriteOp::Delete => EventKind::Delete,
    }
}

/// Run one trigger for one event: the depth check, the `only_if`, the action.
///
/// `Ok(None)` means the `only_if` did not select this row — a trigger that
/// deliberately did nothing, which is not a failure and must not read as one.
pub async fn fire_trigger(
    catalog: &Catalog,
    registry: &ActionRegistry,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
    trigger: &Trigger,
    event: &Event,
) -> Result<Option<Json>> {
    // The cascade bound, checked before anything else runs: past it, the trigger
    // does not fire and the error names the whole chain (§10.2).
    let chain = event.firing(&trigger.name)?;
    if !only_if_selects(catalog, evaluator, trigger, event).await? {
        return Ok(None);
    }
    let action = registry.require(trigger.action.trim())?;
    let mut ctx =
        ActionContext::new(catalog, event, &trigger.configuration, &trigger.name).with_chain(chain);
    if let Some(evaluator) = evaluator {
        ctx = ctx.with_evaluator(evaluator);
    }
    action.run(&mut ctx).await.map(Some)
}

/// Whether the trigger's `only_if` selects this event's row. No formula is
/// "always".
///
/// Always **reified**: there is no statement for a translation to ride in on —
/// the row is in hand, already written. The formula ranges over the event's table
/// (decision 7's bare scope), with the event's `row`/`old`/`user` ambient, and
/// every Ⱶ-path or Ↄ-relation it reads resolved by the same `prefetch_bindings`
/// an ownership check and an action's `where` use.
///
/// An evaluator error propagates: the trigger does not run. "Could not decide"
/// is not "yes", and a trigger that fired on a broken predicate would be a write
/// nobody asked for.
async fn only_if_selects(
    catalog: &Catalog,
    evaluator: Option<&Arc<dyn JsEvaluator>>,
    trigger: &Trigger,
    event: &Event,
) -> Result<bool> {
    let Some(source) = trigger
        .only_if
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(true);
    };
    let named = |e: Error| Error::invalid(format!("trigger `{}`: `only if`: {e}", trigger.name));

    let table = catalog.require(event.require_channel()?)?;
    let formula = Formula::parse(source).map_err(named)?;
    let shape = trigger_shape(catalog, Some(&table.name))?;
    let analysis = formula.validate(&shape, &table.name).map_err(named)?;

    // The event's objects, each field typed by its own column, so what the
    // predicate reads and what a prefetch correlates on agree with the database.
    let bindings = crate::scope::EventBindings::with_values(event, |ambient, field, json| {
        match ambient {
            // Neither the caller nor the payload is typed here: neither ever
            // correlates a prefetch (and the payload has no columns to be typed
            // against at all).
            Ambient::User | Ambient::Payload => value_from_json(json),
            Ambient::Row | Ambient::Old => typed_value(Some(&table), field, json),
        }
    });
    // The bare scope is the **affected row**: `status` is the row this event is
    // about, `row.status` the same thing spelled the other way (and `old.status`
    // what it replaced).
    let mut values: BTreeMap<String, Value> = bindings
        .ambient
        .get(&Ambient::Row)
        .cloned()
        .flatten()
        .unwrap_or_default();
    prefetch_bindings(catalog, &table, &analysis, &shape, &mut values).await?;

    let evaluator = evaluator.ok_or_else(|| {
        Error::config(format!(
            "trigger `{}` has an `only if` formula but no JavaScript engine \
             is available in this context",
            trigger.name
        ))
    })?;
    let call = bindings.call(&formula, operation(event.kind), &values);
    evaluator.eval(call).await.map_err(named)
}

/// The operation an event's flags would fold to. An `only_if` may not name them
/// (validation refuses it — the trigger's own event *is* the operation), so this
/// only ever matters for honesty at the call.
fn operation(kind: EventKind) -> Operation {
    match kind {
        EventKind::Insert => Operation::Insert,
        EventKind::Update => Operation::Update,
        EventKind::Delete => Operation::Delete,
        _ => Operation::Read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_catalog::CallerContext;
    use serde_json::json;

    /// A `TableWrite` needs a `Table`, and a `Table` needs no database to be
    /// built from a physical description — which is what lets the event-shaping
    /// half of dispatch be tested here rather than only through the router.
    fn table(name: &str) -> sc_catalog::Table {
        sc_catalog::Table::from_physical(
            sc_catalog::DbId::primary(),
            &sc_db::PhysicalTable {
                name: name.to_owned(),
                schema: None,
                columns: vec![sc_db::Column {
                    name: "id".into(),
                    sql_type: "int8".into(),
                    nullable: false,
                    generated: None,
                }],
                primary_key: vec!["id".into()],
                foreign_keys: Vec::new(),
                constraints: Vec::new(),
            },
        )
    }

    #[test]
    fn a_write_becomes_the_event_its_caller_describes() {
        let books = table("books");
        let caller =
            CallerContext::new(1, Some(json!({ "email": "a@b.c" }))).chained(vec!["audit".into()]);
        let event = table_event(&TableWrite {
            table: &books,
            op: WriteOp::Update,
            row: json!({ "id": 1, "title": "now" }),
            old_row: Some(json!({ "id": 1, "title": "was" })),
            caller: Some(&caller),
        });
        assert_eq!(event.kind, EventKind::Update);
        assert_eq!(event.channel.as_deref(), Some("books"));
        assert_eq!(event.old_row.as_ref().unwrap()["title"], json!("was"));
        assert_eq!(event.role, 1);
        assert_eq!(event.user.as_ref().unwrap()["email"], json!("a@b.c"));
        // The chain travels with the caller, which is what bounds a cascade: this
        // event is already one deep.
        assert_eq!(event.depth(), 1);

        // A write with no caller is anonymous and at depth 0 — an insert's `old`
        // is absent, not null.
        let event = table_event(&TableWrite {
            table: &books,
            op: WriteOp::Insert,
            row: json!({ "id": 2 }),
            old_row: None,
            caller: None,
        });
        assert_eq!(event.kind, EventKind::Insert);
        assert_eq!(event.role, ROLE_PUBLIC);
        assert!(event.old_row.is_none());
        assert_eq!(event.depth(), 0);
    }

    #[test]
    fn nothing_is_observed_until_a_trigger_says_so() {
        let dispatcher = TriggerDispatcher::new(Arc::new(ActionRegistry::new()));
        assert!(!dispatcher.observes("books", WriteOp::Insert));

        let mut off = Trigger::new("off", EventKind::Delete, "insert_row").on("books");
        off.set_enabled(false);
        dispatcher
            .set_triggers(Triggers::of(vec![
                Trigger::new("audit", EventKind::Insert, "insert_row").on("books"),
                off,
            ]))
            .unwrap();

        assert!(dispatcher.observes("books", WriteOp::Insert));
        // The wrong table, the wrong operation, and a disabled trigger each mean
        // the write pays nothing: no pre-image fetch, no dispatch.
        assert!(!dispatcher.observes("authors", WriteOp::Insert));
        assert!(!dispatcher.observes("books", WriteOp::Update));
        assert!(!dispatcher.observes("books", WriteOp::Delete));
    }
}
