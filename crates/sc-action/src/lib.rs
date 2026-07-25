//! Actions and triggers (layer 6; technical design §10.1–§10.2).
//!
//! This crate owns the **event → action** half of the trigger system: what can
//! happen ([`Event`]), what can be done about it ([`Action`]), what one run of an
//! action can see ([`ActionContext`]), which actions exist ([`ActionRegistry`]),
//! and the [`Trigger`] that binds an event to a configured action — its storage in
//! `_sc_triggers` ([`save_trigger`] and friends), its
//! [validation](validate_trigger), and the cached live set ([`Triggers`]) an event
//! is matched against. The dispatch that actually fires one lands in a later
//! phase; the event model is deliberately independent of the trigger, so the row
//! layer can emit events without knowing whether anything listens.
//!
//! It holds no actions itself — the core set is a crate of its own
//! (`sc-core-actions`, above the row layer, because three of them write rows) —
//! but it does hold the machinery *every* action's configuration goes through:
//! what a configured formula may name ([`action_shape`], [`EVENT_SCOPE`]), what an
//! event binds it to ([`EventBindings`]), and how a setting is parsed
//! ([`config_str`], [`formula_map`], [`check_formula`] and friends). So a plugin's
//! action needs this crate and nothing else.
//!
//! Three design commitments are expressed as types here rather than as prose:
//!
//! - **An action is one elementary step.** Control flow is the workflow engine's
//!   (§10.3), which is why [`Action::run`] returns a value and takes no branch:
//!   the small built-in set GOALS asks for is a consequence of that split.
//! - **Configuration is data.** An action declares its settings as
//!   [`FormField`](sc_types::FormField)s, so the admin UI renders a form for an
//!   action it has never heard of and save-time validation checks the values
//!   against the same declaration.
//! - **The caller travels as JSON.** An event carries a role and the user's
//!   fields, not a `sc_auth::User` — that object is exactly what the formula
//!   language binds `user` to, and it keeps this crate implementable from a guest
//!   language.
//!
//! Layering: this is the lowest crate that both holds a [`Catalog`](sc_catalog::Catalog)
//! and needs the formula language, and nothing below it may depend on it. Above
//! it, `sc-api` does — it implements the built-in actions that **write rows**
//! (`insert_row`/`update_rows`/`delete_rows`), which must go through the row layer
//! so a trigger's write is coerced, validated and observable exactly like an API
//! caller's; the rule is "an action lives where the things it needs are". Emitting
//! runs the other way regardless: the row layer raises an event through a seam the
//! catalog holds (Phase 4), never by calling in here.

mod action;
mod dispatch;
mod event;
mod registry;
mod scope;
mod store;
mod trigger;
mod triggers;
mod validate;

pub use action::{Action, ActionContext, ConfigCheck};
pub use dispatch::{TriggerDispatcher, TriggerRun, fire_trigger};
pub use event::{EVENT_KINDS, Event, EventKind, MAX_DEPTH, ROLE_PUBLIC};
pub use registry::ActionRegistry;
pub use scope::{
    EVENT_SCOPE, EventBindings, action_shape, check_formula, config_str, event_formula_value,
    formula_map, optional_formula, required_formula, typed_value,
};
pub use store::{
    TRIGGERS_TABLE, bootstrap_triggers, delete_trigger, list_triggers, load_trigger,
    load_trigger_by_name, save_trigger,
};
pub use trigger::{ATTR_ENABLED, Trigger, TriggerId};
pub use triggers::{TriggerIssue, Triggers};
pub use validate::{trigger_shape, validate_trigger};
