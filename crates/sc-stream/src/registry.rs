//! The stream-provider registry: which providers exist, and how a stored name
//! becomes one (TODO §3, task 1.4).
//!
//! `ActionRegistry`'s, `AgentRegistry`'s and `ModelRegistry`'s fourth sibling,
//! and deliberately so — a `BTreeMap` rather than a `match`, ordered by name,
//! duplicates refused — because the reasons are the same: the set is meant to
//! grow from outside this crate, and the admin UI must render a provider it has
//! never heard of from its [`config_spec`](StreamProvider::config_spec) alone.
//!
//! A duplicate is refused **naming both sources**, because "two providers are
//! called `mqtt`" is not actionable and "the module `@saltcorn/mqtt` supplies
//! `mqtt`, which is also a built-in" is.
//!
//! Like the other three it is **rebuilt rather than mutated** when the module
//! set changes: `StreamServices` in `sc-server` holds it behind an `Arc` and
//! swaps the whole thing, so a subscription that is running keeps the provider
//! it started with.

use std::collections::BTreeMap;
use std::sync::Arc;

use sc_error::{Error, Result};

use crate::provider::{StreamProvider, StreamProviderHost, StreamProviderKind};

/// The stream providers available to streams, by name.
///
/// Built once (at boot, on a module change, and in tests) and then read-only,
/// so it is cheap to share behind an `Arc`. Ordered by name so the picker and
/// every error message that lists the alternatives are stable rather than
/// hash-ordered.
#[derive(Clone, Default)]
pub struct StreamRegistry {
    providers: BTreeMap<String, Arc<dyn StreamProvider>>,
}

impl StreamRegistry {
    /// An empty registry.
    ///
    /// [`builtin_registry`](crate::builtin_registry) fills one with the
    /// built-in set, a module host adds its own, and a test starts from one
    /// with just the provider it is about.
    pub fn new() -> StreamRegistry {
        StreamRegistry::default()
    }

    /// Register `provider` under its own [`name`](StreamProvider::name).
    ///
    /// A duplicate name is refused rather than overwritten: two implementations
    /// claiming one name means every stored stream referencing it is ambiguous,
    /// and letting the last registration win would make which one *connects*
    /// depend on load order.
    pub fn register(&mut self, provider: Arc<dyn StreamProvider>) -> Result<()> {
        let name = provider.name().to_owned();
        if name.trim().is_empty() {
            return Err(Error::config("a stream provider must have a name"));
        }
        if let Some(existing) = self.providers.get(&name) {
            return Err(Error::config(format!(
                "two stream providers are registered under the name `{name}`: one from {} and \
                 one from {}",
                existing.kind().source(),
                provider.kind().source()
            )));
        }
        self.providers.insert(name, provider);
        Ok(())
    }

    /// Register every provider a module host supplies.
    ///
    /// All or nothing is **not** the contract, and that is on purpose: the
    /// first failure stops the registration, because a registry that silently
    /// held half of a module's providers would make "why is my feed missing" a
    /// question with no error to answer it. The caller reports it and the
    /// server carries on with the providers it did get, exactly as a module
    /// whose actions fail to load is reported and skipped.
    pub fn register_host(&mut self, host: Arc<dyn StreamProviderHost>) -> Result<()> {
        for kind in host.providers() {
            let provider = host.provider(&kind)?;
            if provider.name() != kind.name {
                return Err(Error::config(format!(
                    "the stream provider declared as `{}` supplies code calling itself `{}`",
                    kind.name,
                    provider.name()
                )));
            }
            self.register(provider)?;
        }
        Ok(())
    }

