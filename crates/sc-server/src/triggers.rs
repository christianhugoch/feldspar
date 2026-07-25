//! Bringing the trigger system up at boot (§10.2, TODO Phase 4).
//!
//! Everything the row layer needs to raise an event lives below here — the
//! [seam](sc_catalog::TableEvents) in the catalog, the
//! [dispatcher](sc_action::TriggerDispatcher) in `sc-action`, the actions in
//! `sc-core-actions` — and none of them knows about the others. This is where
//! they are put together, once, by the process that is going to serve requests:
//! the built-in actions are registered, the stored triggers are loaded and
//! validated against them, and the dispatcher is installed into the catalog.
//!
//! Until that call, a write is simply unobserved. That is what makes a build
//! tool, a test, or an admin script safe to run against the same catalog without
//! firing anything.

use std::sync::Arc;

use sc_action::{TriggerDispatcher, bootstrap_triggers};
use sc_catalog::Catalog;
use sc_core_actions::builtin_actions;
use sc_error::{Context, Result};
use sc_expr::JsEvaluator;

/// Install the trigger dispatcher into `catalog` and return it.
///
/// Fails only on the things a server must not start without: the built-in action
/// set not assembling (a TLS stack that will not initialise, §10.1), the
/// `_sc_triggers` table not being creatable, or the database being unreadable. A
/// **trigger** that does not validate is not one of those — it is dropped from
/// the live set with its reason reported, exactly as a file store that will not
/// connect is, because the rest of the server works and the admin can fix it in
/// the UI.
pub async fn install_triggers(
    catalog: &Arc<Catalog>,
    evaluator: Arc<dyn JsEvaluator>,
) -> Result<Arc<TriggerDispatcher>> {
    bootstrap_triggers(catalog)
        .await
        .context("ensuring the triggers table exists")?;
    let registry = builtin_actions().context("registering the built-in actions")?;
    let dispatcher = Arc::new(TriggerDispatcher::new(Arc::new(registry)).with_evaluator(evaluator));
    dispatcher
        .reload(catalog)
        .await
        .context("loading the stored triggers")?;
    for issue in dispatcher.triggers()?.issues() {
        eprintln!(
            "saltcorn: trigger `{}` is stored but not usable: {}",
            issue.trigger, issue.problem
        );
    }
    catalog.set_table_events(Arc::clone(&dispatcher) as Arc<dyn sc_catalog::TableEvents>)?;
    Ok(dispatcher)
}

/// Fire the **`startup`** event: the server is up (§10.2).
///
/// Called once, after the catalog, the file stores, the applications *and* the
/// triggers are all up and before the listener is announced — so a startup
/// trigger's action finds a server that works, and anything it writes is visible
/// to the first request rather than racing it.
///
/// Reports rather than fails, like every other fire-and-forget event: a
/// misconfigured startup trigger must not be the reason a server refuses to
/// boot, which is the one moment nobody can fix it from the admin UI.
pub async fn fire_startup(catalog: &Arc<Catalog>, dispatcher: &Arc<TriggerDispatcher>) {
    dispatcher.fire(catalog, &sc_action::Event::startup()).await;
}
