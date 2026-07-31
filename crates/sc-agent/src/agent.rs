//! The [`Agent`]: a provider, a system prompt and a set of enabled traits
//! (design §11.2).
//!
//! Pure data, like a `Trigger` — the row it is stored as
//! lives in [`store`](crate::store), the validation it must pass in
//! [`validate`](crate::validate), and the running in [`machine`](crate::machine)
//! and [`driver`](crate::driver). An agent knows nothing about how it is run,
//! which is what lets the same record be chatted with, fired from a trigger and
//! (later) resumed by the workflow engine without reshaping anything.
//!
//! **An agent is its own record, not a trigger's configuration** (§11's third
//! separation). `run_agent` is the one registered action that runs one, so an
//! agent becomes a trigger body through machinery that already exists and stays
//! editable as an agent rather than as a blob inside a trigger's
//! `configuration`.

use sc_types::Attrs;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

/// Identifies an agent: the UUID primary key of its `_sc_agents` row (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId(pub Uuid);

impl AgentId {
    /// Mint an id for a new agent.
    pub fn new() -> AgentId {
        AgentId(Uuid::new_v4())
    }
}

impl Default for AgentId {
    fn default() -> Self {
        AgentId::new()
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The attribute holding the sampling temperature — sparse (§9's rule): absent
/// means the provider's own default, which is not the same as any number we
/// could store in its place.
pub const ATTR_TEMPERATURE: &str = "temperature";
/// The attribute capping the tokens one *answer* may generate.
pub const ATTR_MAX_TOKENS: &str = "max_tokens";
/// The attribute capping how many times one run may go round the loop.
pub const ATTR_MAX_STEPS: &str = "max_steps";

/// How many model calls one run makes before the loop stops it (§11.2).
///
/// An agent that will not converge — one that answers every tool result with
/// another tool call — has to be stopped by a number, because nothing else about
/// the conversation distinguishes it from one making progress. Twenty is enough
/// for the coding traits' read-edit-build-read cycle and small enough that a
/// runaway costs cents rather than a bill.
pub const DEFAULT_MAX_STEPS: u32 = 20;

/// One enabled trait: which trait, and how it is configured.
///
/// A **pair in a list**, not a map entry, because a trait may be enabled more
/// than once (§11.2): "query the `books` table" and "query the `orders` table"
/// are one trait with two configurations. Each instance derives its tool names
/// from its own configuration, and a collision between two of them is refused on
/// save by [`validate_agent`](crate::validate_agent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnabledTrait {
    /// The registered name of the trait ([`AgentTrait::name`](crate::AgentTrait::name)).
    ///
    /// Serialised as `trait` — the field is `trait_` only because `trait` is a
    /// Rust keyword, and the stored shape should not have to know that.
    #[serde(rename = "trait")]
    pub trait_: String,
    /// This instance's configuration, keyed by the trait's
    /// [`config_spec`](crate::AgentTrait::config_spec) field names.
    #[serde(default)]
    pub config: Attrs,
}

impl EnabledTrait {
    /// Enable `trait_` with no configuration set.
    pub fn new(trait_: impl Into<String>) -> EnabledTrait {
        EnabledTrait {
            trait_: trait_.into(),
            config: Attrs::new(),
        }
    }

    /// Set one configuration value, returning `self` for chaining.
    pub fn config(mut self, key: impl Into<String>, value: impl Into<Json>) -> EnabledTrait {
        self.config.insert(key.into(), value.into());
        self
    }

    /// Set the whole configuration, returning `self` for chaining.
    pub fn configuration(mut self, config: Attrs) -> EnabledTrait {
        self.config = config;
        self
    }
}

/// A configured LLM loop: a provider and model, a system prompt, and a set of
/// enabled traits.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    /// Stable identity: the UUID of its `_sc_agents` row.
    pub id: AgentId,
    /// The unique, human-facing name — what a trigger's `run_agent` and the chat
    /// panel address it by, so renaming one breaks those references deliberately
    /// rather than silently.
    pub name: String,
    /// Human-readable description (§9 requires one on every metadata row; the
    /// empty string means "none given").
    pub description: String,
    /// The name of an `_sc_llm_providers` record.
    pub provider: String,
    /// The model to call, overriding the provider's default. `None` means the
    /// provider's own — which is the common case, and why this is not required.
    pub model: Option<String>,
    /// What the agent is told it is, before anything the conversation adds.
    pub system_prompt: String,
    /// The enabled traits, in the order the admin added them — which is the
    /// order their tools are offered to the model in.
    pub traits: Vec<EnabledTrait>,
    /// The role floor for chatting with this agent. `None` is **admin-only**,
    /// the same safe reading a trigger's takes: an agent nobody has thought about
    /// the access of is not public.
    pub min_role: Option<u8>,
    /// Sparse per-agent values (§9): [`ATTR_TEMPERATURE`], [`ATTR_MAX_TOKENS`],
    /// [`ATTR_MAX_STEPS`].
    pub attributes: Attrs,
}

impl Agent {
    /// A **new** agent with a fresh id: a name and a provider, and nothing else
    /// set.
    pub fn new(name: impl Into<String>, provider: impl Into<String>) -> Agent {
        Agent::with_id(AgentId::new(), name, provider)
    }

