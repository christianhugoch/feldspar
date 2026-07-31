//! The [`Runner`]: the one thing that does IO for an [`AgentLoop`].
//!
//! The machine decides; this drives. It builds each request from the agent's
//! definition, streams it through the [`LlmProvider`], dispatches tool calls to
//! the traits that declared them, and — after **every** step — writes the run to
//! `_sc_runs`. Splitting it this way is what makes the decisions testable without
//! a provider and the persistence uniform: there is exactly one place a step
//! ends, so there is exactly one place a step is saved.
//!
//! ## What the driver refuses to decide
//!
//! Who the tools run as. That travels on the [`RunCaller`] the runner was
//! constructed with, and there is no default (decision 5): a chat turn carries
//! the person, a triggered run carries the trigger's authority, and a `Runner`
//! that never said which cannot be built.

use std::sync::Arc;

use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_expr::JsEvaluator;
use sc_llm::{AssistantMessage, LlmDelta, LlmProvider, LlmRequest, ToolCall, ToolSpec};

use crate::agent::Agent;
use crate::agent_trait::{RunCaller, TraitContext, Turn};
use crate::machine::{AgentLoop, Conclusion, Step, ToolOutcome};
use crate::registry::AgentRegistry;
use crate::run::Run;
use crate::run_store::save_run;

/// What a caller watching a run wants to see while it happens.
///
/// The chat socket (§11.4) is the implementation that matters; a triggered run
/// passes nothing and the default no-ops do the rest. It exists at this layer
/// rather than in the transport because the events are the *loop's* — a tool
/// call, its result — and only the loop knows when they happen.
///
/// Synchronous and returning nothing: an observer watches, it does not steer. A
/// hook that could rewrite a tool call would make the transcript stop being a
/// record of what happened.
pub trait RunObserver: Send + Sync {
    /// A delta from the model's stream, as it arrives.
    fn on_delta(&self, delta: &LlmDelta) {
        let _ = delta;
    }

    /// A tool is about to run.
    fn on_tool_call(&self, call: &ToolCall) {
        let _ = call;
    }

    /// A tool has returned — successfully or not.
    fn on_tool_result(&self, outcome: &ToolOutcome) {
        let _ = outcome;
    }
}

/// The observer a caller who is not watching passes.
impl RunObserver for () {}

/// Everything one agent needs to actually run: its definition, a connected
/// provider, the traits it names, and who it runs as.
pub struct Runner<'a> {
    catalog: &'a Catalog,
    registry: &'a AgentRegistry,
    agent: &'a Agent,
    provider: Arc<dyn LlmProvider>,
    caller: RunCaller,
    observer: Option<&'a dyn RunObserver>,
    evaluator: Option<&'a Arc<dyn JsEvaluator>>,
}

