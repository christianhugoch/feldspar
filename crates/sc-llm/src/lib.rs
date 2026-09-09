//! `sc-llm` — the LLM provider seam (design §11.1).
//!
//! Layer 6. This crate knows **providers, messages, tools and streaming, and
//! nothing about Saltcorn**: no agents, no traits, no loop, no rows. Its twin
//! above it, `sc-agent`, knows agents, traits and the loop and nothing about
//! which vendor is on the other end. That separation is the reason there are two
//! crates rather than one, and it is what lets either be replaced without the
//! other noticing.
//!
//! ## What is here
//!
//! - The **vocabulary** ([`message`]): [`LlmRequest`], [`LlmMessage`],
//!   [`ToolSpec`], [`ToolCall`], [`LlmDelta`], [`Usage`]. Ours, not a provider
//!   crate's — nothing `rig-core` exposes appears in a public signature here.
//! - The **seam** ([`provider`]): the object-safe [`LlmProvider`] trait and
//!   [`LlmStream`], with [`LlmStream::collect`] as the single place the
//!   streaming and non-streaming shapes meet.
//! - Two **adapters** ([`openai`], [`anthropic`]) over `rig-core`, sharing one
//!   translation module ([`rig_bridge`]).
//! - The **call log** ([`logging`]): every configured provider is wrapped in a
//!   [`LoggedProvider`], so a model call reports itself — a summary line with
//!   its token cost at `info`, and the whole request and response at `trace`
//!   (§16).
//! - The **configured entity** ([`def`], [`storage`]): [`LlmProviderDef`],
//!   the backend registry, `_fd_llm_providers`, and [`connect_provider`] — a
//!   provider is a named record an admin fills in, exactly as a file store is.
//!
//! ## What is deliberately not here
//!
//! rig's `Agent`, its tool registry, its RAG and vector stores. The loop is
//! `sc-agent`'s because it persists to `_fd_runs`, runs every tool as the
//! chatting user, and streams to a browser — none of which a provider crate can
//! know about. Concretely, rig's `CompletionModel` is not object-safe (associated
//! types, `impl Future`, `Clone`), so a `Box<dyn>` chosen from stored
//! configuration needs a seam whatever crate is underneath.
//!
//! Encryption at rest for API keys is out of scope for this milestone, by
//! decision: a key sits in the primary database like every other configuration
//! value. What *is* guaranteed is that it does not leave through the API —
//! see [`sc_types::redact_attrs`] and [`FormField::secret`](sc_types::FormField::secret).

pub mod anthropic;
pub mod def;
pub mod logging;
pub mod message;
pub mod openai;
pub mod provider;
mod rig_bridge;
pub mod storage;

pub use def::{
    ANTHROPIC_BACKEND, CFG_API_KEY, CFG_BASE_URL, CFG_MODEL, LlmProviderDef, LlmProviderDefId,
    OPENAI_RESPONSES_BACKEND, anthropic_config_spec, connect_provider, openai_config_spec,
    provider_config_spec, registered_backends, validate_provider_config,
};
pub use logging::{LoggedProvider, request_summary, response_summary};
pub use message::{
    AssistantMessage, LlmDelta, LlmMessage, LlmRequest, StopReason, ToolCall, ToolSpec, Usage,
};
pub use provider::{DeltaStream, LlmProvider, LlmStream};
pub use storage::{
    LLM_PROVIDERS_TABLE, bootstrap_llm_providers, check_provider_saveable, delete_llm_provider,
    list_llm_providers, load_llm_provider, load_llm_provider_by_name, require_llm_provider,
    save_llm_provider,
};
