//! The [`Runner`]: the one thing that does IO for an [`AgentLoop`].
//!
//! The machine decides; this drives. It builds each request from the agent's
//! definition, streams it through the [`LlmProvider`](sc_llm::LlmProvider), dispatches tool calls to
//! the traits that declared them, and — after **every** step — writes the run to
//! `_fd_runs`. Splitting it this way is what makes the decisions testable without
//! a provider and the persistence uniform: there is exactly one place a step
//! ends, so there is exactly one place a step is saved.
//!
//! ## What the driver refuses to decide
//!
//! Who the tools run as. That travels on the [`RunCaller`] the runner was
//! constructed with, and there is no default (decision 5): a chat turn carries
//! the person, a triggered run carries the trigger's authority, and a `Runner`
//! that never said which cannot be built.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sc_action::TriggerDispatcher;
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_expr::JsEvaluator;
use sc_llm::{AssistantMessage, ConnectedModel, LlmDelta, LlmRequest, ToolCall, ToolSpec};

use crate::agent::{Agent, ModelRole};
use crate::agent_trait::{RunCaller, ToolsContext, TraitContext, Turn};
use crate::delegate::{ATTR_DELEGATED_BY, ATTR_PARENT_RUN, DelegateRequest, Delegated, Delegator};
use crate::ledger::ChildLedger;
use crate::machine::{AgentLoop, Conclusion, Step, StepMeta, ToolOutcome, trait_state_key};
use crate::registry::AgentRegistry;
use crate::run::{Run, RunId, RunMode};
use crate::run_store::{load_run, save_run};

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

/// Everything one agent needs to actually run: its definition, its connected
/// models, the traits it names, and who it runs as.
pub struct Runner<'a> {
    catalog: &'a Catalog,
    registry: &'a AgentRegistry,
    agent: &'a Agent,
    /// The agent's own model: the executor, connected by the caller.
    executor: ConnectedModel,
    /// The roles' models, connected on first use through `connector`.
    roles: Mutex<HashMap<ModelRole, ConnectedModel>>,
    caller: RunCaller,
    observer: Option<&'a dyn RunObserver>,
    evaluator: Option<&'a Arc<dyn JsEvaluator>>,
    triggers: Option<&'a Arc<TriggerDispatcher>>,
    connector: Option<&'a Arc<dyn ProviderConnector>>,
    /// The mode and role a run this runner *starts* is given. A run it drives
    /// carries its own on its row.
    mode: RunMode,
    role: ModelRole,
    /// The agents above this one in the delegation chain, root first — empty for
    /// a run a person or a trigger started.
    ///
    /// Carried on the runner rather than on the run because it is a property of
    /// *this* execution, not of the transcript: a delegated run reloaded on its
    /// own is a run in its own right, and what its parent was is on the row
    /// ([`ATTR_DELEGATED_BY`]) for a reader, not for a bound to be re-derived
    /// from.
    chain: Vec<String>,
}

