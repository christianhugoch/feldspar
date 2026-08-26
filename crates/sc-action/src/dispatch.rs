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
//! 3. **Run the body**, with the chain [`firing`](Event::firing) returned, so
//!    any write *it* makes carries what led there and the next level down knows
//!    how deep it is. An `Action` body runs its action here and answers what it
//!    returned; a `Workflow` body starts a durable run through the
//!    [`WorkflowEngine`](crate::WorkflowEngine) seam and answers the run's id and
//!    state, because a workflow may still be going long after this call is over
//!    (§10.3).
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

use sc_catalog::{
    CallerContext, Catalog, SharedTx, TableEvents, TableWrite, WriteOp, prefetch_bindings,
};
use sc_email::Mailer;
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

/// The process-wide things an action run may need but cannot build for itself:
/// the JavaScript engine and the mail transport.
///
/// One struct rather than a growing list of parameters, because these have
/// exactly the same shape and exactly the same rule — the process that serves
/// requests installs them once, a process that does not (a build tool, a unit
/// test) installs neither, and an action that needs one it was not given says so
/// by name instead of doing nothing. Adding the next one (§18's queue, a metrics
/// sink) is a field here rather than another argument at every call site.
#[derive(Default, Clone)]
pub struct ActionServices {
    /// The server's one isolate (§7.3), which an `only_if` and most actions'
    /// configuration need.
    pub evaluator: Option<Arc<dyn JsEvaluator>>,
    /// The mail transport (§18.2), which `send_email` needs.
    pub mailer: Option<Arc<dyn Mailer>>,
}

/// The live trigger set, the actions it can run, and the services they run with
/// — everything one dispatch needs, held in one place a server installs once.
///
/// Cheap to share (`Arc` it): the trigger set is behind an `RwLock` so a save can
/// swap it in ([`set_triggers`](TriggerDispatcher::set_triggers)) while events
/// keep firing.
pub struct TriggerDispatcher {
    registry: RwLock<Arc<ActionRegistry>>,
    triggers: RwLock<Arc<Triggers>>,
    services: ActionServices,
    observer: RwLock<Option<Arc<dyn crate::TriggerObserver>>>,
    /// The engine a **workflow** body is run by (§10.3), installed by whoever
    /// built one. `None` in a process that has none, where a workflow trigger
    /// refuses by name rather than doing nothing.
    ///
    /// An `RwLock` for the reason the registry is one: the engine is started
    /// after the dispatcher exists (the dispatcher is what it is installed
    /// *on*), so it arrives on a handle that is already shared.
    engine: RwLock<Option<Arc<dyn crate::WorkflowEngine>>>,
}

impl TriggerDispatcher {
    /// A dispatcher over `registry`, with no triggers yet — a server calls
    /// [`reload`](TriggerDispatcher::reload) once the catalog is up.
    pub fn new(registry: Arc<ActionRegistry>) -> TriggerDispatcher {
        TriggerDispatcher {
            registry: RwLock::new(registry),
            triggers: RwLock::new(Arc::new(Triggers::empty())),
            services: ActionServices::default(),
            observer: RwLock::new(None),
            engine: RwLock::new(None),
        }
    }

    /// Supply the JavaScript engine (the server's one isolate, §7.3), which an
    /// `only_if` and most actions' configuration need.
    pub fn with_evaluator(mut self, evaluator: Arc<dyn JsEvaluator>) -> TriggerDispatcher {
        self.services.evaluator = Some(evaluator);
        self
    }

    /// Supply the mail transport (§18.2), which `send_email` needs.
    ///
    /// Optional in the same way the evaluator is: a process that installs no
    /// mailer runs every other action normally, and a trigger that sends mail
    /// fails there with a message saying so rather than at a half-written
    /// `Option` somewhere further in.
    pub fn with_mailer(mut self, mailer: Arc<dyn Mailer>) -> TriggerDispatcher {
        self.services.mailer = Some(mailer);
        self
    }

