//! [`LlmProviderDef`]: the stored *definition* of an LLM provider, and the
//! backend registry that turns one into a connected
//! [`LlmProvider`](crate::LlmProvider) — design §11.1.
//!
//! This follows `sc-files`' file-store backends exactly, and the parallel is
//! worth stating because it is what makes the admin UI generic: a **definition**
//! is inert data (a name, which backend, that backend's settings), a backend
//! declares its settings as [`FormField`]s, and the form that renders a file
//! store's settings renders these without knowing what an API key is.
//!
//! Two kinds of wrong, kept apart, for the same reason §14.1 keeps them apart:
//!
//! - **Structurally wrong** — a missing API key, a `base_url` that is a number,
//!   an unknown backend. The admin's typo, knowable without a network, so
//!   [`validate_provider_config`] catches it and the save is refused while they
//!   are still looking at the form.
//! - **Currently not working** — a well-formed definition with a revoked key, an
//!   endpoint that is down, a model that was retired. Not a typo, often not the
//!   admin's fault, and it can become true after a successful save. Only a real
//!   request finds it, which is what *Test connection* is for.
//!
//! Only the first blocks a save.

use std::sync::Arc;

use sc_error::{Error, Repr, Result};
use sc_types::{Attrs, BasicType, FormField, validate_attrs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::anthropic::Anthropic;
use crate::openai::OpenAiResponses;
use crate::provider::LlmProvider;

/// The backend speaking **OpenAI's Responses API** — including every
/// OpenAI-compatible endpoint, which is what [`CFG_BASE_URL`] is for.
pub const OPENAI_RESPONSES_BACKEND: &str = "openai_responses";

/// The backend speaking **Anthropic's messages API**.
pub const ANTHROPIC_BACKEND: &str = "anthropic";

/// The `base_url` setting: which endpoint to call. Blank means the backend's own
/// default.
pub const CFG_BASE_URL: &str = "base_url";

/// The `api_key` setting — declared [`secret`](FormField::secret), so it is
/// redacted wherever the record is serialised and preserved when a save submits
/// the sentinel back (§11.1).
pub const CFG_API_KEY: &str = "api_key";

/// The `model` setting: the model this provider calls when an agent does not
/// override it.
pub const CFG_MODEL: &str = "model";

/// Identifies a stored provider definition. A UUID, per §9's rule that every
/// system metadata table has a UUID primary key.
///
/// Distinct from the provider's *name*, which is what an agent references and
/// what everything else resolves through — so a provider can be renamed without
/// its row changing identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LlmProviderDefId(pub Uuid);

impl LlmProviderDefId {
    /// A fresh random id, for a definition that has never been saved.
    pub fn new() -> LlmProviderDefId {
        LlmProviderDefId(Uuid::new_v4())
    }
}

impl Default for LlmProviderDefId {
    fn default() -> LlmProviderDefId {
        LlmProviderDefId::new()
    }
}

/// The stored definition of one LLM provider (§11.1).
///
/// The fields mirror the `_sc_llm_providers` columns, following §9's
/// column-vs-attributes rule: every provider has an id, name, description,
/// backend and backend config, so each gets a column, and anything sparse goes
/// in [`attributes`](LlmProviderDef::attributes).
///
/// **No `min_role`.** A file store has one because an application's users browse
/// it; a provider is reached only through an agent, and it is the *agent* that
/// carries who may chat with it (§11.2). Putting a floor here as well would be a
/// second authority over the same question with no way to reconcile them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmProviderDef {
    /// The row's identity.
    pub id: LlmProviderDefId,
    /// The provider's name — what an agent references, unique across providers.
    pub name: String,
    /// A human description; empty when none was given.
    pub description: String,
    /// Which backend serves it: [`OPENAI_RESPONSES_BACKEND`] or
    /// [`ANTHROPIC_BACKEND`].
    pub backend: String,
    /// The backend's settings, validated against that backend's declared
    /// [`config_spec`](provider_config_spec) on save and on load.
    pub config: Attrs,
    /// Sparse per-provider values (§9).
    pub attributes: Attrs,
}