impl<'a> Runner<'a> {
    /// A runner for `agent`, answered by `executor`, on behalf of `caller`.
    ///
    /// The executor is passed in rather than resolved here because resolving it
    /// is [`connect_model`](sc_llm::connect_model)'s job and doing it once
    /// per run rather than once per step is the caller's decision to make. The
    /// roles' models are connected lazily, through
    /// [`with_connector`](Runner::with_connector).
    pub fn new(
        catalog: &'a Catalog,
        registry: &'a AgentRegistry,
        agent: &'a Agent,
        executor: ConnectedModel,
        caller: RunCaller,
    ) -> Runner<'a> {
        Runner {
            catalog,
            registry,
            agent,
            executor,
            roles: Mutex::new(HashMap::new()),
            caller,
            observer: None,
            evaluator: None,
            triggers: None,
            connector: None,
            mode: RunMode::Act,
            role: ModelRole::Executor,
            chain: Vec::new(),
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

    /// Give this run's tools the trigger dispatcher — the server's own, so a
    /// trigger an agent runs is the trigger everything else runs (§11.3).
    ///
    /// Optional on the same terms as the evaluator: an agent with no
    /// `run_trigger` trait never needs one, and a tool that does need it and has
    /// not got it says so (`TraitContext::require_triggers`) rather than finding
    /// another way to fire an event.
    pub fn with_triggers(mut self, triggers: &'a Arc<TriggerDispatcher>) -> Runner<'a> {
        self.triggers = Some(triggers);
        self
    }

    /// Connect models through `connector`: the agent's **roles** (TODO §3), and
    /// every sub-agent this run **delegates** to (§11.3's `subagent`).
    ///
    /// Optional on the same terms as the evaluator and the dispatcher: an agent
    /// with no roles and no `subagent` trait never needs it. A run that asks a
    /// configured role without one fails with a configuration error, and a tool
    /// that delegates without one says so
    /// ([`TraitContext::require_delegate`](crate::TraitContext::require_delegate)).
    ///
    /// The connector rather than connected models, because a role and a
    /// sub-agent name a provider and a model of their own: routing work to a
    /// cheaper model is most of the point of both.
    pub fn with_connector(mut self, connector: &'a Arc<dyn ProviderConnector>) -> Runner<'a> {
        self.connector = Some(connector);
        self
    }

    /// Start runs in `mode`, answered by `role`'s model.
    pub fn starting_in(mut self, mode: RunMode, role: ModelRole) -> Runner<'a> {
        self.mode = mode;
        self.role = role;
        self
    }

    /// Run as the bottom of `chain` — the agents that delegated their way here,
    /// root first.
    ///
    /// Only delegation has cause to call this: it is what makes the depth bound
    /// and the cycle check possible one level down, and a caller who set it by
    /// hand would be claiming a history that did not happen.
    pub fn within(mut self, chain: Vec<String>) -> Runner<'a> {
        self.chain = chain;
        self
    }

    /// Who this runner's tools execute as.
    pub fn caller(&self) -> &RunCaller {
        &self.caller
    }

    /// A new run holding `message`, within the agent's budgets and in this
    /// runner's starting mode and role — not yet saved, so a caller can give it
    /// a description first.
    pub fn new_run(&self, message: impl Into<String>) -> Result<Run> {
        let mut state = AgentLoop::for_agent(self.agent);
        state.push_user(message)?;
        Ok(Run::new(&self.agent.name, &self.caller, &state)
            .with_mode(self.mode)
            .with_role(self.role))
    }

    /// Start a new run with `message` and drive it to a conclusion.
    ///
    /// The run is returned whatever happened, including when the provider failed:
    /// a failed run is a row the chat panel can show with its reason, and
    /// throwing it away would leave the person watching with nothing (§11.4).
    pub async fn start(&self, message: impl Into<String>) -> Result<(Run, Conclusion)> {
        let mut run = self.new_run(message)?;
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

    /// The connected model for `role`: the executor, or the role's own model
    /// connected on first use. A role the agent leaves unset is the executor.
    pub async fn model(&self, role: ModelRole) -> Result<ConnectedModel> {
        if role == ModelRole::Executor || self.agent.configured_role(role)?.is_none() {
            return Ok(self.executor.clone());
        }
        if let Some(model) = lock(&self.roles).get(&role) {
            return Ok(model.clone());
        }
        let connector = self.connector.ok_or_else(|| {
            Error::config(format!(
                "agent `{}`: its `{role}` role names a model, and this context \
                 has no way to connect one",
                self.agent.name
            ))
        })?;
        let model = connector.connect(self.catalog, self.agent, role).await?;
        lock(&self.roles).insert(role, model.clone());
        Ok(model)
    }

    /// Drive `run` from wherever it is until it concludes.
    ///
    /// Resuming is the same call: the state came off the row, so a run reloaded
    /// after a restart carries on from the step it was on.
    pub async fn drive(&self, run: &mut Run) -> Result<Conclusion> {
        let mut state = run.agent_loop()?;
        let mode = run.mode()?;
        let role = run.role()?;
        // What a run cost, said once at the end (§16). Timed from here rather
        // than from the row's creation because this is the part that is
        // *running*: a run resumed after a restart is one drive, not one that
        // took a fortnight.
        let started = std::time::Instant::now();
        sc_log::log_verbose!(
            "agent `{}` run {}: driving from step {} in {mode} mode as {role}",
            self.agent.name,
            run.id,
            state.step()
        );
        loop {
            match state.next_step() {
                Step::Done(conclusion) => {
                    run.record(&state);
                    save_run(self.catalog, run).await?;
                    let total = state.ledger().total();
                    sc_log::log_info!(
                        "agent `{}` run {}: {} after {} steps in {}, cost {}, cache hits {}",
                        self.agent.name,
                        run.id,
                        conclusion_label(&conclusion),
                        state.step(),
                        sc_log::human_duration(started.elapsed()),
                        total
                            .cost
                            .map_or_else(|| "unknown".to_owned(), |c| format!("{c:.4}")),
                        total
                            .cache_hit_ratio()
                            .map_or_else(|| "n/a".to_owned(), |r| format!("{:.0}%", r * 100.0)),
                    );
                    return Ok(conclusion);
                }
                Step::CallModel { messages, step } => {
                    let began = std::time::Instant::now();
                    let answered = match self.model(role).await {
                        Ok(model) => {
                            let request = self.request(messages, step, mode, &model).await?;
                            self.ask(&model, request)
                                .await
                                .map(|answer| (model, answer))
                        }
                        Err(e) => Err(e),
                    };
                    let elapsed = began.elapsed();
                    state.ledger_mut().add_working(elapsed);
                    match answered {
                        Ok((model, answer)) => {
                            let meta = StepMeta {
                                role,
                                model: model.provider.model().to_owned(),
                                cost: answer.usage.cost(&model.prices),
                                elapsed,
                            };
                            state.model_answered_with(answer, meta)?;
                        }
                        Err(e) => {
                            // The conversation is intact and the reason is on the
                            // row; the caller gets the error too, because a chat
                            // socket has to render it as an event rather than
                            // simply stopping.
                            sc_log::log_warn!(
                                "agent `{}` run {}: failed at step {step} — {e}",
                                self.agent.name,
                                run.id
                            );
                            run.record(&state);
                            run.fail(&e);
                            save_run(self.catalog, run).await?;
                            return Err(e);
                        }
                    }
                }
                Step::CallTools { calls } => {
                    let began = std::time::Instant::now();
                    let children = Mutex::new(Vec::new());
                    let capabilities = match self.model(role).await {
                        Ok(model) => model.capabilities,
                        Err(_) => self.executor.capabilities,
                    };
                    let tools = ToolsContext::new(self.catalog, mode, &capabilities);
                    let mut outcomes = Vec::with_capacity(calls.len());
                    for call in calls {
                        outcomes.push(
                            self.dispatch(call, run.id, mode, &tools, &mut state, &children)
                                .await,
                        );
                    }
                    state.tool_results(outcomes)?;
                    let ledger = state.ledger_mut();
                    ledger.add_working(began.elapsed());
                    for child in lock(&children).drain(..) {
                        ledger.record_child(child);
                    }
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
    /// the conversation, and every enabled trait's tools for this mode.
    ///
    /// Rebuilt per step rather than kept on the run, because the agent is
    /// editable while its runs exist — a conversation resumed after a trait was
    /// added must get the new tool.
    async fn request(
        &self,
        messages: Vec<sc_llm::LlmMessage>,
        step: u32,
        mode: RunMode,
        model: &ConnectedModel,
    ) -> Result<LlmRequest> {
        let mut turn = Turn::new(&self.caller, &self.agent.name, step);
        turn.mode = mode;
        let cx = ToolsContext::new(self.catalog, mode, &model.capabilities);
        let mut tools: Vec<ToolSpec> = Vec::new();
        for enabled in &self.agent.traits {
            let trait_ = self.registry.require(&enabled.trait_)?;
            trait_.on_turn(&enabled.config, &mut turn).await?;
            tools.extend(trait_.tools(&cx, &enabled.config));
        }

        let system = turn.system_prompt(&self.agent.system_prompt);
        let mut request = LlmRequest {
            system: (!system.trim().is_empty()).then_some(system),
            messages,
            tools,
            max_tokens: self.agent.max_tokens(),
            temperature: self.agent.temperature(),
            // Off unless the agent says otherwise (R§12): one call per turn is
            // what a doom-loop detector can fingerprint. The adapters send it
            // only with tools.
            parallel_tool_calls: Some(self.agent.parallel_tool_calls()),
            // Cache breakpoints and a cache key are the loop's to decide once
            // the request layout is (TODO Phase 4); until then the host's
            // defaults.
            ..LlmRequest::default()
        };
        // An agent with no traits offers no tools, which is a request with an
        // empty list rather than one with a field the vendors read as "call
        // something".
        request.tools.retain(|t| !t.name.is_empty());
        Ok(request)
    }

    /// Send one request to `model` and assemble the answer, forwarding deltas as
    /// they arrive.
    async fn ask(&self, model: &ConnectedModel, request: LlmRequest) -> Result<AssistantMessage> {
        let mut stream = model.provider.stream(request).await?;
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
    async fn dispatch(
        &self,
        call: ToolCall,
        run: RunId,
        mode: RunMode,
        tools: &ToolsContext<'_>,
        state: &mut AgentLoop,
        children: &Mutex<Vec<ChildLedger>>,
    ) -> ToolOutcome {
        if let Some(observer) = self.observer {
            observer.on_tool_call(&call);
        }
        // The arguments in full at trace: they are the model's actual decision,
        // and the summary line's tool *name* is exactly the part that is never
        // in question when something went wrong.
        if sc_log::enabled(sc_log::Verbosity::Trace) {
            sc_log::log_trace!(
                "agent `{}` run {} tool `{}` arguments:\n{}",
                self.agent.name,
                run,
                call.name,
                serde_json::to_string_pretty(&call.arguments)
                    .unwrap_or_else(|e| format!("‹could not be serialised for the log: {e}›"))
            );
        }
        let started = std::time::Instant::now();
        let outcome = match self.owner(&call.name, tools) {
            None => ToolOutcome::failed(
                call.clone(),
                format!(
                    "there is no tool named `{}`; the tools available are {}",
                    call.name,
                    self.tool_names(tools).join(", ")
                ),
            ),
            Some(trait_index) => {
                let enabled = &self.agent.traits[trait_index];
                match self.registry.require(&enabled.trait_) {
                    Err(e) => ToolOutcome::failed(call.clone(), e),
                    Ok(trait_) => {
                        let delegation = RunDelegation {
                            runner: self,
                            parent_mode: mode,
                            children,
                        };
                        let key = trait_state_key(trait_index, &enabled.trait_);
                        let mut ctx = TraitContext {
                            catalog: self.catalog,
                            caller: &self.caller,
                            agent: &self.agent.name,
                            run,
                            mode,
                            trait_state: state.trait_state_mut(&key),
                            evaluator: self.evaluator,
                            triggers: self.triggers,
                            // Offered only where this deployment assembled a way
                            // to connect a sub-agent's provider, so a trait that
                            // needs one gets `require_delegate`'s configuration
                            // error rather than a runner that cannot finish.
                            delegate: self.connector.map(|_| &delegation as &dyn Delegator),
                        };
                        match trait_
                            .call(&enabled.config, &call.name, &call.arguments, &mut ctx)
                            .await
                        {
                            Ok(value) => ToolOutcome::ok(call.clone(), &value),
                            Err(e) => ToolOutcome::failed(call.clone(), e),
                        }
                    }
                }
            }
        };
        sc_log::log_info!(
            "agent `{}` run {}: tool `{}` {} in {}",
            self.agent.name,
            run,
            call.name,
            if outcome.is_error { "failed" } else { "ok" },
            sc_log::human_duration(started.elapsed())
        );
        // What the model will actually read back, in full — the other half of
        // the conversation the request dump shows one turn later.
        sc_log::log_trace!(
            "agent `{}` run {} tool `{}` result:\n{}",
            self.agent.name,
            run,
            call.name,
            outcome.content
        );
        if let Some(observer) = self.observer {
            observer.on_tool_result(&outcome);
        }
        outcome
    }

    /// Which enabled trait offers `tool` in this mode, by position — the answer
    /// validation guaranteed is unique.
    fn owner(&self, tool: &str, cx: &ToolsContext<'_>) -> Option<usize> {
        self.agent.traits.iter().position(|enabled| {
            self.registry.require(&enabled.trait_).is_ok_and(|trait_| {
                trait_
                    .tools(cx, &enabled.config)
                    .iter()
                    .any(|spec| spec.name == tool)
            })
        })
    }

    /// Every tool this agent offers in this mode, for the message a model that
    /// invented one gets back.
    fn tool_names(&self, cx: &ToolsContext<'_>) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for enabled in &self.agent.traits {
            if let Ok(trait_) = self.registry.require(&enabled.trait_) {
                names.extend(
                    trait_
                        .tools(cx, &enabled.config)
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

    /// Start or resume a child run for `request`, from a run in `parent_mode`
    /// (§11.3, TODO §5).
    async fn delegate_from(
        &self,
        parent_mode: RunMode,
        children: &Mutex<Vec<ChildLedger>>,
        request: DelegateRequest<'_>,
    ) -> Result<Delegated> {
        let parent = self.agent.name.as_str();
        let target = request.agent.trim();
        let problem = |msg: String| Error::invalid(format!("agent `{parent}`: {msg}"));
        let child_mode = request.mode.unwrap_or_default();
        let child_role = request.role.unwrap_or_default();

        // The chain as it *would* be: everyone above this run, then this agent.
        // Both checks read it, and both read it the same way.
        let mut chain = self.chain.clone();
        chain.push(parent.to_owned());

        // The one cycle allowed: a run nobody delegated starting a session of
        // its own agent in another mode. Its child is at depth 1, where the
        // chain is no longer empty, so the exception cannot recurse.
        let own_session = target == parent && self.chain.is_empty() && child_mode != parent_mode;
        if target == parent && !own_session {
            return Err(problem(if self.chain.is_empty() {
                format!(
                    "`{parent}` may start a session of its own only in a different \
                     mode; this run is already in `{parent_mode}` mode"
                )
            } else {
                format!(
                    "delegating to `{target}` would loop: {} → `{target}`. A \
                     delegated run cannot start a session of its own agent.",
                    chain
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" → ")
                )
            }));
        }
        // A cycle first, because it is the more specific diagnosis: `a → b → a`
        // would also trip a depth bound eventually, and "you have exceeded three
        // levels" sends the admin to a number when the fault is a loop they can
        // see.
        if !own_session && let Some(at) = chain.iter().position(|name| name == target) {
            return Err(problem(format!(
                "delegating to `{target}` would loop: {} → `{target}`. \
                 An agent cannot be asked to do work it is already doing.",
                chain[at..]
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" → ")
            )));
        }
        let depth = u32::try_from(chain.len()).unwrap_or(u32::MAX);
        if depth > request.max_depth {
            return Err(problem(format!(
                "delegating to `{target}` would be {depth} agents deep and this \
                 delegation allows {}: {}. Do the work here, or ask for a \
                 smaller task.",
                request.max_depth,
                chain
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" → ")
            )));
        }

        let connector = self.connector.ok_or_else(|| {
            Error::config(format!(
                "agent `{parent}`: no way to connect a sub-agent's provider is \
                 configured on this server"
            ))
        })?;

        // The **live** set, so an agent whose provider vanished or whose trait
        // was configured against a dropped table says that, in its own
        // validation's words, rather than failing somewhere inside the child
        // loop.
        let agents = crate::validate::Agents::load(self.catalog, self.registry).await?;
        let sub = agents.require(target).map_err(|e| problem(e.to_string()))?;

        // The sub-agent's own floor, on top of the caller's authority — being
        // allowed to chat with one agent does not thereby allow everything it
        // can reach, which is the rule `run_trigger` applies to a trigger.
        if !self.caller.meets_role(sub.min_role) {
            return Err(Error::auth(format!(
                "agent `{parent}`: you may not use `{target}`; \
                 it needs role {} or better",
                sub.min_role.unwrap_or(1)
            )));
        }

        let executor = connector
            .connect(self.catalog, sub, ModelRole::Executor)
            .await?;
        let child = Runner {
            catalog: self.catalog,
            registry: self.registry,
            agent: sub,
            executor,
            roles: Mutex::new(HashMap::new()),
            // The parent run's caller, unchanged: delegation must not be a way
            // to reach a row the person chatting could not have been shown.
            caller: self.caller.clone(),
            // Deliberately not the parent's: the child's deltas are a different
            // conversation, and interleaving them into the parent's stream would
            // render one agent's thinking as another's (§11.4).
            observer: None,
            evaluator: self.evaluator,
            triggers: self.triggers,
            connector: self.connector,
            mode: child_mode,
            role: child_role,
            chain,
        };

        let existing = match request.resume {
            Some(id) => load_run(self.catalog, id).await?,
            None => None,
        };
        let mut run = match existing {
            Some(run) => {
                let parent_of = run.attributes.get(ATTR_PARENT_RUN).and_then(|v| v.as_str());
                if run.subject != sub.name || parent_of != Some(&request.parent_run.to_string()) {
                    return Err(problem(format!(
                        "run {} is not a session of `{target}` started by this run, \
                         so it cannot be resumed from here",
                        run.id
                    )));
                }
                sc_log::log_verbose!(
                    "agent `{parent}` run {}: resuming child run {}",
                    request.parent_run,
                    run.id
                );
                run
            }
            None => {
                let mut state = AgentLoop::for_agent(sub);
                if let Some(steps) = request.max_steps {
                    state = AgentLoop::new(steps).with_budgets(state.budgets());
                }
                state.push_user(request.briefing)?;
                let mut run = Run::new(&sub.name, &child.caller, &state)
                    .description(if own_session {
                        format!("{child_mode} session of `{parent}`")
                    } else {
                        format!("delegated by `{parent}`")
                    })
                    .with_mode(child_mode)
                    .with_role(child_role);
                if let Some(id) = request.resume {
                    run.id = id;
                }
                run.attributes
                    .insert(ATTR_DELEGATED_BY.to_owned(), parent.into());
                // The link the chat panel reads a nested transcript through. A
                // string, because a JSON number cannot hold a UUID.
                run.attributes.insert(
                    ATTR_PARENT_RUN.to_owned(),
                    request.parent_run.to_string().into(),
                );
                save_run(self.catalog, &run).await?;
                run
            }
        };

        let driven = child.drive(&mut run).await;
        // Rolled up whatever happened: a child that failed halfway still spent.
        let ledger = run
            .agent_loop()
            .map(|l| l.ledger().clone())
            .unwrap_or_default();
        lock(children).push(ledger.as_child(run.id, &sub.name));
        let conclusion = driven.map_err(|e| problem(format!("`{target}` could not run: {e}")))?;
        Ok(Delegated {
            agent: sub.name.clone(),
            run: run.id,
            conclusion,
            steps: run.agent_loop().map(|l| l.step()).unwrap_or_default(),
            totals: ledger.total(),
        })
    }
}

/// How a conclusion reads in a few words on the run's closing line.
fn conclusion_label(conclusion: &Conclusion) -> String {
    match conclusion {
        Conclusion::Answered { .. } => "answered".to_owned(),
        Conclusion::MaxSteps => "ran out of steps".to_owned(),
        Conclusion::Aborted => "was aborted".to_owned(),
        Conclusion::OverBudget { budget } => format!("ran out of its {budget} budget"),
    }
}

/// The delegation one tool call is offered: the runner, plus what it needs to
/// know about the run the call belongs to.
struct RunDelegation<'r, 'a> {
    runner: &'r Runner<'a>,
    /// The mode of the run doing the asking, which a session of its own agent
    /// must differ from.
    parent_mode: RunMode,
    /// Where the child's ledger goes, for the parent's to roll up.
    children: &'r Mutex<Vec<ChildLedger>>,
}

/// A run delegates by starting another one — the sub-agent's own — under the
/// same authority, bounded by depth and refused on a cycle (§11.3).
#[async_trait::async_trait]
impl Delegator for RunDelegation<'_, '_> {
    async fn delegate(&self, request: DelegateRequest<'_>) -> Result<Delegated> {
        self.runner
            .delegate_from(self.parent_mode, self.children, request)
            .await
    }
}

/// A lock that survives a poisoned mutex: a panic elsewhere has already failed
/// the run, and the map it guards is a cache.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How a run gets the model it talks to.
///
/// In a deployment this is [`StoredProviders`], which is [`connect`]: the agent
/// names an `_fd_llm_providers` record and one of its `_fd_llm_models` rows (or
/// none, for the provider's default) for itself and for each role, and the two
/// are loaded and connected. It
/// is a **trait** rather than that function called directly because everything
/// built on top of a run — the chat socket's deltas and aborts (§11.4), the
/// `run_agent` action a trigger fires (§11.5) — has to be testable against a
/// script rather than a vendor (decision 7,
/// [`FakeProvider`](crate::testing::FakeProvider)), and pointing a real adapter
/// at a stub endpoint would test the adapter instead.
///
/// The seam is at the **connection**, not inside the loop: whatever answers, the
/// driver, the run storage and everything above them are the production ones.
#[async_trait::async_trait]
pub trait ProviderConnector: Send + Sync {
    /// The connected model `agent` uses for `role` — its own for the executor
    /// and for a role it leaves unset — with its capabilities and prices, or why
    /// there is none.
    async fn connect(
        &self,
        catalog: &Catalog,
        agent: &Agent,
        role: ModelRole,
    ) -> Result<ConnectedModel>;
}

/// The production connector: the provider and model the agent's definition
/// names.
pub struct StoredProviders;

#[async_trait::async_trait]
impl ProviderConnector for StoredProviders {
    async fn connect(
        &self,
        catalog: &Catalog,
        agent: &Agent,
        role: ModelRole,
    ) -> Result<ConnectedModel> {
        connect(catalog, agent, role).await
    }
}

/// The connected model `agent` uses for `role`, resolved from its stored
/// definition: the provider the role names (the agent's own for the executor
/// or an unset role), and the model row it names under that provider or the
/// provider's default.
///
/// Separate from [`Runner::new`] so a caller that runs several turns connects
/// once, and so a test can hand the runner a fake without a database row.
pub async fn connect(catalog: &Catalog, agent: &Agent, role: ModelRole) -> Result<ConnectedModel> {
    let named = agent.model_for(role);
    let def = sc_llm::load_llm_provider_by_name(catalog, named.provider.trim())
        .await?
        .ok_or_else(|| {
            Error::invalid(format!(
                "agent `{}`: no LLM provider named `{}`",
                agent.name, named.provider
            ))
        })?;
    let model = sc_llm::require_llm_model(catalog, &def, named.model.as_deref())
        .await
        .map_err(|e| match e.repr() {
            sc_error::Repr::NotFound(msg) => {
                Error::invalid(format!("agent `{}`: {msg}", agent.name))
            }
            _ => e,
        })?;
    sc_llm::connect_model(&def, &model)
}
