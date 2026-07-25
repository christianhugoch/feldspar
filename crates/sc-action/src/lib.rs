//! Actions and triggers (layer 6; technical design §10.1–§10.2).
//!
//! This crate owns the **event → action** half of the trigger system: what can
//! happen ([`Event`]), what can be done about it ([`Action`]), what one run of an
//! action can see ([`ActionContext`]), and which actions exist
//! ([`ActionRegistry`]). The trigger that binds the two — its storage in
//! `_sc_triggers`, its validation and the dispatch that fires it — lands in the
//! phases after this one; the event model is deliberately independent of it, so
//! the row layer can emit events without knowing whether anything listens.
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
//! and needs the formula language, and nothing below it may depend on it — the
//! row layer (`sc-api`, layer 8) reaches it through a seam the catalog holds
//! (Phase 4), never the other way round.

mod action;
mod event;
mod registry;

pub use action::{Action, ActionContext};
pub use event::{EVENT_KINDS, Event, EventKind, MAX_DEPTH, ROLE_PUBLIC};
pub use registry::ActionRegistry;