    /// The actions this dispatcher can run.
    ///
    /// A clone of the handle rather than a borrow, because the set is
    /// **swappable**: installing a module adds actions to a live server, and a
    /// borrow would hold the lock across whatever the caller does next.
    pub fn registry(&self) -> Arc<ActionRegistry> {
        match self.registry.read() {
            Ok(guard) => Arc::clone(&guard),
            // A poisoned lock means a panic while the set was being swapped.
            // The set itself is an `Arc` that was either replaced or not, so
            // the honest answer is the one that is in there.
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// The services an action run is given: the JavaScript engine and the mail
    /// transport this process assembled, or neither.
    ///
    /// Exposed for the **workflow engine** (§10.3), which builds an
    /// [`ActionContext`] of its own for every step it runs and must build it
    /// with what a trigger's own action would have got. An engine that assembled
    /// its own evaluator would be a second isolate, and one that assembled none
    /// would make `run_js_code` work as a trigger and fail as a step.
    pub fn services(&self) -> &ActionServices {
        &self.services
    }

    /// Replace the actions this dispatcher can run — what installing,
    /// configuring or removing a **module** does (TODO decision 5).
    ///
    /// The caller rebuilds the whole set (the built-ins plus every loaded
    /// module's actions) and swaps it in one act, rather than adding to the
    /// live one: a registry half way through a rebuild is a registry a firing
    /// trigger could read. Reloading the trigger set afterwards is the caller's
    /// job and is what turns a trigger that was broken ("unknown action
    /// `mqtt_publish`") back into a working one.
    pub fn set_registry(&self, registry: Arc<ActionRegistry>) -> Result<()> {
        let mut guard = self
            .registry
            .write()
            .map_err(|_| Error::msg("action registry lock poisoned"))?;
        *guard = registry;
        Ok(())
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

    /// Install the observer notified whenever the live set is reloaded — the
    /// mount registry, so an application's exposed-trigger endpoints follow a
    /// change with no restart.
    ///
    /// Set once, at boot, by whoever holds both handles; a later call replaces
    /// it. Takes `&self` because the dispatcher is already shared behind an
    /// `Arc` by the time a server has anything to install.
    pub fn set_observer(&self, observer: Arc<dyn crate::TriggerObserver>) {
        if let Ok(mut guard) = self.observer.write() {
            *guard = Some(observer);
        }
    }

    /// Install the engine that runs a **workflow** body (§10.3) — what
    /// `WorkflowEngineTask` does when `serve` starts it.
    ///
    /// Takes `&self`, as [`set_observer`](TriggerDispatcher::set_observer) does
    /// and for the same reason: the engine is built after the dispatcher is
    /// already shared behind an `Arc`, because the engine holds the dispatcher.
    pub fn set_workflow_engine(&self, engine: Arc<dyn crate::WorkflowEngine>) {
        if let Ok(mut guard) = self.engine.write() {
            *guard = Some(engine);
        }
    }

    /// The workflow engine, if this process has one.
    pub fn workflow_engine(&self) -> Option<Arc<dyn crate::WorkflowEngine>> {
        match self.engine.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Load and validate every stored trigger into the live set — at boot, and
    /// after any change to `_sc_triggers`.
    ///
    /// Every writer of a trigger calls this afterwards, which is what makes it
    /// the place the [`TriggerObserver`](crate::TriggerObserver) is notified: an
    /// admin's save, a restore, and an agent's `save_trigger` all arrive here,
    /// and none of them has to remember to re-project anything.
    pub async fn reload(&self, catalog: &Catalog) -> Result<()> {
        let triggers = Triggers::load(catalog, &self.registry()).await?;
        self.set_triggers(triggers)?;
        self.notify(catalog);
        Ok(())
    }

    /// Tell the observer the set moved, reporting — never returning — a failed
    /// reaction.
    ///
    /// The trigger *is* saved by the time this runs, so an error here is a
    /// mounted application that keeps its previous projection, not a save that
    /// did not happen; failing the caller would report the opposite of what
    /// occurred. The app names the missing or changed trigger when it is next
    /// built or mounted.
    fn notify(&self, catalog: &Catalog) {
        let observer = match self.observer.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        if let Some(observer) = observer
            && let Err(e) = observer.triggers_changed(catalog)
        {
            eprintln!(
                "saltcorn: the trigger set changed, but an application could not be \
                 re-projected and keeps its previous mount: {}",
                sc_error::format_chain(&e)
            );
        }
    }

    /// Fire every trigger listening for `event`, in name order, and report what
    /// each of them did.
    ///
    /// Never fails as a whole: a trigger that cannot run contributes an `Err`
    /// outcome and the rest still run. The caller decides what to do with those —
    /// the emit path logs them, a test asserts on them.
    pub async fn dispatch(&self, catalog: &Catalog, event: &Event) -> Vec<TriggerRun> {
        self.dispatch_in(catalog, event, None).await
    }

    /// [`dispatch`](TriggerDispatcher::dispatch) **inside a transaction**: every
    /// row these triggers write joins `tx` (§10.3, decision 6).
    ///
    /// This is how a workflow step's atomicity survives a cascade. The step's
    /// write raised this event, and a trigger listening to it writes an audit
    /// row, a total, a notification — work that is part of the step, and must
    /// commit with the step or vanish with it. `None` is the ordinary case and
    /// is exactly [`dispatch`](TriggerDispatcher::dispatch).
    pub async fn dispatch_in(
        &self,
        catalog: &Catalog,
        event: &Event,
        tx: Option<&SharedTx>,
    ) -> Vec<TriggerRun> {
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
            let outcome = fire_trigger_in(self, catalog, &trigger, event, tx).await;
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
        self.fire_in(catalog, event, None).await;
    }

    /// [`fire`](TriggerDispatcher::fire) **inside a transaction** — see
    /// [`dispatch_in`](TriggerDispatcher::dispatch_in).
    pub async fn fire_in(&self, catalog: &Catalog, event: &Event, tx: Option<&SharedTx>) {
        if event.kind != EventKind::Error {
            self.report(catalog, event, tx).await;
            return;
        }
        if HANDLING_ERROR.try_with(|()| ()).is_ok() {
            // Already inside an error event's dispatch on this task: the error
            // this one describes came from handling the last one.
            return;
        }
        HANDLING_ERROR
            .scope((), async { self.report(catalog, event, tx).await })
            .await;
    }

    /// [`dispatch`](TriggerDispatcher::dispatch), logging what failed.
    async fn report(&self, catalog: &Catalog, event: &Event, tx: Option<&SharedTx>) {
        for run in self.dispatch_in(catalog, event, tx).await {
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
        self.run_trigger_in(catalog, name, payload, caller, None)
            .await
    }

    /// [`run_trigger`](TriggerDispatcher::run_trigger) **inside a transaction**:
    /// what the trigger writes joins `tx`.
    ///
    /// A code body inside a workflow step calls `trigger("name")`, and what that
    /// trigger writes is as much the step's work as what the body wrote itself —
    /// see [`dispatch_in`](TriggerDispatcher::dispatch_in).
    pub async fn run_trigger_in(
        &self,
        catalog: &Catalog,
        name: &str,
        payload: Json,
        caller: Option<&CallerContext>,
        tx: Option<&SharedTx>,
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
        let result = fire_trigger_in(self, catalog, trigger, &event, tx).await?;
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
        // The write has happened, so a trigger that failed is reported rather
        // than returned — the same rule every non-request event follows.
        //
        // In the transaction the write was made in, when it was made in one: a
        // step's cascade is part of the step, and lands where the step lands
        // (§10.3, decision 6).
        self.fire_in(catalog, &table_event(&write), write.tx.as_ref())
            .await;
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
///
/// Takes **the dispatcher** rather than its registry and services separately,
/// because an action may need the dispatcher itself: `run_js_code`'s `trigger(…)`
/// runs another trigger, and what it must run is the one the admin configured,
/// through the same path every other event takes. Passing `&self` down is what
/// gives it that without an `Arc` cycle — the borrow is this stack frame, and the
/// recursion it allows (a body running a trigger whose action is a body) is
/// bounded where every other cascade is, by [`Event::firing`].
pub async fn fire_trigger(
    dispatcher: &TriggerDispatcher,
    catalog: &Catalog,
    trigger: &Trigger,
    event: &Event,
) -> Result<Option<Json>> {
    fire_trigger_in(dispatcher, catalog, trigger, event, None).await
}

/// [`fire_trigger`] **inside a transaction**: the action's row writes join `tx`
/// (§10.3, decision 6), and so do the writes of whatever they cascade into.
///
/// A **workflow** body is the one thing `tx` does not reach into: starting a run
/// writes a run row that has to survive whatever happens to the transaction that
/// started it, and its steps open transactions of their own, one per step. So a
/// workflow fired from inside a step is its own unit of durability, which is what
/// a run has always been.
pub async fn fire_trigger_in(
    dispatcher: &TriggerDispatcher,
    catalog: &Catalog,
    trigger: &Trigger,
    event: &Event,
    tx: Option<&SharedTx>,
) -> Result<Option<Json>> {
    let services = &dispatcher.services;
    // The cascade bound, checked before anything else runs: past it, the trigger
    // does not fire and the error names the whole chain (§10.2).
    let chain = event.firing(&trigger.name)?;
    if !only_if_selects(catalog, services.evaluator.as_ref(), trigger, event).await? {
        return Ok(None);
    }
    // Which engine runs it is the body's question, and the only one dispatch
    // asks about it (§10.3): an action runs here and now, a workflow starts a
    // run that outlives this call.
    let (action, configuration) = match &trigger.body {
        crate::TriggerBody::Action {
            action,
            configuration,
        } => (action, configuration),
        crate::TriggerBody::Workflow => {
            let engine = dispatcher.workflow_engine().ok_or_else(|| {
                Error::config(format!(
                    "trigger `{}` is a workflow, but no workflow engine is \
                     available in this context",
                    trigger.name
                ))
            })?;
            let started = engine.start(catalog, trigger, event, chain).await?;
            return Ok(Some(started.to_json()));
        }
    };
    let registry = dispatcher.registry();
    let action = registry.require(action.trim())?;
    let mut ctx = ActionContext::new(catalog, event, configuration, &trigger.name)
        .with_chain(chain)
        .with_triggers(dispatcher);
    if let Some(tx) = tx {
        ctx = ctx.with_transaction(tx.clone());
    }
    if let Some(evaluator) = &services.evaluator {
        ctx = ctx.with_evaluator(evaluator);
    }
    if let Some(mailer) = &services.mailer {
        ctx = ctx.with_mailer(mailer);
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
            Ambient::User | Ambient::Payload | Ambient::Context => value_from_json(json),
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
            tx: None,
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
            tx: None,
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
