//! The model-provider registry: which providers exist, and how a stored name
//! becomes one (TODO §14, task 2.2).
//!
//! `ActionRegistry`'s and `AgentRegistry`'s third sibling, and deliberately so —
//! a `BTreeMap` rather than a `match`, ordered by name, duplicates refused —
//! because the reasons are the same: the set is meant to grow from outside this
//! crate, and the admin UI must render a provider it has never heard of from its
//! [`config_spec`](ModelProvider::config_spec) alone.
//!
//! The one thing it does that the other two do not is take a **host**. A module
//! supplies model providers the way it supplies actions and table providers, and
//! what arrives from it is data ([`ModelProviderKind`]) rather than code — so
//! [`register_host`](ModelRegistry::register_host) wraps each supplied kind in a
//! [`HostProvider`] and the map holds one kind of thing. A duplicate is refused
//! **naming both sources**, because "two providers are called `random_forest`"
//! is not actionable and "the module `@saltcorn/sklearn` supplies
//! `random_forest`, which is also a built-in" is.
//!
//! Like the other two, it is rebuilt rather than mutated when the module set
//! changes: `ModelServices` in `sc-server` holds it behind an `Arc` and swaps
//! the whole thing, so a fit that is running keeps the registry it started with.

use std::collections::BTreeMap;
use std::sync::Arc;

use sc_error::{Error, Result};

use crate::provider::{HostProvider, ModelProvider, ModelProviderHost, ModelProviderKind};

/// The model providers available to models, by name.
///
/// Built once (at boot, on a module change, and in tests) and then read-only, so
/// it is cheap to share behind an `Arc`. Ordered by name so the picker and every
/// error message that lists the alternatives are stable rather than hash-ordered.
#[derive(Clone, Default)]
pub struct ModelRegistry {
    providers: BTreeMap<String, Arc<dyn ModelProvider>>,
}

impl ModelRegistry {
    /// An empty registry.
    ///
    /// `sc_model::builtin_providers()` fills one with the built-in set (Phase 4,
    /// behind the `smartcore` feature for three of them), a module host adds its
    /// own, and a test starts from one with just the provider it is about.
    pub fn new() -> ModelRegistry {
        ModelRegistry::default()
    }

