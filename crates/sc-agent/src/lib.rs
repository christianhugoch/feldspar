//! `sc-agent` — agents, traits and the loop (design §11.2).
//!
//! Layer 7. This crate knows **agents, traits and the loop, and nothing about
//! which vendor is on the other end** — that is [`sc_llm`]'s, below it. It also
//! knows nothing about *which* traits exist: it defines what a trait is, and the
//! built-in set lives in `sc-core-traits` at layer 9, above the row layer, so a
//! trait that writes a row goes through `sc-api` and its write is coerced,
//! validated and observed like any other.
//!
//! ## What is here
//!
//! - The **record** ([`agent`], [`store`], [`validate`]): [`Agent`], `_sc_agents`,
//!   and one validation function run on save *and* on load. An agent that fails
//!   is dropped from the live set ([`Agents`]) with its reason kept, and stays
//!   stored, listed and editable — editing it is the repair.
//! - The **extension point** ([`agent_trait`], [`registry`]): [`AgentTrait`] and
//!   [`AgentRegistry`], the twin of `sc-action`'s `ActionRegistry`.
//! - The **loop** ([`machine`]): [`AgentLoop`], a steppable, serialisable state
//!   machine that decides and does no IO.
//! - The **driver** ([`driver`]): [`Runner`], the one thing that does IO for it —
//!   streams the provider, dispatches tools as the run's caller, and writes
//!   `_sc_runs` after every step.
//! - The **run** ([`run`], [`run_store`]): [`Run`] and `_sc_runs`, in the shape
//!   §10.3's workflow engine will also use.
//! - **Delegation** ([`delegate`]): [`Delegator`], the capability a run offers a
//!   trait that names *another agent* — a child run, under the same authority,
//!   with a context of its own and a bound on how deep the chain may go.
//! - A **scripted provider** ([`testing`], behind the `testing` feature), because
//!   no test in this tree may need an API key or spend a token.
//!
//! ## The two decisions worth knowing before reading
//!
//! **The loop is a machine, not an `async fn`.** The state is a value, so it is
//! what `_sc_runs` stores and a resumed run is a loaded one. See [`machine`].
//!
//! **A tool runs as the caller, not as the server.** [`RunCaller`] travels with
//! the run and there is no default: a chat turn carries the person and a
//! triggered run carries the trigger's authority, so §7.3's ownership and RLS
//! apply inside an agent exactly as they do outside it.

pub mod agent;
pub mod agent_trait;
pub mod delegate;
pub mod driver;
pub mod machine;
pub mod registry;
pub mod run;
pub mod run_store;
pub mod store;
pub mod validate;

#[cfg(feature = "testing")]
#[cfg_attr(docsrs, doc(cfg(feature = "testing")))]
pub mod testing;

pub use agent::{
    ATTR_MAX_STEPS, ATTR_MAX_TOKENS, ATTR_TEMPERATURE, Agent, AgentId, DEFAULT_MAX_STEPS,
    EnabledTrait,
};
pub use agent_trait::{AgentTrait, RunCaller, TraitCheck, TraitContext, Turn};
pub use delegate::{
    ATTR_DELEGATED_BY, ATTR_PARENT_RUN, DEFAULT_MAX_DEPTH, DelegateRequest, Delegated, Delegator,
};
pub use driver::{ProviderConnector, RunObserver, Runner, StoredProviders, connect};
pub use machine::{AgentLoop, Conclusion, Step, ToolOutcome};
pub use registry::AgentRegistry;
pub use run::{Run, RunId, RunKind, RunState};
pub use run_store::{
    RUNS_TABLE, bootstrap_runs, delete_run, list_runs, load_run, require_run, run_insert,
    run_update, save_run,
};
pub use store::{
    AGENTS_TABLE, bootstrap_agents, delete_agent, list_agents, load_agent, load_agent_by_name,
    require_agent, save_agent,
};
pub use validate::{AgentIssue, Agents, validate_agent};
