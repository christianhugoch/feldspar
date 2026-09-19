//! A module's **stream providers**, as `sc-stream`'s [`StreamProviderHost`]
//! (TODO "Streams" §12, task 9.2).
//!
//! A module exports `streamproviders` beside its `actions`, `table_providers`
//! and `modelproviders`:
//!
//! ```js
//! streamproviders: {
//!   poll_feed: {
//!     description: "An RSS feed, polled",
//!     config_fields: [{ name: "url", type: "String", required: true },
//!                     { name: "interval_s", type: "Integer", default: 60 }],
//!     element_type: ({ configuration }) => ({ kind: "json", keys: [ … ] }),
//!     poll: async ({ configuration, cursor }) => ({ elements: [ … ], cursor: "…" }),
//!   },
//! }
//! ```
//!
//! [`ModuleStreamProviders`] is [`ModuleModelProviders`](crate::
//! ModuleModelProviders)' sibling in every respect that is about the seam:
//! built whole on every module change, routed to the worker the named module is
//! loaded on, and checking every name again on this side because the set can be
//! rebuilt between a stream being saved and a poll of it running.
//!
//! # Poll, not push — and what that costs
//!
//! A module call is request/response on a Deno worker; there is no channel from
//! a worker back into the host, and building one is a milestone of its own. So
//! what this host hands the registry is **not** a provider of its own making:
//! it is [`sc_stream::PollingProvider`] wrapping the two calls below, and the
//! interval loop, the opaque cursor, the decoding of every element against the
//! declared type and the behaviour of a poll that throws all live in
//! `sc-stream`, written once. That is the one structural difference from the
//! model seam, and it is why this file is mostly two calls and a translation.
//!
//! # The element type is a call, not a value
//!
//! GOALS makes the element type a function of the configuration, and here that
//! function is JavaScript. It therefore does not cross in the manifest — only
//! the name, the description and the settings do — and is asked for per
//! configuration through [`ModuleHost::stream_element_type`]. `PollingProvider`
//! is what caches the answer for the synchronous half of the provider trait.

use std::sync::Arc;

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_stream::{
    ElementType, PollAnswer, PollHost, PollingProvider, StreamProvider, StreamProviderHost,
    StreamProviderKind,
};
use sc_types::Attrs;
use serde_json::Value as Json;

use crate::host::ModuleHost;
use crate::modules::ModuleSet;
use crate::spec::config_fields_to_form_fields;

/// The stream providers this server's JavaScript modules supply.
pub struct ModuleStreamProviders {
    host: Arc<ModuleHost>,
    providers: Vec<StreamProviderKind>,
}

impl ModuleStreamProviders {
    /// Every stream provider of every loaded module in `set`, over `host`.
    ///
    /// A module that would not load supplies none, and a provider the host
    /// script already refused — one with no `poll`, or no element type —
    /// never reaches here: it is an issue on the module's card, where the
    /// admin who installed it is looking.
    #[must_use]
    pub fn new(host: &Arc<ModuleHost>, set: &ModuleSet) -> ModuleStreamProviders {
        let mut providers = Vec::new();
        for loaded in set.modules() {
            let Some(manifest) = &loaded.manifest else {
                continue;
            };
            for provider in &manifest.stream_providers {
                let owner = format!("the stream provider `{}`", provider.name);
                let (config_spec, _) =
                    config_fields_to_form_fields(&provider.config_fields, &owner);
                providers.push(
                    StreamProviderKind::new(
                        &provider.name,
                        provider.label.as_deref().unwrap_or(&provider.name),
                        &provider.description,
                    )
                    .module(loaded.module.name.clone())
                    .config(config_spec),
                );
            }
        }
        ModuleStreamProviders {
            host: Arc::clone(host),
            providers,
        }
    }

    /// An empty set — a server with no modules, and the starting point for a
    /// test that wants the seam without a worker.
    #[must_use]
    pub fn empty(host: &Arc<ModuleHost>) -> ModuleStreamProviders {
        ModuleStreamProviders {
            host: Arc::clone(host),
            providers: Vec::new(),
        }
    }

    /// Every provider, in the order the module set has them.
    #[must_use]
    pub fn providers(&self) -> &[StreamProviderKind] {
        &self.providers
    }

    /// Refuse a provider this set does not have, before a worker is reached.
    ///
    /// Re-checked even though the registry resolved the provider through this
    /// same list: a module can be uninstalled between a stream being saved and
    /// its next poll, and the honest answer then is a sentence naming what went
    /// away — which the supervisor turns into `failed` with that sentence
    /// rather than a panic.
    fn require(&self, kind: &StreamProviderKind) -> Result<()> {
        let module = kind.module.as_deref().ok_or_else(|| {
            Error::config(format!(
                "the stream provider `{}` does not say which module supplies it",
                kind.name
            ))
        })?;
        if self
            .providers
            .iter()
            .any(|p| p.name == kind.name && p.module.as_deref() == Some(module))
        {
            return Ok(());
        }
        Err(Error::not_found(format!(
            "no installed module supplies the stream provider `{}` of `{module}`; it may have \
             been uninstalled, or failed to load",
            kind.name
        )))
    }
}