    /// Register `provider` under its own [`name`](ModelProvider::name).
    ///
    /// A duplicate name is refused rather than overwritten: two implementations
    /// claiming one name means every stored model referencing it is ambiguous,
    /// and letting the last registration win would make which one fits depend on
    /// load order.
    pub fn register(&mut self, provider: Arc<dyn ModelProvider>) -> Result<()> {
        let name = provider.name().to_owned();
        if name.trim().is_empty() {
            return Err(Error::config("a model provider must have a name"));
        }
        if let Some(existing) = self.providers.get(&name) {
            return Err(Error::config(format!(
                "two model providers are registered under the name `{name}`: one from {} and \
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
    /// Each arrives as a [`ModelProviderKind`] — a declaration, not code — and
    /// is wrapped in a [`HostProvider`] that routes `fit` and `predict` back to
    /// the host by the `(module, provider)` pair the kind carries.
    ///
    /// All or nothing is **not** the contract, and that is on purpose: the first
    /// failure stops the registration, because a registry that silently held
    /// half of a module's providers would make "why is my estimator missing" a
    /// question with no error to answer it. The caller reports it and the server
    /// carries on with the providers it did get, exactly as a module whose
    /// actions fail to load is reported and skipped.
    pub fn register_host(&mut self, host: Arc<dyn ModelProviderHost>) -> Result<()> {
        for kind in host.providers() {
            let provider = HostProvider::new(kind, Arc::clone(&host))?;
            self.register(Arc::new(provider))?;
        }
        Ok(())
    }

    /// The provider registered under `name`, if any.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ModelProvider>> {
        self.providers.get(name)
    }

    /// The provider registered under `name`, or a configuration error naming it
    /// and the registered alternatives.
    ///
    /// This is the lookup every model goes through — on save and on load —
    /// because a model naming a provider nothing implements must be reported,
    /// never silently fitted with something else.
    pub fn require(&self, name: &str) -> Result<&Arc<dyn ModelProvider>> {
        self.get(name).ok_or_else(|| {
            Error::config(format!(
                "unknown model provider `{name}`; the registered providers are {}",
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
    pub fn all(&self) -> impl Iterator<Item = &Arc<dyn ModelProvider>> {
        self.providers.values()
    }

    /// What the picker lists: every provider's name, description, module,
    /// declared settings, hyperparameters and outcome — in name order.
    ///
    /// The [`config_spec`](ModelProviderKind::config_spec) here is the
    /// *declaration*: the column pickers are unresolved, because the picker is
    /// shown before a dataset has been chosen. `listModelProviders` resolves them
    /// when the request carries a dataset (task 5.1).
    pub fn kinds(&self) -> Vec<ModelProviderKind> {
        self.providers.values().map(|p| p.kind()).collect()
    }

    /// How many providers are registered.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether nothing is registered — which, with the `smartcore` feature off
    /// and no module installed, is a real state the screen has to explain rather
    /// than an empty list that reads like a bug (§13).
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl std::fmt::Debug for ModelRegistry {
    /// Names only: a provider is a trait object with no meaningful `Debug`, and
    /// the names are the whole of what a reader wants from a registry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ModelRegistry").field(&self.names()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::provider::{FitResult, OutcomeSpec, Prediction};
    use async_trait::async_trait;
    use sc_types::{Attrs, FormField};
    use serde_json::Value as Json;

    /// A provider that does nothing, to exercise ordering and the duplicate
    /// refusal.
    struct Named(&'static str);

    #[async_trait]
    impl ModelProvider for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "test provider"
        }
        fn config_declaration(&self) -> Vec<FormField> {
            Vec::new()
        }
        fn outcome_spec(&self) -> OutcomeSpec {
            OutcomeSpec::Cluster
        }
        async fn fit(&self, _f: &Frame, _c: &Attrs, _h: &Attrs) -> Result<FitResult> {
            Ok(FitResult::new(Json::Null))
        }
        async fn predict(&self, _s: &Json, _f: &Frame) -> Result<Vec<Prediction>> {
            Ok(Vec::new())
        }
    }

    /// A host supplying the named providers, all from one module.
    struct Host(&'static str, Vec<&'static str>);

    #[async_trait]
    impl ModelProviderHost for Host {
        fn providers(&self) -> Vec<ModelProviderKind> {
            self.1
                .iter()
                .map(|name| {
                    ModelProviderKind::new(*name, "from a module", OutcomeSpec::Cluster)
                        .module(self.0)
                })
                .collect()
        }
        async fn fit(
            &self,
            _m: &str,
            _p: &str,
            _f: &Frame,
            _c: &Attrs,
            _h: &Attrs,
        ) -> Result<FitResult> {
            Ok(FitResult::new(Json::from("fitted by the module")))
        }
        async fn predict(
            &self,
            _m: &str,
            _p: &str,
            _s: &Json,
            _f: &Frame,
        ) -> Result<Vec<Prediction>> {
            Ok(vec![Prediction::number(1.0)])
        }
    }

    fn registry(names: &[&'static str]) -> ModelRegistry {
        let mut reg = ModelRegistry::new();
        for name in names {
            reg.register(Arc::new(Named(name))).unwrap();
        }
        reg
    }

    #[test]
    fn a_provider_is_found_by_the_name_it_declares() {
        let reg = registry(&["linear_regression", "kmeans"]);
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
        assert_eq!(reg.require("kmeans").unwrap().name(), "kmeans");
        assert!(reg.get("linear_regression").is_some());
        // Ordered by name, not by registration order, so every listing is stable.
        assert_eq!(reg.names(), vec!["kmeans", "linear_regression"]);
        assert_eq!(reg.all().count(), 2);
    }

    #[test]
    fn an_unknown_provider_names_itself_and_the_alternatives() {
        let reg = registry(&["kmeans"]);
        let msg = reg.require("pca").err().unwrap().to_string();
        assert!(msg.contains("pca") && msg.contains("kmeans"), "{msg}");
    }

    #[test]
    fn an_empty_registry_still_explains_itself() {
        let reg = ModelRegistry::new();
        assert!(reg.is_empty());
        let msg = reg.require("kmeans").err().unwrap().to_string();
        assert!(msg.contains("kmeans") && msg.contains("(none)"), "{msg}");
    }

    #[test]
    fn a_modules_providers_are_registered_as_providers_like_any_other() {
        let mut reg = registry(&["kmeans"]);
        reg.register_host(Arc::new(Host("@saltcorn/sklearn", vec!["gbm", "svm"])))
            .unwrap();
        assert_eq!(reg.names(), vec!["gbm", "kmeans", "svm"]);
        // Nothing downstream can tell which of the three came from a module,
        // except by asking — which is what the definition of done requires.
        let kinds = reg.kinds();
        let gbm = kinds.iter().find(|k| k.name == "gbm").unwrap();
        assert_eq!(gbm.module.as_deref(), Some("@saltcorn/sklearn"));
        let kmeans = kinds.iter().find(|k| k.name == "kmeans").unwrap();
        assert_eq!(kmeans.module, None);
        assert_eq!(kmeans.source(), "the built-in providers");
    }

    #[test]
    fn a_duplicate_name_is_refused_naming_both_sources() {
        let mut reg = registry(&["random_forest"]);
        let err = reg
            .register_host(Arc::new(Host("@saltcorn/sklearn", vec!["random_forest"])))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("random_forest"), "{msg}");
        assert!(
            msg.contains("built-in") && msg.contains("@saltcorn/sklearn"),
            "{msg}"
        );
        // The first registration is still the one that answers.
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.require("random_forest").unwrap().kind().module, None);
    }

    #[tokio::test]
    async fn a_hosted_provider_routes_its_fit_back_to_its_module() {
        let mut reg = ModelRegistry::new();
        reg.register_host(Arc::new(Host("@saltcorn/sklearn", vec!["gbm"])))
            .unwrap();
        let gbm = reg.require("gbm").unwrap();
        let fit = gbm
            .fit(&Frame::default(), &Attrs::new(), &Attrs::new())
            .await
            .unwrap();
        assert_eq!(fit.state, Json::from("fitted by the module"));
        assert_eq!(
            gbm.predict(&Json::Null, &Frame::default()).await.unwrap(),
            vec![Prediction::number(1.0)]
        );
    }

    #[test]
    fn a_hosted_provider_that_names_no_module_cannot_be_routed_to() {
        /// A host that forgot to say where its provider came from.
        struct Anonymous;
        #[async_trait]
        impl ModelProviderHost for Anonymous {
            fn providers(&self) -> Vec<ModelProviderKind> {
                vec![ModelProviderKind::new("x", "", OutcomeSpec::Cluster)]
            }
            async fn fit(
                &self,
                _m: &str,
                _p: &str,
                _f: &Frame,
                _c: &Attrs,
                _h: &Attrs,
            ) -> Result<FitResult> {
                unreachable!()
            }
            async fn predict(
                &self,
                _m: &str,
                _p: &str,
                _s: &Json,
                _f: &Frame,
            ) -> Result<Vec<Prediction>> {
                unreachable!()
            }
        }
        let err = ModelRegistry::new()
            .register_host(Arc::new(Anonymous))
            .unwrap_err();
        assert!(err.to_string().contains("names no module"), "{err}");
    }

    #[test]
    fn the_debug_rendering_is_the_names() {
        let reg = registry(&["kmeans", "pca"]);
        assert_eq!(format!("{reg:?}"), "ModelRegistry([\"kmeans\", \"pca\"])");
    }
}
