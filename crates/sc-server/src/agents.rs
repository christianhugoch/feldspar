//! Bringing the agents up at boot (§11.2, §11.4).
//!
//! The twin of [`triggers`](crate::triggers), and for the same reason: the trait
//! seam is in `sc-agent` (layer 7), the built-in traits are in `sc-core-traits`
//! (layer 9, above the row layer so a trait's write goes through `sc-api`), and
//! neither knows about the other. This is where they are put together, once, by
//! the process that is going to serve requests.
//!
//! What is assembled here is [`AgentServices`]: the trait registry every agent's
//! configuration is validated against, and **how an agent's provider is
//! connected**. The second is a seam rather than a call because every test of
//! the chat socket has to drive a whole turn without a vendor and without
//! spending a token (decision 7) — see [`ProviderConnector`].

use std::sync::Arc;

use sc_agent::{Agent, AgentRegistry, bootstrap_agents, bootstrap_runs};
use sc_catalog::Catalog;
use sc_error::{Context, Result};
use sc_llm::LlmProvider;

/// How a run gets the provider it talks to.
///
/// In a server this is [`StoredProviders`]: the agent names an
/// `_sc_llm_providers` record, that record is loaded and connected, and the
/// agent's `model` overrides the provider's default. It is a **trait** rather
/// than that function called directly because the whole of §11.4 — deltas
/// arriving in order, a tool call rendering with its result, an abort, a
/// provider that refuses — has to be testable against a script (decision 7,
/// `sc_agent::testing::FakeProvider`), and pointing a real adapter at a stub
/// endpoint would test the adapter rather than the socket.
///
/// The seam is at the *connection*, not inside the loop: whatever answers, the
/// driver, the run storage and the socket are the production ones.
#[async_trait::async_trait]
pub trait ProviderConnector: Send + Sync {
    /// The connected provider for `agent`, or why there is none.
    async fn connect(&self, catalog: &Catalog, agent: &Agent) -> Result<Arc<dyn LlmProvider>>;
}

/// The production connector: the provider the agent's definition names.
pub struct StoredProviders;

#[async_trait::async_trait]
impl ProviderConnector for StoredProviders {
    async fn connect(&self, catalog: &Catalog, agent: &Agent) -> Result<Arc<dyn LlmProvider>> {
        sc_agent::connect(catalog, agent).await
    }
}

/// Everything the agent surface needs that is not a request: which traits exist,
/// and how to reach a model.
///
/// Cloneable and shared, like the trigger dispatcher, because the admin API's
/// handlers and the chat socket must validate against the *same* registry — an
/// agent that saved against one trait set and ran against another would be
/// refused in one place and accepted in the other.
#[derive(Clone)]
pub struct AgentServices {
    registry: Arc<AgentRegistry>,
    providers: Arc<dyn ProviderConnector>,
}

impl AgentServices {
    /// Services over `registry`, connecting providers from their stored records.
    pub fn new(registry: Arc<AgentRegistry>) -> AgentServices {
        AgentServices {
            registry,
            providers: Arc::new(StoredProviders),
        }
    }

    /// Resolve providers some other way — the seam [`ProviderConnector`]
    /// describes.
    pub fn with_providers(mut self, providers: Arc<dyn ProviderConnector>) -> AgentServices {
        self.providers = providers;
        self
    }

    /// The traits an agent's configuration is validated against.
    pub fn registry(&self) -> &Arc<AgentRegistry> {
        &self.registry
    }

    /// How this server connects an agent's provider.
    pub fn providers(&self) -> &Arc<dyn ProviderConnector> {
        &self.providers
    }
}

/// Assemble the agent services a server runs with: the built-in traits, and the
/// two tables an agent and its runs are stored in.
///
/// Fails only on what a server must not start without — the built-in trait set
/// not assembling (a duplicate registration, which is a bug) or the tables not
/// being creatable. A **stored agent** that does not validate is not one of
/// those: it is dropped from the live set with its reason reported, stays listed
/// and stays editable, exactly as a trigger that does not validate is.
pub async fn install_agents(catalog: &Arc<Catalog>) -> Result<AgentServices> {
    bootstrap_agents(catalog)
        .await
        .context("ensuring the agents table exists")?;
    bootstrap_runs(catalog)
        .await
        .context("ensuring the runs table exists")?;
    let registry = sc_core_traits::builtin_traits().context("registering the built-in traits")?;
    let services = AgentServices::new(Arc::new(registry));
    for issue in sc_agent::validate::Agents::load(catalog, services.registry())
        .await?
        .issues()
    {
        eprintln!(
            "saltcorn: agent `{}` is stored but not usable: {}",
            issue.agent, issue.problem
        );
    }
    Ok(services)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_services_carry_the_built_in_traits_and_the_stored_connector() {
        let services = AgentServices::new(Arc::new(
            sc_core_traits::builtin_traits().expect("the built-in traits assemble"),
        ));
        assert!(services.registry().get("query_table").is_some());
        assert!(services.registry().get("run_trigger").is_some());
    }
}
