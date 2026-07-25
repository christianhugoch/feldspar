//! The [`Trigger`]: what binds an [`Event`](crate::Event) to a configured action
//! (design §10.2).
//!
//! Pure data, like an [`Application`](sc_app::Application) — the row it is stored
//! as lives in [`store`](crate::store), the validation it must pass in
//! [`validate`](crate::validate), and the firing in the dispatcher. A trigger
//! knows nothing about how it is run, which is what lets the same record grow a
//! workflow body later (§10.3) without reshaping anything around it.
//!
//! One trigger = one event + one action. Not a list of actions: a sequence of
//! steps is a *workflow*, and conflating the two is what made v1's execution path
//! hard to reason about. Two triggers on the same event is how you get two
//! things done today.

use sc_types::Attrs;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::event::EventKind;

/// Identifies a trigger: the UUID primary key of its `_sc_triggers` row (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriggerId(pub Uuid);

impl TriggerId {
    /// Mint an id for a new trigger.
    pub fn new() -> TriggerId {
        TriggerId(Uuid::new_v4())
    }
}

impl Default for TriggerId {
    fn default() -> Self {
        TriggerId::new()
    }
}

impl std::fmt::Display for TriggerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The attribute holding whether a trigger is enabled — sparse (§9's rule),
/// because the answer is `true` for almost every trigger almost always.
pub const ATTR_ENABLED: &str = "enabled";

/// One event bound to one configured action.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    /// Stable identity: the UUID of its `_sc_triggers` row.
    pub id: TriggerId,
    /// The unique, human-facing name. It is what an application's exposed subset,
    /// an API path (`POST /actions/{name}`) and a "run" button all reference, so
    /// it is the key, and renaming one breaks those references deliberately
    /// rather than silently.
    pub name: String,
    /// Human-readable description (§9 requires one on every metadata row; the
    /// empty string means "none given").
    pub description: String,
    /// The event that fires it.
    pub when: EventKind,
    /// What the event is about: the table name for a table event, `None`
    /// otherwise. Validation enforces that correspondence.
    pub channel: Option<String>,
    /// The **"only if"** formula: a predicate over the affected row (and `user`,
    /// `row`, `old`) that must be true for the action to run. `None` means
    /// always. Table events only — nothing else has a row to test.
    pub only_if: Option<String>,
    /// The registered name of the action to run.
    pub action: String,
    /// That action's configuration, keyed by its `config_spec` field names.
    pub configuration: Attrs,
    /// The role floor for running this trigger through an API. `None` is
    /// **admin-only** (the safe reading: a trigger nobody has thought about the
    /// access of is not public), which Phase 7's endpoint projection applies.
    pub min_role: Option<u8>,
    /// Sparse per-trigger values (§9): [`ATTR_ENABLED`], and Phase 8's periodic
    /// timing and `last_run_at`.
    pub attributes: Attrs,
}

impl Trigger {
    /// A **new** trigger with a fresh id: an event, an action, and nothing else
    /// set.
    pub fn new(name: impl Into<String>, when: EventKind, action: impl Into<String>) -> Trigger {
        Trigger::with_id(TriggerId::new(), name, when, action)
    }

    /// Reconstruct an existing trigger, which already has an id — what
    /// [`load`](crate::load_trigger) and an update path use.
    pub fn with_id(
        id: TriggerId,
        name: impl Into<String>,
        when: EventKind,
        action: impl Into<String>,
    ) -> Trigger {
        Trigger {
            id,
            name: name.into(),
            description: String::new(),
            when,
            channel: None,
            only_if: None,
            action: action.into(),
            configuration: Attrs::new(),
            min_role: None,
            attributes: Attrs::new(),
        }
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> Trigger {
        self.description = description.into();
        self
    }

    /// Set the channel — the table, for a table event.
    pub fn on(mut self, channel: impl Into<String>) -> Trigger {
        self.channel = Some(channel.into());
        self
    }

    /// Set the "only if" formula.
    pub fn only_if(mut self, formula: impl Into<String>) -> Trigger {
        self.only_if = Some(formula.into());
        self
    }

    /// Set one configuration value.
    pub fn config(mut self, key: impl Into<String>, value: impl Into<Json>) -> Trigger {
        self.configuration.insert(key.into(), value.into());
        self
    }

    /// Set the whole configuration.
    pub fn configuration(mut self, configuration: Attrs) -> Trigger {
        self.configuration = configuration;
        self
    }

    /// Set the role floor for running this trigger through an API.
    pub fn min_role(mut self, role: u8) -> Trigger {
        self.min_role = Some(role);
        self
    }

    /// Whether the trigger fires. Absent means **enabled**: a trigger is created
    /// to run, and the attribute exists so an admin can switch one off without
    /// deleting it (and losing its configuration).
    pub fn is_enabled(&self) -> bool {
        self.attributes
            .get(ATTR_ENABLED)
            .and_then(Json::as_bool)
            .unwrap_or(true)
    }

    /// Enable or disable the trigger. Enabling **removes** the key rather than
    /// storing `true`, so an untouched trigger carries no residue (§9's sparse
    /// rule, as `TableMeta`'s accessors do).
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            self.attributes.remove(ATTR_ENABLED);
        } else {
            self.attributes
                .insert(ATTR_ENABLED.into(), Json::Bool(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_trigger_is_built_from_an_event_and_an_action() {
        let t = Trigger::new("audit", EventKind::Insert, "insert_row")
            .description("write an audit row")
            .on("books")
            .only_if("pages > 100")
            .config("table", "audit")
            .min_role(1);
        assert_eq!(t.name, "audit");
        assert_eq!(t.when, EventKind::Insert);
        assert_eq!(t.channel.as_deref(), Some("books"));
        assert_eq!(t.only_if.as_deref(), Some("pages > 100"));
        assert_eq!(t.configuration["table"], json!("audit"));
        assert_eq!(t.min_role, Some(1));
        // Nothing was set for these, and they read as their defaults.
        assert!(t.attributes.is_empty());
        assert!(t.is_enabled());
    }

    #[test]
    fn enabling_leaves_no_residue_but_disabling_is_recorded() {
        let mut t = Trigger::new("t", EventKind::None, "fetch");
        t.set_enabled(false);
        assert!(!t.is_enabled());
        assert_eq!(t.attributes[ATTR_ENABLED], json!(false));
        t.set_enabled(true);
        assert!(t.is_enabled());
        assert!(!t.attributes.contains_key(ATTR_ENABLED));
    }

    #[test]
    fn a_fresh_trigger_has_its_own_id() {
        let a = Trigger::new("a", EventKind::None, "fetch");
        let b = Trigger::new("b", EventKind::None, "fetch");
        assert_ne!(a.id, b.id);
        // Reconstructing keeps the id — the update path depends on it.
        let same = Trigger::with_id(a.id, "a", EventKind::None, "fetch");
        assert_eq!(same.id, a.id);
    }
}