    /// Reconstruct an existing agent, which already has an id — what
    /// [`load_agent`](crate::load_agent) and an update path use.
    pub fn with_id(id: AgentId, name: impl Into<String>, provider: impl Into<String>) -> Agent {
        Agent {
            id,
            name: name.into(),
            description: String::new(),
            provider: provider.into(),
            model: None,
            system_prompt: String::new(),
            traits: Vec::new(),
            min_role: None,
            attributes: Attrs::new(),
        }
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> Agent {
        self.description = description.into();
        self
    }

    /// Override the provider's default model.
    pub fn model(mut self, model: impl Into<String>) -> Agent {
        self.model = Some(model.into());
        self
    }

    /// Set the system prompt.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Agent {
        self.system_prompt = prompt.into();
        self
    }

    /// Enable one trait, returning `self` for chaining.
    pub fn with_trait(mut self, enabled: EnabledTrait) -> Agent {
        self.traits.push(enabled);
        self
    }

    /// Set the role floor for chatting with this agent.
    pub fn min_role(mut self, role: u8) -> Agent {
        self.min_role = Some(role);
        self
    }

    /// Set one attribute, returning `self` for chaining.
    pub fn attribute(mut self, key: impl Into<String>, value: impl Into<Json>) -> Agent {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// The sampling temperature, if one is set.
    ///
    /// Absent means "do not send one", so the provider applies its own default —
    /// deliberately not resolved to a number here, because guessing which number
    /// a vendor would have chosen is how a request stops matching what the
    /// vendor's own documentation says it does.
    pub fn temperature(&self) -> Option<f64> {
        self.attributes.get(ATTR_TEMPERATURE).and_then(Json::as_f64)
    }

    /// The cap on tokens generated per answer, if one is set.
    pub fn max_tokens(&self) -> Option<u32> {
        self.attributes
            .get(ATTR_MAX_TOKENS)
            .and_then(Json::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    }

    /// How many times one run may go round the loop, defaulting to
    /// [`DEFAULT_MAX_STEPS`].
    ///
    /// Zero is not a value the loop can act on — an agent that may not call the
    /// model cannot answer — so a stored zero reads as the default rather than
    /// producing a run that ends before it starts.
    pub fn max_steps(&self) -> u32 {
        self.attributes
            .get(ATTR_MAX_STEPS)
            .and_then(Json::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_STEPS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_agent_is_built_from_a_provider_and_its_traits() {
        let a = Agent::new("librarian", "anthropic")
            .description("answers questions about the books table")
            .model("claude-opus-5")
            .system_prompt("You answer questions about books.")
            .with_trait(EnabledTrait::new("query_table").config("table", "books"))
            .with_trait(EnabledTrait::new("query_table").config("table", "orders"))
            .min_role(1);
        assert_eq!(a.name, "librarian");
        assert_eq!(a.model.as_deref(), Some("claude-opus-5"));
        // The same trait, twice, with different configurations (§11.2).
        assert_eq!(a.traits.len(), 2);
        assert_eq!(a.traits[0].trait_, a.traits[1].trait_);
        assert_eq!(a.traits[0].config["table"], json!("books"));
        assert_eq!(a.traits[1].config["table"], json!("orders"));
        assert_eq!(a.min_role, Some(1));
    }

    #[test]
    fn the_sparse_attributes_read_as_their_defaults() {
        let a = Agent::new("a", "p");
        assert!(a.attributes.is_empty());
        // Absent temperature and max_tokens mean "do not send one".
        assert_eq!(a.temperature(), None);
        assert_eq!(a.max_tokens(), None);
        assert_eq!(a.max_steps(), DEFAULT_MAX_STEPS);

        let a = a
            .attribute(ATTR_TEMPERATURE, 0.2)
            .attribute(ATTR_MAX_TOKENS, 8192)
            .attribute(ATTR_MAX_STEPS, 4);
        assert_eq!(a.temperature(), Some(0.2));
        assert_eq!(a.max_tokens(), Some(8192));
        assert_eq!(a.max_steps(), 4);
    }

    #[test]
    fn a_zero_step_budget_reads_as_the_default() {
        // A run that may not call the model cannot answer, so zero is a value
        // the loop cannot act on rather than a very short leash.
        let a = Agent::new("a", "p").attribute(ATTR_MAX_STEPS, 0);
        assert_eq!(a.max_steps(), DEFAULT_MAX_STEPS);
    }

    #[test]
    fn an_enabled_trait_serialises_its_name_as_trait() {
        // The stored shape should not have to know that `trait` is a Rust
        // keyword: `_sc_agents.traits` holds `{"trait": ..., "config": ...}`.
        let enabled = EnabledTrait::new("run_trigger").config("trigger", "reindex");
        let text = serde_json::to_value(&enabled).unwrap();
        assert_eq!(
            text,
            json!({"trait": "run_trigger", "config": {"trigger": "reindex"}})
        );
        let back: EnabledTrait = serde_json::from_value(text).unwrap();
        assert_eq!(back, enabled);
    }

    #[test]
    fn a_fresh_agent_has_its_own_id() {
        let a = Agent::new("a", "p");
        let b = Agent::new("b", "p");
        assert_ne!(a.id, b.id);
        assert_eq!(Agent::with_id(a.id, "a", "p").id, a.id);
    }
}