impl StreamProviderHost for ModuleStreamProviders {
    fn providers(&self) -> Vec<StreamProviderKind> {
        self.providers.clone()
    }

    fn provider(&self, kind: &StreamProviderKind) -> Result<Arc<dyn StreamProvider>> {
        self.require(kind)?;
        Ok(Arc::new(PollingProvider::new(
            kind.clone(),
            Arc::new(ModulePollHost {
                host: Arc::clone(&self.host),
            }),
        )))
    }
}

/// The two calls a [`PollingProvider`] makes, routed to the worker the module
/// is loaded on.
///
/// Separate from [`ModuleStreamProviders`] because a provider handed out
/// **outlives the set it came from**: a subscription holds its provider for as
/// long as it runs, and a module change rebuilds the set underneath it. What a
/// running poll needs is the host, not the declaration list — and a module that
/// went away answers "the module … is not loaded in this host", which is the
/// sentence the supervisor shows.
struct ModulePollHost {
    host: Arc<ModuleHost>,
}

impl ModulePollHost {
    /// The module and provider names a call is routed by.
    fn route(kind: &StreamProviderKind) -> Result<(&str, &str)> {
        let module = kind.module.as_deref().ok_or_else(|| {
            Error::config(format!(
                "the stream provider `{}` does not say which module supplies it",
                kind.name
            ))
        })?;
        Ok((module, kind.name.as_str()))
    }
}

#[async_trait]
impl PollHost for ModulePollHost {
    async fn element_type(
        &self,
        provider: &StreamProviderKind,
        config: &Attrs,
    ) -> Result<ElementType> {
        let (module, name) = ModulePollHost::route(provider)?;
        let answer = self
            .host
            .stream_element_type(module, name, &Json::Object(config.clone()))
            .await?;
        read_element_type(module, name, answer)
    }

    async fn poll(
        &self,
        provider: &StreamProviderKind,
        config: &Attrs,
        cursor: &Json,
    ) -> Result<PollAnswer> {
        let (module, name) = ModulePollHost::route(provider)?;
        let answer = self
            .host
            .stream_poll(module, name, &Json::Object(config.clone()), cursor)
            .await?;
        PollAnswer::read(answer).map_err(|e| {
            Error::config(format!(
                "the stream provider `{name}` of `{module}` answered a poll this server could \
                 not read: {e}"
            ))
        })
    }
}

/// What a module answered an element-type question with, as an [`ElementType`].
///
/// Named rather than defaulted, for the reason [`read_fit`](crate::
/// model_providers::read_fit) is: a declaration this server cannot read is a
/// stream that would deliver nothing and say nothing, and the moment the
/// question was asked — a save, or a start — is the only place an admin can act
/// on it.
pub fn read_element_type(module: &str, provider: &str, answer: Json) -> Result<ElementType> {
    serde_json::from_value(answer).map_err(|e| {
        Error::config(format!(
            "the stream provider `{provider}` of `{module}` declared an element type this server \
             could not read: {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_provider_nothing_supplies_is_refused_before_a_worker_is_reached() {
        let host = Arc::new(ModuleHost::new("modules"));
        let providers = ModuleStreamProviders::empty(&host);
        let kind = StreamProviderKind::new("poll_feed", "Polled feed", "").module("@feldspar/rss");
        let err = providers.require(&kind).expect_err("nothing supplies it");
        assert!(err.to_string().contains("poll_feed"), "{err}");
        assert!(StreamProviderHost::providers(&providers).is_empty());
        // And a declaration with no module behind it is refused rather than
        // routed to a worker with an empty module name.
        let orphan = StreamProviderKind::new("poll_feed", "Polled feed", "");
        let err = providers.require(&orphan).expect_err("no module");
        assert!(err.to_string().contains("which module"), "{err}");
    }

    #[test]
    fn an_element_type_is_read_as_declared_or_named_when_it_cannot_be() {
        let ty = read_element_type(
            "@feldspar/rss",
            "poll_feed",
            json!({ "kind": "json", "keys": [{ "name": "title", "type": "string" }] }),
        )
        .expect("a json element type");
        assert_eq!(ty.kind(), "json");
        assert_eq!(ty.keys().len(), 1);

        let err = read_element_type("@feldspar/rss", "poll_feed", json!({ "kind": "tensor" }))
            .expect_err("no such kind")
            .to_string();
        assert!(
            err.contains("poll_feed") && err.contains("could not read"),
            "{err}"
        );
    }
}
