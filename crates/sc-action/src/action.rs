//! The [`Action`] trait and the [`ActionContext`] it runs in (design §10.1).
//!
//! An action is **one elementary step**: it is handed an event, its own
//! configuration, and a catalog, and it returns a JSON result. Control flow —
//! branching, looping, retrying — is deliberately not here; it belongs to the
//! workflow engine (§10.3), which is what keeps the built-in action set as small
//! as GOALS asks for.
//!
//! The shape is chosen so that a workflow step can reuse it unchanged: the
//! `context` an action reads and writes *is* the run context a workflow
//! accumulates, and the returned value is what a step contributes to it. Today
//! there is exactly one caller (a trigger firing once), so the context starts
//! empty and is dropped afterwards; nothing about the trait has to change when
//! it starts persisting between steps.

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_expr::JsEvaluator;
use sc_types::{Attrs, FormField};
use serde_json::Value as Json;

use crate::event::Event;

/// One elementary step: configurable, run against an event.
///
/// Object-safe and dynamically dispatched, because which action runs is decided
/// at runtime from stored configuration and may — once `sc-code` lands — be
/// implemented in a guest language behind a single Rust shim (§2.1).
#[async_trait::async_trait]
pub trait Action: Send + Sync {
    /// The name the action is registered and stored under (`insert_row`, …).
    /// Stable: it is what a saved trigger references.
    fn name(&self) -> &str;

    /// One line for the admin UI's action picker.
    fn description(&self) -> &str;

    /// The configuration this action takes, as form fields — the same
    /// "settings as data" move that lets the admin UI render a form for a file
    /// store backend or a framework it knows nothing about (§6.2). It is also
    /// what a trigger's configuration is validated against on save (Phase 2).
    fn config_spec(&self) -> Vec<FormField>;

    /// Run against `ctx`, returning the action's result.
    ///
    /// The result is the response body of a directly-run trigger, and will be a
    /// workflow step's contribution to the run context. An action with nothing to
    /// report returns [`Json::Null`].
    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json>;
}

/// Everything one action run has access to.
///
/// Borrowed rather than owned (hence the lifetime): a run is a single `await` on
/// the caller's stack, and copying a catalog handle and a configuration map per
/// firing would be waste for no gain in clarity.
pub struct ActionContext<'a> {
    /// The data layer. An action that writes rows goes through the row layer
    /// (`sc-api`, above this crate) rather than the driver, so its writes are
    /// themselves events — which is what makes a cascade possible, and bounded.
    pub catalog: &'a Catalog,
    /// What happened.
    pub event: &'a Event,
    /// This trigger's configuration for this action, keyed by the
    /// [`config_spec`](Action::config_spec) field names.
    pub config: &'a Attrs,
    /// The name of the trigger being run, for error messages: an action failure
    /// the admin cannot attribute to a trigger is a failure they cannot fix.
    pub trigger: &'a str,
    /// The chain of trigger names that led here, *including* this one — what
    /// [`Event::firing`](crate::Event::firing) returned. Any event this action's
    /// writes raise carries it, so the next level down knows how deep it is.
    pub chain: Vec<String>,
    /// The JavaScript engine, when the caller has one. Absent in contexts that
    /// have no engine (client generation, unit tests); an action that needs it
    /// asks through [`evaluator`](ActionContext::evaluator) and gets a named
    /// configuration error rather than silently doing nothing.
    evaluator: Option<&'a Arc<dyn JsEvaluator>>,
    /// The run context: a JSON object the action may read and write. The seam the
    /// workflow engine's durable context grows into.
    pub context: Attrs,
}

impl<'a> ActionContext<'a> {
    /// A context for running `trigger`'s action against `event` with `config`.
    ///
    /// The chain starts as just this trigger — the caller replaces it with
    /// [`Event::firing`](crate::Event::firing)'s result when the event descends
    /// from another trigger.
    pub fn new(
        catalog: &'a Catalog,
        event: &'a Event,
        config: &'a Attrs,
        trigger: &'a str,
    ) -> ActionContext<'a> {
        ActionContext {
            catalog,
            event,
            config,
            trigger,
            chain: vec![trigger.to_owned()],
            evaluator: None,
            context: Attrs::new(),
        }
    }

    /// Supply the JavaScript engine (the server's one isolate, §7.3).
    pub fn with_evaluator(mut self, evaluator: &'a Arc<dyn JsEvaluator>) -> ActionContext<'a> {
        self.evaluator = Some(evaluator);
        self
    }

    /// Supply the chain this run descends from (`Event::firing`'s result).
    pub fn with_chain(mut self, chain: Vec<String>) -> ActionContext<'a> {
        self.chain = chain;
        self
    }

    /// The JavaScript engine, or a configuration error naming the trigger.
    ///
    /// Fails rather than skipping: an action whose configuration is formulas and
    /// whose engine is missing has nothing correct to do, and quietly doing
    /// nothing is the silent failure principle 5 forbids.
    pub fn evaluator(&self) -> Result<&Arc<dyn JsEvaluator>> {
        self.evaluator.ok_or_else(|| {
            Error::config(format!(
                "trigger `{}` needs to evaluate a formula but no JavaScript engine \
                 is available in this context",
                self.trigger
            ))
        })
    }

    /// A configuration setting, if present.
    pub fn setting(&self, key: &str) -> Option<&Json> {
        self.config.get(key)
    }

    /// A required string setting, or an error naming the trigger and the setting.
    ///
    /// Save-time validation against the [`config_spec`](Action::config_spec)
    /// should mean this always succeeds; it exists because "should" is not a
    /// guarantee once a row can be edited by a restore, and because the
    /// alternative at the call site is an `unwrap`.
    ///
    /// Returns an owned `String` rather than a borrow of the configuration: an
    /// action reads its settings *and* writes [`context`](ActionContext::context),
    /// and a borrow of `&self` here would make the second of those a borrow-check
    /// error in every action that does both. One small clone per setting is the
    /// cheaper half of that trade.
    pub fn require_str(&self, key: &str) -> Result<String> {
        match self.config.get(key) {
            Some(Json::String(s)) if !s.is_empty() => Ok(s.clone()),
            _ => Err(Error::config(format!(
                "trigger `{}` is missing the `{key}` setting",
                self.trigger
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal action, standing in for the Phase 3 built-ins: it reads one
    /// setting, writes to the run context, and returns a value derived from the
    /// event — the whole `ActionContext` contract in one implementation.
    struct Echo;

    #[async_trait::async_trait]
    impl Action for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Return a configured message and the event's channel"
        }
        fn config_spec(&self) -> Vec<FormField> {
            vec![FormField::new("message", sc_types::BasicType::Text).required()]
        }
        async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json> {
            let message = ctx.require_str("message")?;
            ctx.context.insert("ran".into(), json!(true));
            Ok(json!({
                "message": message,
                "channel": ctx.event.channel,
                "trigger": ctx.trigger,
            }))
        }
    }

    // Everything an `ActionContext` does needs a `Catalog`, which needs a
    // database, so the context's own contract is pinned in
    // `tests/action_context.rs` against a real one. What is asserted here is what
    // is genuinely catalog-free: that the trait is object-safe and declares its
    // configuration as data.
    #[test]
    fn the_action_trait_is_object_safe_and_declares_its_config() {
        let action: Arc<dyn Action> = Arc::new(Echo);
        assert_eq!(action.name(), "echo");
        assert!(!action.description().is_empty());
        let spec = action.config_spec();
        assert_eq!(spec.len(), 1);
        assert_eq!(spec[0].name(), "message");
        assert!(spec[0].required);
    }
}
