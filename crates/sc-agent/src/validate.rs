//! Validating an [`Agent`] before it is stored — and again when it is loaded.
//!
//! Every way an agent can be wrong is checked in one place, and checked **on
//! save**, because that is when the admin is standing in front of the form: a
//! provider that is not connected, a trait nothing implements, a configuration
//! the trait does not declare, a table the trait names that no longer exists, two
//! enabled traits whose tools would collide. Discovering any of those mid-answer
//! means an agent that fails — or, worse, quietly answers without the tool it was
//! built around.
//!
//! The same function runs at **load** ([`Agents`]), where an agent that no longer
//! validates is dropped from the live set with its reason kept. That is the
//! fail-closed reading a trigger's load takes, for the same reason.

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_types::validate_attrs;

use crate::agent::Agent;
use crate::agent_trait::TraitCheck;
use crate::registry::AgentRegistry;
use crate::store::list_agents;

/// The longest tool name both vendors accept, and the character set they accept
/// it in.
///
/// Checked here rather than left to the provider because the failure is
/// otherwise a 400 in the middle of a conversation, naming a tool the admin never
/// typed — the name was derived from their table's, and the connection between
/// the two is exactly what an error at the vendor cannot make.
const MAX_TOOL_NAME: usize = 64;

/// Check everything about `agent` that can be checked without running it.
///
/// Called by [`save_agent`](crate::save_agent) and by [`Agents::load`] . The
/// error names the agent first, then the problem, because the admin is looking at
/// a list of agents when they see it.
pub async fn validate_agent(
    catalog: &Catalog,
    registry: &AgentRegistry,
    agent: &Agent,
) -> Result<()> {
    let name = agent.name.trim();
    if name.is_empty() {
        return Err(Error::invalid("an agent needs a name"));
    }
    let problem = |msg: String| Error::invalid(format!("agent `{name}`: {msg}"));

    // The provider must be *connected*, not merely named: an agent pointing at a
    // provider nobody configured cannot answer, and the admin who deleted the
    // provider is the one who needs telling.
    let provider = agent.provider.trim();
    if provider.is_empty() {
        return Err(problem("no LLM provider is set".to_owned()));
    }
    if sc_llm::load_llm_provider_by_name(catalog, provider)
        .await?
        .is_none()
    {
        return Err(problem(format!("no LLM provider named `{provider}`")));
    }

    if let Some(role) = agent.min_role
        && !(1..=100).contains(&role)
    {
        return Err(problem(format!(
            "`min_role` must be a role between 1 and 100, got {role}"
        )));
    }

    // Each enabled trait: the name resolves, the values are of the declared
    // shapes, and then the trait's own check — which is the only one that can
    // know whether the table it names still exists.
    //
    // Numbered, because a trait may be enabled more than once and "trait
    // `query_table`" would otherwise not say which of them is wrong.
    let mut tool_names: Vec<(String, String)> = Vec::new();
    for (i, enabled) in agent.traits.iter().enumerate() {
        let position = i + 1;
        let where_ = |msg: String| {
            problem(format!(
                "trait {position} (`{}`): {msg}",
                enabled.trait_.trim()
            ))
        };
        let trait_ = registry
            .require(enabled.trait_.trim())
            .map_err(|e| where_(e.to_string()))?;
        validate_attrs(&trait_.config_spec(), &enabled.config)
            .map_err(|e| where_(e.to_string()))?;
        trait_
            .validate_config(&TraitCheck {
                catalog,
                config: &enabled.config,
                agent: name,
            })
            .await
            .map_err(|e| where_(e.to_string()))?;

        for tool in trait_.tools(&enabled.config) {
            if tool.name.is_empty()
                || tool.name.len() > MAX_TOOL_NAME
                || !tool
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(where_(format!(
                    "the tool name `{}` is not one a provider will accept \
                     (up to {MAX_TOOL_NAME} letters, digits, `_` or `-`)",
                    tool.name
                )));
            }
            // A collision is refused here, where it is fixable by renaming or
            // dropping one of the two, rather than discovered when the model
            // picks whichever of them the request happened to carry last.
            if let Some((_, other)) = tool_names.iter().find(|(name, _)| *name == tool.name) {
                return Err(where_(format!(
                    "its tool `{}` has the same name as one from {other}; \
                     each enabled trait's tools must be distinguishable",
                    tool.name
                )));
            }
            tool_names.push((tool.name, format!("trait {position}")));
        }
    }

    Ok(())
}

/// Why one stored agent is not in the live set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIssue {
    /// The agent's name, as stored.
    pub agent: String,
    /// What is wrong with it, in the words the validator used.
    pub problem: String,
}

/// The agents that can run, plus the ones that cannot and why.
///
/// `sc-action`'s `Triggers`'s twin. An agent that fails validation is
/// **not** deleted and **not** hidden: it stays stored, stays listed and stays
/// editable, because editing it is the repair. What it does not do is answer.
#[derive(Debug, Clone, Default)]
pub struct Agents {
    agents: Vec<Agent>,
    issues: Vec<AgentIssue>,
}

impl Agents {
    /// An empty set — a catalog with no `_sc_agents` table, and the starting
    /// point for a test.
    pub fn empty() -> Agents {
        Agents::default()
    }

    /// Load and validate every stored agent.
    ///
    /// A catalog with no `_sc_agents` table yields an empty set rather than an
    /// error: that table's absence *means* "no agents have ever been defined".
    pub async fn load(catalog: &Catalog, registry: &AgentRegistry) -> Result<Agents> {
        if catalog.get(crate::AGENTS_TABLE)?.is_none() {
            return Ok(Agents::empty());
        }
        let mut out = Agents::default();
        for agent in list_agents(catalog).await? {
            match validate_agent(catalog, registry, &agent).await {
                Ok(()) => out.agents.push(agent),
                Err(e) => out.issues.push(AgentIssue {
                    agent: agent.name.clone(),
                    problem: e.to_string(),
                }),
            }
        }
        Ok(out)
    }

    /// Reload in place, so a live handle picks up a save or a delete.
    pub async fn reload(&mut self, catalog: &Catalog, registry: &AgentRegistry) -> Result<()> {
        *self = Agents::load(catalog, registry).await?;
        Ok(())
    }

    /// The agents that can run, ordered by name.
    pub fn all(&self) -> &[Agent] {
        &self.agents
    }

    /// The stored agents that cannot, with their reasons.
    pub fn issues(&self) -> &[AgentIssue] {
        &self.issues
    }

    /// The agent named `name`, if it is in the live set.
    pub fn by_name(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// The agent named `name`, or an error that distinguishes the two ways it can
    /// be missing — never defined, or defined and invalid.
    ///
    /// The distinction is the whole point: "no agent named `librarian`" sends the
    /// admin looking for a typo, and "agent `librarian` is not usable: no LLM
    /// provider named `anthropic`" sends them to the thing that is actually
    /// broken.
    pub fn require(&self, name: &str) -> Result<&Agent> {
        if let Some(agent) = self.by_name(name) {
            return Ok(agent);
        }
        match self.issues.iter().find(|i| i.agent == name) {
            Some(issue) => Err(Error::invalid(format!(
                "agent `{name}` is not usable: {}",
                issue.problem
            ))),
            None => Err(Error::not_found(format!("no agent named `{name}`"))),
        }
    }
}