impl<'a> Runner<'a> {
    /// A runner for `agent`, through `provider`, on behalf of `caller`.
    ///
    /// The provider is passed in rather than resolved here because resolving it
    /// is [`connect_provider`](sc_llm::connect_provider)'s job and doing it once
    /// per run rather than once per step is the caller's decision to make.
    pub fn new(
        catalog: &'a Catalog,
        registry: &'a AgentRegistry,
        agent: &'a Agent,
        provider: Arc<dyn LlmProvider>,
        caller: RunCaller,
    ) -> Runner<'a> {
        Runner {
            catalog,
            registry,
            agent,
            provider,
            caller,
            observer: None,
            evaluator: None,
        }
    }

    /// Watch the run as it happens.
    pub fn observing(mut self, observer: &'a dyn RunObserver) -> Runner<'a> {
        self.observer = Some(observer);
        self
    }

    /// Give this run's tools the JavaScript engine.
    ///
    /// Optional, and deliberately: an agent whose traits never touch a table with
    /// an untranslatable ownership formula never needs one, and a `Runner` that
    /// demanded an engine to hold a conversation would make every caller build a
    /// V8 isolate to say hello. A tool that does need it and does not have it
    /// says so (`TraitContext::require_evaluator`) rather than reading anyway.
    pub fn with_evaluator(mut self, evaluator: &'a Arc<dyn JsEvaluator>) -> Runner<'a> {
        self.evaluator = Some(evaluator);
        self
    }

    /// Who this runner's tools execute as.
    pub fn caller(&self) -> &RunCaller {
        &self.caller
    }

    /// Start a new run with `message` and drive it to a conclusion.
    ///
    /// The run is returned whatever happened, including when the provider failed:
    /// a failed run is a row the chat panel can show with its reason, and
    /// throwing it away would leave the person watching with nothing (§11.4).
    pub async fn start(&self, message: impl Into<String>) -> Result<(Run, Conclusion)> {
        let mut state = AgentLoop::new(self.agent.max_steps());
        state.push_user(message)?;
        let mut run = Run::new(&self.agent.name, &self.caller, &state);
        save_run(self.catalog, &run).await?;
        let conclusion = self.drive(&mut run).await?;
        Ok((run, conclusion))
    }

    /// Add `message` to an existing run and drive it on — the second and later
    /// turns of a conversation.
    pub async fn continue_run(
        &self,
        run: &mut Run,
        message: impl Into<String>,
    ) -> Result<Conclusion> {
        let mut state = run.agent_loop()?;
        state.push_user(message)?;
        run.record(&state);
        save_run(self.catalog, run).await?;
        self.drive(run).await
    }

    /// Drive `run` from wherever it is until it concludes.
    ///
    /// Resuming is the same call: the state came off the row, so a run reloaded
    /// after a restart carries on from the step it was on.
    pub async fn drive(&self, run: &mut Run) -> Result<Conclusion> {
        let mut state = run.agent_loop()?;
        loop {
            match state.next_step() {
                Step::Done(conclusion) => {
                    run.record(&state);
                    save_run(self.catalog, run).await?;
                    return Ok(conclusion);
                }
                Step::CallModel { messages, step } => {
                    let request = self.request(messages, step).await?;
                    match self.ask(request).await {
                        Ok(answer) => state.model_answered(answer)?,
                        Err(e) => {
                            // The conversation is intact and the reason is on the
                            // row; the caller gets the error too, because a chat
                            // socket has to render it as an event rather than
                            // simply stopping.
                            run.fail(&e);
                            save_run(self.catalog, run).await?;
                            return Err(e);
                        }
                    }
                }
                Step::CallTools { calls } => {
                    let mut outcomes = Vec::with_capacity(calls.len());
                    for call in calls {
                        outcomes.push(self.dispatch(call, run).await);
                    }
                    state.tool_results(outcomes)?;
                }
            }
            // After every step, without exception. A run written only at the end
            // is a run that cannot be resumed and a chat that cannot be reloaded
            // mid-answer.
            run.record(&state);
            save_run(self.catalog, run).await?;
        }
    }

    /// Build one request: the system prompt with whatever the traits appended,
    /// the conversation, and every enabled trait's tools.
    ///
    /// Rebuilt per step rather than kept on the run, because the agent is
    /// editable while its runs exist — a conversation resumed after a trait was
    /// added must get the new tool.
    async fn request(&self, messages: Vec<sc_llm::LlmMessage>, step: u32) -> Result<LlmRequest> {
        let mut turn = Turn::new(&self.caller, &self.agent.name, step);
        let mut tools: Vec<ToolSpec> = Vec::new();
        for enabled in &self.agent.traits {
            let trait_ = self.registry.require(&enabled.trait_)?;
            trait_.on_turn(&enabled.config, &mut turn).await?;
            tools.extend(trait_.tools(self.catalog, &enabled.config));
        }

        let system = turn.system_prompt(&self.agent.system_prompt);
        let mut request = LlmRequest {
            system: (!system.trim().is_empty()).then_some(system),
            messages,
            tools,
            max_tokens: self.agent.max_tokens(),
            temperature: self.agent.temperature(),
        };
        // An agent with no traits offers no tools, which is a request with an
        // empty list rather than one with a field the vendors read as "call
        // something".
        request.tools.retain(|t| !t.name.is_empty());
        Ok(request)
    }

    /// Send one request and assemble the answer, forwarding deltas as they
    /// arrive.
    async fn ask(&self, request: LlmRequest) -> Result<AssistantMessage> {
        let mut stream = self.provider.stream(request).await?;
        let mut answer = AssistantMessage::default();
        while let Some(delta) = stream.next().await {
            let delta = delta?;
            if let Some(observer) = self.observer {
                observer.on_delta(&delta);
            }
            answer.push(delta);
        }
        Ok(answer)
    }

    /// Run one tool call, as the run's caller.
    ///
    /// Never returns an error: a tool that fails — including one the model
    /// invented — comes back as the tool *result*, because an error the model can
    /// read is one it can recover from (§11.2). Only a failure of the loop itself
    /// ends a run.
    async fn dispatch(&self, call: ToolCall, run: &Run) -> ToolOutcome {
        if let Some(observer) = self.observer {
            observer.on_tool_call(&call);
        }
        let outcome = match self.owner(&call.name) {
            None => ToolOutcome::failed(
                call.clone(),
                format!(
                    "there is no tool named `{}`; the tools available are {}",
                    call.name,
                    self.tool_names().join(", ")
                ),
            ),
            Some((trait_index, tool_name)) => {
                let enabled = &self.agent.traits[trait_index];
                match self.registry.require(&enabled.trait_) {
                    Err(e) => ToolOutcome::failed(call.clone(), e),
                    Ok(trait_) => {
                        let mut ctx = TraitContext {
                            catalog: self.catalog,
                            caller: &self.caller,
                            agent: &self.agent.name,
                            run: run.id,
                            evaluator: self.evaluator,
                        };
                        match trait_
                            .call(&enabled.config, &tool_name, &call.arguments, &mut ctx)
                            .await
                        {
                            Ok(value) => ToolOutcome::ok(call.clone(), &value),
                            Err(e) => ToolOutcome::failed(call.clone(), e),
                        }
                    }
                }
            }
        };
        if let Some(observer) = self.observer {
            observer.on_tool_result(&outcome);
        }
        outcome
    }

    /// Which enabled trait declares `tool`, by position — the answer validation
    /// guaranteed is unique.
    fn owner(&self, tool: &str) -> Option<(usize, String)> {
        for (i, enabled) in self.agent.traits.iter().enumerate() {
            let Ok(trait_) = self.registry.require(&enabled.trait_) else {
                continue;
            };
            if trait_
                .tools(self.catalog, &enabled.config)
                .iter()
                .any(|spec| spec.name == tool)
            {
                return Some((i, tool.to_owned()));
            }
        }
        None
    }

    /// Every tool this agent offers, for the message a model that invented one
    /// gets back.
    fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for enabled in &self.agent.traits {
            if let Ok(trait_) = self.registry.require(&enabled.trait_) {
                names.extend(
                    trait_
                        .tools(self.catalog, &enabled.config)
                        .into_iter()
                        .map(|t| t.name),
                );
            }
        }
        if names.is_empty() {
            names.push("(none)".to_owned());
        }
        names
    }
}

/// The connected provider for `agent`, resolved from its stored definition.
///
/// Separate from [`Runner::new`] so a caller that runs several turns connects
/// once, and so a test can hand the runner a fake without a database row.
pub async fn connect(catalog: &Catalog, agent: &Agent) -> Result<Arc<dyn LlmProvider>> {
    let def = sc_llm::load_llm_provider_by_name(catalog, agent.provider.trim())
        .await?
        .ok_or_else(|| {
            Error::invalid(format!(
                "agent `{}`: no LLM provider named `{}`",
                agent.name, agent.provider
            ))
        })?;
    sc_llm::connect_provider(&def, agent.model.as_deref())
}
