#![allow(dead_code)] // each test binary compiles this module and uses part of it

//! The scaffolding both integration tests share: a catalog with the agent
//! tables bootstrapped, a connected provider row to point agents at, and a
//! trait whose tools are derived from its configuration.

use std::sync::Arc;
use std::sync::Mutex;

use sc_agent::{
    AgentRegistry, AgentTrait, ToolsContext, TraitCheck, TraitContext, bootstrap_agents,
    bootstrap_runs,
};
use sc_catalog::Catalog;
use sc_db::DatabaseDriver;
use sc_db_postgres::PgDriver;
use sc_error::{Error, Result};
use sc_llm::{ConnectedModel, LlmProvider, ToolSpec, bootstrap_llm_providers};
use sc_test_harness::TestDb;
use sc_types::{Attrs, BasicType, FormField};
use serde_json::{Value as Json, json};

/// A catalog over a per-test database with `_fd_llm_providers`,
/// `_fd_llm_models`, `_fd_agents` and `_fd_runs` bootstrapped, and one provider
/// named `main` saved with two models — `claude-sonnet-4-5`, its default, and
/// `claude-opus-5` — because an agent that names no connected provider and
/// model does not validate, so every test would otherwise start by writing the
/// same rows.
pub async fn catalog(db: &TestDb) -> Result<Catalog> {
    let driver = Arc::new(PgDriver::from_pool(db.pool().clone()));
    let catalog = Catalog::init(driver as Arc<dyn DatabaseDriver>).await?;
    bootstrap_llm_providers(&catalog).await?;
    bootstrap_agents(&catalog).await?;
    bootstrap_runs(&catalog).await?;
    sc_llm::save_llm_provider(
        &catalog,
        &sc_llm::LlmProviderDef::new("main", sc_llm::ANTHROPIC_BACKEND)
            .with(sc_llm::CFG_API_KEY, "sk-ant-not-a-real-key"),
    )
    .await?;
    // The model an agent naming no model calls: the provider's default row.
    let provider = sc_llm::require_llm_provider(&catalog, "main").await?;
    sc_llm::save_llm_model(
        &catalog,
        &sc_llm::LlmModelDef::new(provider.id, "claude-sonnet-4-5").default_model(),
    )
    .await?;
    sc_llm::save_llm_model(
        &catalog,
        &sc_llm::LlmModelDef::new(provider.id, "claude-opus-5"),
    )
    .await?;
    Ok(catalog)
}

/// A scripted provider as the connected executor a `Runner` takes.
pub fn model<P: LlmProvider + 'static>(provider: Arc<P>) -> ConnectedModel {
    ConnectedModel::unconfigured(provider as Arc<dyn LlmProvider>)
}

/// A trait that counts something in a named collection.
///
/// It exists to exercise the three things every real trait will do and the loop
/// depends on: its **tool name is derived from its configuration**
/// (`count_books`, not `count`), its `validate_config` refuses a collection it
/// does not know about, and its `call` can be made to fail on demand.
pub struct Counter {
    /// What it returns, and what it records having been asked.
    pub calls: Mutex<Vec<Json>>,
}

impl Counter {
    pub fn new() -> Arc<Counter> {
        Arc::new(Counter {
            calls: Mutex::new(Vec::new()),
        })
    }