    /// The provider registered under `name`, if any.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn StreamProvider>> {
        self.providers.get(name)
    }

    /// The provider registered under `name`, or a configuration error naming it
    /// and the registered alternatives.
    ///
    /// This is the lookup every stream goes through — on save and on start —
    /// because a stream naming a provider nothing implements must be reported,
    /// never silently subscribed with something else.
    pub fn require(&self, name: &str) -> Result<&Arc<dyn StreamProvider>> {
        self.get(name).ok_or_else(|| {
            Error::config(format!(
                "unknown stream provider `{name}`; the registered providers are {}",
                if self.providers.is_empty() {
                    "(none)".to_owned()
                } else {
                    self.names().join(", ")
                }
            ))
        })
    }

    /// Every registered provider's name, in order.
    pub fn names(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    /// Every registered provider, in name order.
    pub fn all(&self) -> impl Iterator<Item = &Arc<dyn StreamProvider>> {
        self.providers.values()
    }

    /// What the picker lists: every provider's name, label, description, module
    /// and declared settings — in name order.
    pub fn kinds(&self) -> Vec<StreamProviderKind> {
        self.providers.values().map(|p| p.kind()).collect()
    }

    /// How many providers are registered.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether nothing is registered — which, with the `mqtt` feature off and
    /// no module installed, is a real state the screen has to explain rather
    /// than an empty list that reads like a bug (task 6.1).
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl std::fmt::Debug for StreamRegistry {
    /// Names only: a provider is a trait object with no meaningful `Debug`, and
    /// the names are the whole of what a reader wants from a registry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StreamRegistry")
            .field(&self.names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{ElementField, ElementType};
    use crate::provider::StreamSink;
    use crate::subscription::Subscription;
    use async_trait::async_trait;
    use sc_types::{Attrs, BasicType, FormField};

    /// A provider that observes nothing, to exercise ordering and the duplicate
    /// refusal.
    struct Named(&'static str, Option<&'static str>);

    #[async_trait]
    impl StreamProvider for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "test provider"
        }
        fn config_spec(&self) -> Vec<FormField> {
            Vec::new()
        }
        fn element_type(&self, _config: &Attrs) -> Result<ElementType> {
            Ok(ElementType::json([ElementField::new("n", BasicType::Int)]))
        }
        fn kind(&self) -> StreamProviderKind {
            let kind = StreamProviderKind::new(self.0, self.0, "test provider");
            match self.1 {
                Some(module) => kind.module(module),
                None => kind,
            }
        }
        async fn subscribe(
            &self,
            _stream: &str,
            _config: &Attrs,
            _sink: Arc<dyn StreamSink>,
        ) -> Result<Subscription> {
            Ok(Subscription::spawn(|mut stop| async move {
                stop.stopped().await;
            }))
        }
    }

    /// A host supplying the named providers, all from one module.
    struct Host(&'static str, Vec<&'static str>);

    impl StreamProviderHost for Host {
        fn providers(&self) -> Vec<StreamProviderKind> {
            self.1
                .iter()
                .map(|name| StreamProviderKind::new(*name, *name, "from a module").module(self.0))
                .collect()
        }
        fn provider(&self, kind: &StreamProviderKind) -> Result<Arc<dyn StreamProvider>> {
            let name = self
                .1
                .iter()
                .find(|n| **n == kind.name)
                .ok_or_else(|| Error::config(format!("no such provider `{}`", kind.name)))?;
            Ok(Arc::new(Named(name, Some(self.0))))
        }
    }

    fn registry(names: &[&'static str]) -> StreamRegistry {
        let mut reg = StreamRegistry::new();
        for name in names {
            reg.register(Arc::new(Named(name, None))).unwrap();
        }
        reg
    }

    #[test]
    fn a_provider_is_found_by_the_name_it_declares() {
        let reg = registry(&["mqtt", "amqp"]);
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
        assert_eq!(reg.require("mqtt").unwrap().name(), "mqtt");
        assert!(reg.get("amqp").is_some());
        // Ordered by name, not by registration order, so every listing is
        // stable.
        assert_eq!(reg.names(), vec!["amqp", "mqtt"]);
        assert_eq!(reg.all().count(), 2);
        assert_eq!(format!("{reg:?}"), "StreamRegistry([\"amqp\", \"mqtt\"])");
    }

    #[test]
    fn an_unknown_provider_names_itself_and_the_alternatives() {
        let reg = registry(&["mqtt"]);
        let msg = reg.require("kafka").err().unwrap().to_string();
        assert!(msg.contains("kafka") && msg.contains("mqtt"), "{msg}");
    }

    #[test]
    fn an_empty_registry_still_explains_itself() {
        let reg = StreamRegistry::new();
        assert!(reg.is_empty());
        let msg = reg.require("mqtt").err().unwrap().to_string();
        assert!(msg.contains("mqtt") && msg.contains("(none)"), "{msg}");
    }

    #[test]
    fn a_modules_providers_are_registered_as_providers_like_any_other() {
        let mut reg = registry(&["mqtt"]);
        reg.register_host(Arc::new(Host("@saltcorn/rss", vec!["rss", "atom"])))
            .unwrap();
        assert_eq!(reg.names(), vec!["atom", "mqtt", "rss"]);
        let kinds = reg.kinds();
        let rss = kinds.iter().find(|k| k.name == "rss").unwrap();
        assert_eq!(rss.module.as_deref(), Some("@saltcorn/rss"));
        let mqtt = kinds.iter().find(|k| k.name == "mqtt").unwrap();
        assert_eq!(mqtt.module, None);
        assert_eq!(mqtt.source(), "the built-in providers");
    }

    #[test]
    fn a_duplicate_name_is_refused_naming_both_sources() {
        let mut reg = registry(&["mqtt"]);
        let err = reg
            .register_host(Arc::new(Host("@saltcorn/mqtt", vec!["mqtt"])))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`mqtt`"), "{msg}");
        assert!(
            msg.contains("built-in") && msg.contains("@saltcorn/mqtt"),
            "{msg}"
        );
        // The first registration is still the one that answers.
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.require("mqtt").unwrap().kind().module, None);
    }

    #[test]
    fn a_nameless_provider_is_refused() {
        let err = StreamRegistry::new()
            .register(Arc::new(Named("  ", None)))
            .unwrap_err();
        assert!(err.to_string().contains("must have a name"), "{err}");
    }

    #[test]
    fn a_host_whose_code_disagrees_with_its_declaration_is_refused() {
        /// A host that declares one name and hands back another — the shape a
        /// routing bug in a module host takes, and one that would otherwise
        /// leave the picker offering a provider nothing can start.
        struct Liar;
        impl StreamProviderHost for Liar {
            fn providers(&self) -> Vec<StreamProviderKind> {
                vec![StreamProviderKind::new("rss", "RSS", "").module("@saltcorn/rss")]
            }
            fn provider(&self, _kind: &StreamProviderKind) -> Result<Arc<dyn StreamProvider>> {
                Ok(Arc::new(Named("atom", Some("@saltcorn/rss"))))
            }
        }
        let err = StreamRegistry::new()
            .register_host(Arc::new(Liar))
            .unwrap_err()
            .to_string();
        assert!(err.contains("`rss`") && err.contains("`atom`"), "{err}");
    }
}