impl LlmProviderDef {
    /// A definition named `name` served by `backend`, with no settings and no
    /// description — the base to add settings to.
    pub fn new(name: impl Into<String>, backend: impl Into<String>) -> LlmProviderDef {
        LlmProviderDef {
            id: LlmProviderDefId::new(),
            name: name.into(),
            description: String::new(),
            backend: backend.into(),
            config: Attrs::new(),
            attributes: Attrs::new(),
        }
    }

    /// An [`anthropic`](ANTHROPIC_BACKEND) provider with a key and a default
    /// model.
    pub fn anthropic(
        name: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> LlmProviderDef {
        LlmProviderDef::new(name, ANTHROPIC_BACKEND)
            .with(CFG_API_KEY, api_key.into())
            .with(CFG_MODEL, model.into())
    }

    /// An [`openai_responses`](OPENAI_RESPONSES_BACKEND) provider with a key and
    /// a default model, on OpenAI's own endpoint.
    pub fn openai(
        name: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> LlmProviderDef {
        LlmProviderDef::new(name, OPENAI_RESPONSES_BACKEND)
            .with(CFG_API_KEY, api_key.into())
            .with(CFG_MODEL, model.into())
    }

    /// Set a backend setting, returning `self` for chaining.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.config.insert(key.into(), value.into());
        self
    }

    /// Set the description, returning `self` for chaining.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the id, returning `self` for chaining — for rebuilding a definition
    /// that already has a row.
    pub fn id(mut self, id: LlmProviderDefId) -> Self {
        self.id = id;
        self
    }

    /// The value of a backend setting as text, if it is set and is a string.
    pub fn setting(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(serde_json::Value::as_str)
    }

    /// The model this provider calls when nothing overrides it.
    pub fn default_model(&self) -> Option<&str> {
        self.setting(CFG_MODEL)
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }
}

/// The settings the [`openai_responses`](OPENAI_RESPONSES_BACKEND) backend
/// needs.
///
/// `base_url` is optional and defaulted rather than required, which is the whole
/// design: an admin connecting OpenAI itself leaves it alone, and an admin
/// connecting a gateway, a local server or an alternative vendor types a URL.
/// Neither is a code change.
pub fn openai_config_spec() -> Vec<FormField> {
    vec![
        FormField::new(CFG_API_KEY, BasicType::Text)
            .label("API key")
            .required()
            .secret(),
        FormField::new(CFG_MODEL, BasicType::Text)
            .label("Default model")
            .required()
            .default_value("gpt-5.1"),
        FormField::new(CFG_BASE_URL, BasicType::Text)
            .label("Base URL (blank for OpenAI)")
            .default_value(crate::openai::DEFAULT_BASE_URL),
    ]
}

/// The settings the [`anthropic`](ANTHROPIC_BACKEND) backend needs — the same
/// three, since the difference between the vendors is not in what an admin has
/// to type.
pub fn anthropic_config_spec() -> Vec<FormField> {
    vec![
        FormField::new(CFG_API_KEY, BasicType::Text)
            .label("API key")
            .required()
            .secret(),
        // A default rather than a fixed list: model names change faster than
        // releases do, and an admin who has to type one is better served than
        // one whose model is missing from a select nobody has updated.
        FormField::new(CFG_MODEL, BasicType::Text)
            .label("Default model")
            .required()
            .default_value("claude-sonnet-5"),
        FormField::new(CFG_BASE_URL, BasicType::Text)
            .label("Base URL (blank for Anthropic)")
            .default_value(crate::anthropic::DEFAULT_BASE_URL),
    ]
}

/// The names of every registered backend — what the admin UI lists so an admin
/// can pick one and be shown its [`provider_config_spec`].
///
/// The single place that enumerates them: a new backend is added here, to
/// [`provider_config_spec`] and to [`connect_provider`].
pub fn registered_backends() -> Vec<String> {
    vec![
        ANTHROPIC_BACKEND.to_owned(),
        OPENAI_RESPONSES_BACKEND.to_owned(),
    ]
}

/// The settings the backend registered under `name` declares.
///
/// An unknown name is a configuration error rather than a backend with no
/// settings: a provider nothing implements cannot be connected, and saying so
/// beats accepting it and failing later with no explanation.
pub fn provider_config_spec(name: &str) -> Result<Vec<FormField>> {
    match name {
        OPENAI_RESPONSES_BACKEND => Ok(openai_config_spec()),
        ANTHROPIC_BACKEND => Ok(anthropic_config_spec()),
        other => Err(unknown_backend(other)),
    }
}

/// The error for a backend nothing implements, naming what there is.
fn unknown_backend(name: &str) -> Error {
    Error::config(format!(
        "unknown LLM provider backend `{name}`; the registered backends are {}",
        registered_backends().join(", ")
    ))
}

/// Check a definition's settings against its backend's declared spec — the check
/// a save goes through, and the one a load repeats.
pub fn validate_provider_config(def: &LlmProviderDef) -> Result<()> {
    let spec = provider_config_spec(&def.backend)?;
    validate_attrs(&spec, &def.config).map_err(|e| {
        // Rebuilt rather than wrapped, for the reason `validate_file_store_config`
        // documents: `Invalid` renders its own "invalid:" prefix, so formatting
        // the whole error into a new one would say it twice, and a `Context`
        // would show only the context and hide the setting — the part the admin
        // needs.
        if let Repr::Invalid(msg) = e.repr() {
            Error::invalid(format!(
                "LLM provider `{}` (backend `{}`): {msg}",
                def.name, def.backend
            ))
        } else {
            e
        }
    })
}

/// Turn a stored definition into a callable provider — the one place a
/// definition becomes an instance.
///
/// `model` overrides the definition's [`default_model`](LlmProviderDef::default_model),
/// which is how an agent names a different model against the same key (§11.2).
/// Passing `None` uses the definition's own.
///
/// Building a client sends nothing, so an error here means the *configuration*
/// cannot produce a client — a missing key, an unparseable URL — never that the
/// provider is down. Reachability is a request, and the admin form's *Test
/// connection* is the deliberate way to make one.
pub fn connect_provider(def: &LlmProviderDef, model: Option<&str>) -> Result<Arc<dyn LlmProvider>> {
    validate_provider_config(def)?;

    let api_key = def.setting(CFG_API_KEY).unwrap_or_default();
    let base_url = def.setting(CFG_BASE_URL).unwrap_or_default();
    let model = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) => model,
        None => def.default_model().ok_or_else(|| {
            // Unreachable while `model` is required and validation ran above,
            // but the alternative is an `unwrap` (principle 5), and a provider
            // whose model is blank is a real error worth naming.
            Error::invalid(format!(
                "LLM provider `{}` has no `{CFG_MODEL}` setting and no model was given",
                def.name
            ))
        })?,
    };

    match def.backend.as_str() {
        OPENAI_RESPONSES_BACKEND => Ok(Arc::new(OpenAiResponses::new(base_url, api_key, model)?)),
        ANTHROPIC_BACKEND => Ok(Arc::new(Anthropic::new(base_url, api_key, model)?)),
        // Unreachable while `validate_provider_config` runs first; kept so
        // adding a backend to the registry without adding it here is a clear
        // error rather than a fallthrough.
        other => Err(Error::config(format!(
            "LLM provider backend `{other}` declares settings but cannot be connected"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_types::SECRET_SENTINEL;

    #[test]
    fn every_backend_declares_its_key_as_a_secret() {
        // The point of `secret` being on the declaration: this is checked once,
        // for every backend, rather than trusted per screen.
        for backend in registered_backends() {
            let spec = provider_config_spec(&backend).unwrap();
            let key = spec.iter().find(|f| f.name() == CFG_API_KEY).unwrap();
            assert!(key.secret, "{backend}'s API key must be declared secret");
            assert!(key.required, "{backend} cannot be called without a key");
        }
    }

    #[test]
    fn the_registry_resolves_a_name_to_the_same_spec() {
        assert_eq!(
            provider_config_spec(OPENAI_RESPONSES_BACKEND).unwrap(),
            openai_config_spec()
        );
        assert_eq!(
            provider_config_spec(ANTHROPIC_BACKEND).unwrap(),
            anthropic_config_spec()
        );
        let err = provider_config_spec("bedrock").unwrap_err().to_string();
        assert!(err.contains("bedrock"), "{err}");
        assert!(
            err.contains(ANTHROPIC_BACKEND),
            "should list what there is: {err}"
        );
    }

    #[test]
    fn a_base_url_is_a_field_value_not_a_code_change() {
        // The whole of "OpenAI-compatible": an alternative endpoint is typed in.
        let spec = openai_config_spec();
        let base = spec.iter().find(|f| f.name() == CFG_BASE_URL).unwrap();
        assert!(!base.required, "an admin using OpenAI itself types nothing");
        assert_eq!(
            base.default,
            Some(serde_json::json!(crate::openai::DEFAULT_BASE_URL))
        );
    }

    #[test]
    fn a_missing_key_is_refused_on_save_naming_the_provider() {
        let def = LlmProviderDef::new("main", ANTHROPIC_BACKEND).with(CFG_MODEL, "claude-opus-4");
        let err = validate_provider_config(&def).unwrap_err().to_string();
        assert!(err.contains("main"), "{err}");
        assert!(err.contains(CFG_API_KEY), "{err}");
    }

    #[test]
    fn an_unknown_setting_is_refused_rather_than_ignored() {
        let def = LlmProviderDef::anthropic("main", "sk-x", "claude-opus-4").with("temp", 0.5);
        let err = validate_provider_config(&def).unwrap_err().to_string();
        assert!(err.contains("temp"), "{err}");
    }

    #[test]
    fn connecting_resolves_the_model_from_the_override_then_the_definition() {
        let def = LlmProviderDef::anthropic("main", "sk-x", "claude-sonnet-4-5");
        assert_eq!(
            connect_provider(&def, None).unwrap().model(),
            "claude-sonnet-4-5"
        );
        assert_eq!(
            connect_provider(&def, Some("claude-opus-4-1"))
                .unwrap()
                .model(),
            "claude-opus-4-1"
        );
        // A blank override is not an override — it is an agent that left the
        // field empty, which means "the provider's default".
        assert_eq!(
            connect_provider(&def, Some("  ")).unwrap().model(),
            "claude-sonnet-4-5"
        );
    }

    #[test]
    fn a_structurally_broken_definition_will_not_connect() {
        let def = LlmProviderDef::new("main", ANTHROPIC_BACKEND);
        assert!(connect_provider(&def, None).is_err());
        let def = LlmProviderDef::new("main", "bedrock");
        assert!(connect_provider(&def, None).is_err());
    }

    #[test]
    fn both_backends_connect_against_an_arbitrary_base_url() {
        // Building a client sends nothing, so this asserts exactly what
        // `connect_provider` promises: a well-formed definition produces a
        // callable provider without reaching the network.
        let openai = LlmProviderDef::openai("gateway", "sk-x", "gpt-5.1")
            .with(CFG_BASE_URL, "http://127.0.0.1:9/v1");
        assert_eq!(connect_provider(&openai, None).unwrap().model(), "gpt-5.1");

        let anthropic = LlmProviderDef::anthropic("proxy", "sk-y", "claude-opus-4-1")
            .with(CFG_BASE_URL, "http://127.0.0.1:9");
        assert_eq!(
            connect_provider(&anthropic, None).unwrap().model(),
            "claude-opus-4-1"
        );
    }

    #[test]
    fn the_sentinel_is_never_what_gets_stored() {
        // A definition whose key is the mask is a definition that cannot work,
        // and `merge_secrets` is what stops one being built. Asserted here
        // because this crate is where the two meet.
        let spec = provider_config_spec(ANTHROPIC_BACKEND).unwrap();
        let stored = LlmProviderDef::anthropic("main", "sk-real", "claude-opus-4").config;
        let redacted = sc_types::redact_attrs(&spec, &stored);
        assert_eq!(
            redacted.get(CFG_API_KEY),
            Some(&serde_json::json!(SECRET_SENTINEL))
        );
        let merged = sc_types::merge_secrets(&spec, &stored, &redacted);
        assert_eq!(merged.get(CFG_API_KEY), Some(&serde_json::json!("sk-real")));
    }
}