    /// The collections it knows — anything else fails `validate_config`, which is
    /// how a trait configured against something that no longer exists behaves.
    pub const KNOWN: [&'static str; 2] = ["books", "orders"];

    /// The arguments it was called with, in order.
    pub fn seen(&self) -> Vec<Json> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The configured collection, or the empty string.
fn collection(config: &Attrs) -> String {
    config
        .get("collection")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[async_trait::async_trait]
impl AgentTrait for Counter {
    fn name(&self) -> &str {
        "count"
    }

    fn description(&self) -> &str {
        "Count the things in one collection"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![
            FormField::new("collection", BasicType::Text).required(),
            // Not required, and the reason a test can make a tool fail without a
            // second trait: the failure is a configured property of this one.
            FormField::new("always_fails", BasicType::Bool),
        ]
    }

    async fn validate_config(&self, check: &TraitCheck<'_>) -> Result<()> {
        let name = collection(check.config);
        if !Counter::KNOWN.contains(&name.as_str()) {
            return Err(Error::invalid(format!("no collection named `{name}`")));
        }
        Ok(())
    }

    fn tools(&self, _cx: &ToolsContext<'_>, config: &Attrs) -> Vec<ToolSpec> {
        vec![ToolSpec::new(
            format!("count_{}", collection(config)),
            format!("Count the {} ", collection(config)),
            json!({"type": "object", "properties": {}}),
        )]
    }

    async fn call(
        &self,
        config: &Attrs,
        _tool: &str,
        args: &Json,
        _ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(args.clone());
        if config
            .get("always_fails")
            .and_then(Json::as_bool)
            .unwrap_or(false)
        {
            return Err(Error::invalid(format!(
                "the {} collection is unavailable",
                collection(config)
            )));
        }
        Ok(json!(match collection(config).as_str() {
            "books" => 3,
            "orders" => 7,
            _ => 0,
        }))
    }
}

/// A trait that says something extra in the system prompt and offers no tools —
/// the `on_turn` half of the extension point.
pub struct Preamble;

#[async_trait::async_trait]
impl AgentTrait for Preamble {
    fn name(&self) -> &str {
        "preamble"
    }

    fn description(&self) -> &str {
        "Append a line to the system prompt"
    }

    fn config_spec(&self) -> Vec<FormField> {
        vec![FormField::new("text", BasicType::Text).required()]
    }

    fn tools(&self, _cx: &ToolsContext<'_>, _config: &Attrs) -> Vec<ToolSpec> {
        Vec::new()
    }

    async fn call(
        &self,
        _config: &Attrs,
        tool: &str,
        _args: &Json,
        _ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        Err(Error::invalid(format!(
            "`{tool}` is not a tool of this trait"
        )))
    }

    async fn on_turn(&self, config: &Attrs, turn: &mut sc_agent::Turn<'_>) -> Result<()> {
        let text = config
            .get("text")
            .and_then(Json::as_str)
            .unwrap_or_default();
        // Which step it is, so a test can see this runs before *every* call.
        turn.append_system(format!("{text} (step {})", turn.step));
        Ok(())
    }
}

/// A trait that uses the loop's Phase 2 seams: per-run state (`tally`), a
/// tool offered only in `plan` mode (`note_plan`), a tool that takes wall time
/// (`nap`), and a session of its own agent in another mode (`start_session`).
pub struct Tally;

#[async_trait::async_trait]
impl AgentTrait for Tally {
    fn name(&self) -> &str {
        "tally"
    }

    fn description(&self) -> &str {
        "Keep a count in the run's state"
    }

    fn config_spec(&self) -> Vec<FormField> {
        Vec::new()
    }

    fn tools(&self, cx: &ToolsContext<'_>, _config: &Attrs) -> Vec<ToolSpec> {
        let object = json!({"type": "object", "properties": {}});
        let mut tools = vec![
            ToolSpec::new("tally", "Add one to the count", object.clone()),
            ToolSpec::new("nap", "Sleep for `ms` milliseconds", object.clone()),
            ToolSpec::new(
                "start_session",
                "Start a session of this agent",
                object.clone(),
            ),
        ];
        if cx.mode == sc_agent::RunMode::Plan {
            tools.push(ToolSpec::new("note_plan", "Write down the plan", object));
        }
        tools
    }

    async fn call(
        &self,
        _config: &Attrs,
        tool: &str,
        args: &Json,
        ctx: &mut TraitContext<'_>,
    ) -> Result<Json> {
        match tool {
            "tally" => {
                let state = ctx.state();
                let count = state.get("count").and_then(Json::as_u64).unwrap_or(0) + 1;
                *state = json!({"count": count});
                Ok(json!(count))
            }
            "note_plan" => Ok(json!("noted")),
            "nap" => {
                let ms = args.get("ms").and_then(Json::as_u64).unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(json!("rested"))
            }
            "start_session" => {
                let mode = sc_agent::RunMode::parse(
                    args.get("mode").and_then(Json::as_str).unwrap_or("act"),
                )?;
                let resume = args
                    .get("resume")
                    .and_then(Json::as_str)
                    .map(|id| uuid::Uuid::parse_str(id).map(sc_agent::RunId))
                    .transpose()
                    .map_err(|e| Error::invalid(e.to_string()))?;
                let agent = ctx.agent.to_owned();
                let parent = ctx.run;
                let mut request =
                    sc_agent::DelegateRequest::new(&agent, "do the feature", parent).mode(mode);
                if let Some(id) = resume {
                    request = request.resume(id);
                }
                let delegated = ctx.require_delegate()?.delegate(request).await?;
                Ok(json!({
                    "run": delegated.run.to_string(),
                    "answer": delegated.answer(),
                    "steps": delegated.steps,
                }))
            }
            other => Err(Error::invalid(format!("no tool `{other}`"))),
        }
    }
}

/// A registry with the test traits in it.
pub fn registry(counter: Arc<Counter>) -> Result<AgentRegistry> {
    let mut registry = AgentRegistry::new();
    registry.register(counter)?;
    registry.register(Arc::new(Preamble))?;
    registry.register(Arc::new(Tally))?;
    Ok(registry)
}
