//! The loop, as a **steppable machine** rather than an `async fn` (§11.2,
//! decision 8).
//!
//! [`AgentLoop`] owns every *decision* the loop makes — what to send, when to run
//! tools, when to stop — and performs no IO at all. A driver advances it by
//! asking [`next_step`](AgentLoop::next_step) what to do and feeding the result
//! back through [`model_answered`](AgentLoop::model_answered) or
//! [`tool_results`](AgentLoop::tool_results):
//!
//! ```text
//! next_step() ──▶ CallModel { messages }  ──▶ model_answered(assistant)
//!             ──▶ CallTools { calls }     ──▶ tool_results(outcomes)
//!             ──▶ Done(conclusion)
//! ```
//!
//! ## Why this shape
//!
//! Because the whole state is a value, it is `Serialize + Deserialize`, and *the
//! state is what `_sc_runs` stores*. A run persisted after every step is
//! therefore a run that resumes: load it, ask for the next step, carry on. An
//! `async fn` holding the same state on its stack could do the work but not the
//! resuming, and §10.3's durable engine would then need a second mechanism to do
//! what this one already does. The shape is borrowed from `rig-agent`'s sans-IO
//! `AgentRun`; the crate is not (§11.1 records why).
//!
//! It also makes the loop testable without a provider, a database or a runtime:
//! every test in this module is synchronous, and the ones that need a model use
//! the [`FakeProvider`](crate::testing::FakeProvider) through the driver.
//!
//! ## What it deliberately does not decide
//!
//! The system prompt and the tool list are **not** in the state. They are rebuilt
//! from the agent's definition on every turn, because an agent is editable while
//! its runs exist: a conversation resumed after a trait was added must get the
//! new tool, not the one that was current when it started.

use sc_error::{Error, Result};
use sc_llm::{AssistantMessage, LlmMessage, ToolCall, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::agent::DEFAULT_MAX_STEPS;

/// What the driver must do next to advance an [`AgentLoop`].
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Send these messages to the model and feed the answer back through
    /// [`model_answered`](AgentLoop::model_answered).
    CallModel {
        /// The conversation so far, oldest first. The system prompt and the tools
        /// are the driver's to add — they come from the agent, not the run.
        messages: Vec<LlmMessage>,
        /// Which model call this will be, counting from 1. What a trait's
        /// [`on_turn`](crate::AgentTrait::on_turn) is told, and what the step
        /// budget is spent from.
        step: u32,
    },
    /// Run these tool calls **in this order** and feed the outcomes back through
    /// [`tool_results`](AgentLoop::tool_results).
    ///
    /// Sequentially, because a trait that writes a row and a trait that reads one
    /// have an order between them that only the model knows (§11.2). Running them
    /// concurrently is carried past this milestone precisely because deciding
    /// which are independent is a separate question.
    CallTools {
        /// The calls, in the order the model asked for them.
        calls: Vec<ToolCall>,
    },
    /// The run is over; nothing more will be asked of the driver.
    Done(Conclusion),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "conclusion", rename_all = "snake_case")]
pub enum Conclusion {
    /// The model finished its turn: this is its answer, and the conversation can
    /// be continued with another [`push_user`](AgentLoop::push_user).
    Answered {
        /// The final assistant text. Empty is a real answer — a model that ends a
        /// turn having only called tools has said nothing, and pretending
        /// otherwise would put words in its mouth.
        answer: String,
    },
    /// The step budget ran out with the model still asking for tools.
    ///
    /// Not an error: the transcript is intact and readable, and the number that
    /// stopped it is the admin's own ([`max_steps`](crate::Agent::max_steps)).
    MaxSteps,
    /// Someone pressed stop.
    Aborted,
}

impl Conclusion {
    /// The final answer, for a run that produced one.
    pub fn answer(&self) -> Option<&str> {
        match self {
            Conclusion::Answered { answer } => Some(answer),
            Conclusion::MaxSteps | Conclusion::Aborted => None,
        }
    }
}

/// One tool call's outcome, on its way back into the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// The call this answers. Carried whole rather than as an id, so the loop can
    /// check that what came back is what it asked for.
    pub call: ToolCall,
    /// What the model is told, whether the tool succeeded or failed.
    pub content: String,
    /// Whether this is a failure — for the transcript, which renders a failed
    /// tool differently, and for nothing else: the model sees only `content`.
    pub is_error: bool,
}

impl ToolOutcome {
    /// A tool that returned `value`.
    ///
    /// A JSON string is passed through unquoted; anything else is serialised
    /// compactly. Quoting a plain string would spend tokens on punctuation and
    /// make the model read escaping as content.
    pub fn ok(call: ToolCall, value: &Json) -> ToolOutcome {
        let content = match value {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        };
        ToolOutcome {
            call,
            content,
            is_error: false,
        }
    }

    /// A tool that failed, whose error becomes its result (§11.2).
    ///
    /// The prefix is there because the model has no other signal: a tool result
    /// is a string, and "no table named `bookz`" reads as data unless something
    /// says it is a failure. An error the model can read is one it can recover
    /// from — which is the whole reason a failing tool does not end the run.
    pub fn failed(call: ToolCall, error: impl std::fmt::Display) -> ToolOutcome {
        ToolOutcome {
            call,
            content: format!("error: {error}"),
            is_error: true,
        }
    }
}

/// Which side the machine is waiting on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    /// The model is next.
    Model,
    /// These tool calls are next.
    Tools { calls: Vec<ToolCall> },
    /// Nothing is next.
    Done { conclusion: Conclusion },
}

/// The conversation, the budget, and which side is next: everything one run is.
///
/// `Serialize + Deserialize`, and that is not incidental — this value *is*
/// `_sc_runs.context`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoop {
    /// The conversation so far, oldest first.
    messages: Vec<LlmMessage>,
    /// Model calls made so far.
    step: u32,
    /// The budget.
    max_steps: u32,
    /// Every call's usage, accumulated.
    usage: Usage,
    /// Which side is next.
    phase: Phase,
}

impl Default for AgentLoop {
    fn default() -> Self {
        AgentLoop::new(DEFAULT_MAX_STEPS)
    }
}

impl AgentLoop {
    /// An empty conversation with a step budget.
    ///
    /// A budget of zero would make a run that can never call the model, so it is
    /// raised to one: the caller who wrote zero gets a run that answers rather
    /// than one that reports a conclusion nobody can act on.
    /// [`Agent::max_steps`](crate::Agent::max_steps) has already made the same
    /// correction; this is the second gate, for a caller that did not come
    /// through an agent.
    pub fn new(max_steps: u32) -> AgentLoop {
        AgentLoop {
            messages: Vec::new(),
            step: 0,
            max_steps: max_steps.max(1),
            usage: Usage::default(),
            phase: Phase::Model,
        }
    }

    /// Add what the person said, starting or continuing the conversation.
    ///
    /// Refused mid-turn: a message that arrived while a tool was running would
    /// have to be spliced into a history the model is already answering, and
    /// silently appending it there is how a question gets answered before it was
    /// asked. A chat client holds the message until the turn ends.
    pub fn push_user(&mut self, text: impl Into<String>) -> Result<()> {
        // A turn boundary is the start of the run or the end of a turn. Anywhere
        // else the history is half-built — an assistant message awaiting its tool
        // results, or a question already on its way to the model.
        let at_boundary = self.messages.is_empty() || matches!(self.phase, Phase::Done { .. });
        if !at_boundary {
            return Err(Error::invalid(
                "a message cannot be added while the agent is mid-turn; \
                 wait for the turn to finish",
            ));
        }
        self.messages.push(LlmMessage::user(text));
        self.phase = Phase::Model;
        Ok(())
    }

    /// What the driver must do next.
    ///
    /// **Idempotent**: asking twice without answering gives the same instruction
    /// twice, which is what makes a resumed run resumable — a process that died
    /// between being told to call the model and doing it comes back and is told
    /// again.
    pub fn next_step(&mut self) -> Step {
        match &self.phase {
            Phase::Done { conclusion } => Step::Done(conclusion.clone()),
            Phase::Tools { calls } => Step::CallTools {
                calls: calls.clone(),
            },
            Phase::Model => {
                if self.step >= self.max_steps {
                    self.phase = Phase::Done {
                        conclusion: Conclusion::MaxSteps,
                    };
                    return Step::Done(Conclusion::MaxSteps);
                }
                Step::CallModel {
                    messages: self.messages.clone(),
                    step: self.step + 1,
                }
            }
        }
    }

    /// Feed back what the model said.
    ///
    /// The reasoning is not appended: it is not part of the conversation, and
    /// [`AssistantMessage::message`] is where that decision lives.
    pub fn model_answered(&mut self, answer: AssistantMessage) -> Result<()> {
        if !matches!(self.phase, Phase::Model) {
            return Err(Error::msg(
                "the agent loop was given a model answer it did not ask for",
            ));
        }
        self.step += 1;
        self.usage.add(answer.usage);
        self.messages.push(answer.message());
        self.phase = if answer.tool_calls.is_empty() {
            Phase::Done {
                conclusion: Conclusion::Answered {
                    answer: answer.content,
                },
            }
        } else {
            Phase::Tools {
                calls: answer.tool_calls,
            }
        };
        Ok(())
    }

    /// Feed back what the tools returned.
    ///
    /// Every call must be answered, in the order it was asked: a result missing
    /// or out of place would leave a tool call with no matching result in the
    /// history, which both providers reject outright — better to fail here, where
    /// the driver's own mistake is visible, than at the vendor.
    pub fn tool_results(&mut self, outcomes: Vec<ToolOutcome>) -> Result<()> {
        let Phase::Tools { calls } = &self.phase else {
            return Err(Error::msg(
                "the agent loop was given tool results it did not ask for",
            ));
        };
        if outcomes.len() != calls.len()
            || outcomes
                .iter()
                .zip(calls)
                .any(|(outcome, call)| outcome.call.id != call.id)
        {
            return Err(Error::msg(format!(
                "the agent loop asked for {} tool results and got {}, or got them out of order",
                calls.len(),
                outcomes.len()
            )));
        }
        for outcome in outcomes {
            self.messages
                .push(LlmMessage::tool_result(&outcome.call, outcome.content));
        }
        self.phase = Phase::Model;
        Ok(())
    }

    /// Stop the run where it is.
    ///
    /// The transcript is left exactly as it stands, because the history is what
    /// the chat panel shows afterwards and truncating it would hide what the
    /// agent had already done. A run already finished is not re-concluded.
    pub fn abort(&mut self) {
        if !matches!(self.phase, Phase::Done { .. }) {
            self.phase = Phase::Done {
                conclusion: Conclusion::Aborted,
            };
        }
    }

    /// The conversation so far.
    pub fn messages(&self) -> &[LlmMessage] {
        &self.messages
    }

    /// Every model call's usage, accumulated.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// How many model calls have been made.
    pub fn step(&self) -> u32 {
        self.step
    }

    /// The budget.
    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    /// Whether the run is over.
    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done { .. })
    }

    /// How it ended, if it has.
    pub fn conclusion(&self) -> Option<&Conclusion> {
        match &self.phase {
            Phase::Done { conclusion } => Some(conclusion),
            Phase::Model | Phase::Tools { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: json!({}),
        }
    }

    fn text_answer(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: text.to_owned(),
            ..AssistantMessage::default()
        }
    }

    fn tool_answer(calls: Vec<ToolCall>) -> AssistantMessage {
        AssistantMessage {
            tool_calls: calls,
            ..AssistantMessage::default()
        }
    }

    #[test]
    fn a_single_turn_asks_the_model_once_and_ends() {
        let mut run = AgentLoop::new(20);
        run.push_user("hello").unwrap();

        let Step::CallModel { messages, step } = run.next_step() else {
            panic!("the first step is a model call");
        };
        assert_eq!(step, 1);
        assert_eq!(messages, vec![LlmMessage::user("hello")]);

        run.model_answered(text_answer("hi")).unwrap();
        assert!(run.is_done());
        assert_eq!(
            run.next_step(),
            Step::Done(Conclusion::Answered {
                answer: "hi".to_owned()
            })
        );
        assert_eq!(run.step(), 1);
    }

    #[test]
    fn a_tool_call_is_run_and_the_conversation_continues() {
        let mut run = AgentLoop::new(20);
        run.push_user("how many books?").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "query_books")]))
            .unwrap();

        assert_eq!(
            run.next_step(),
            Step::CallTools {
                calls: vec![call("c1", "query_books")]
            }
        );
        run.tool_results(vec![ToolOutcome::ok(call("c1", "query_books"), &json!(3))])
            .unwrap();

        // Back to the model, with the call and its result in the history.
        let Step::CallModel { messages, step } = run.next_step() else {
            panic!("after tools, the model runs again");
        };
        assert_eq!(step, 2);
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[2], LlmMessage::ToolResult { .. }));

        run.model_answered(text_answer("three")).unwrap();
        assert_eq!(run.conclusion().unwrap().answer(), Some("three"));
    }

    #[test]
    fn two_calls_in_one_turn_are_answered_in_order() {
        let mut run = AgentLoop::new(20);
        run.push_user("do both").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "read"), call("c2", "write")]))
            .unwrap();

        let Step::CallTools { calls } = run.next_step() else {
            panic!("two calls are still one tools step");
        };
        assert_eq!(calls.len(), 2);

        run.tool_results(vec![
            ToolOutcome::ok(call("c1", "read"), &json!("a")),
            ToolOutcome::ok(call("c2", "write"), &json!("b")),
        ])
        .unwrap();

        // The results are in the history in the order the model asked.
        let ids: Vec<&str> = run
            .messages()
            .iter()
            .filter_map(|m| match m {
                LlmMessage::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["c1", "c2"]);
    }

    #[test]
    fn results_that_do_not_match_the_calls_are_refused() {
        let mut run = AgentLoop::new(20);
        run.push_user("go").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "read"), call("c2", "write")]))
            .unwrap();
        run.next_step();

        // One result for two calls: a history with an unanswered tool call is one
        // both vendors reject, so it is refused here instead.
        let err = run
            .tool_results(vec![ToolOutcome::ok(call("c1", "read"), &json!("a"))])
            .unwrap_err();
        assert!(err.to_string().contains('2'), "{err}");

        // Out of order is refused too.
        let err = run
            .tool_results(vec![
                ToolOutcome::ok(call("c2", "write"), &json!("b")),
                ToolOutcome::ok(call("c1", "read"), &json!("a")),
            ])
            .unwrap_err();
        assert!(err.to_string().contains("order"), "{err}");
    }

    #[test]
    fn a_failed_tool_becomes_a_result_the_model_can_read() {
        let mut run = AgentLoop::new(20);
        run.push_user("go").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "query_bookz")]))
            .unwrap();
        run.next_step();
        run.tool_results(vec![ToolOutcome::failed(
            call("c1", "query_bookz"),
            "no table named `bookz`",
        )])
        .unwrap();

        // The loop is still running, and the error is in the transcript.
        assert!(!run.is_done());
        let LlmMessage::ToolResult { content, .. } = run.messages().last().unwrap() else {
            panic!("the last message is the tool result");
        };
        assert_eq!(content, "error: no table named `bookz`");
    }

    #[test]
    fn a_model_that_never_stops_asking_is_stopped_by_the_budget() {
        let mut run = AgentLoop::new(2);
        run.push_user("loop forever").unwrap();
        for step in 1..=2 {
            let Step::CallModel { step: n, .. } = run.next_step() else {
                panic!("step {step} should be a model call");
            };
            assert_eq!(n, step);
            run.model_answered(tool_answer(vec![call(&format!("c{step}"), "read")]))
                .unwrap();
            run.next_step();
            run.tool_results(vec![ToolOutcome::ok(
                call(&format!("c{step}"), "read"),
                &json!("more"),
            )])
            .unwrap();
        }
        assert_eq!(run.next_step(), Step::Done(Conclusion::MaxSteps));
        assert!(run.is_done());
        // The work that was done is still readable — the budget stops the loop,
        // it does not erase the transcript.
        assert_eq!(run.messages().len(), 5);
    }

    #[test]
    fn a_budget_of_zero_is_raised_rather_than_producing_a_run_that_cannot_answer() {
        let mut run = AgentLoop::new(0);
        assert_eq!(run.max_steps(), 1);
        run.push_user("hi").unwrap();
        assert!(matches!(run.next_step(), Step::CallModel { .. }));
    }

    #[test]
    fn asking_twice_without_answering_gives_the_same_instruction() {
        // What resumption depends on: a process that died between being told to
        // act and acting comes back and is told the same thing.
        let mut run = AgentLoop::new(20);
        run.push_user("hello").unwrap();
        assert_eq!(run.next_step(), run.next_step());

        run.model_answered(tool_answer(vec![call("c1", "read")]))
            .unwrap();
        assert_eq!(run.next_step(), run.next_step());
    }

    #[test]
    fn the_whole_state_round_trips_through_json() {
        // `_sc_runs.context` is exactly this value, so a run that cannot survive
        // serialisation is a run that cannot resume.
        let mut run = AgentLoop::new(5);
        run.push_user("how many books?").unwrap();
        run.next_step();
        run.model_answered(AssistantMessage {
            content: "checking".to_owned(),
            tool_calls: vec![call("c1", "query_books")],
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                cached_input_tokens: 0,
            },
            ..AssistantMessage::default()
        })
        .unwrap();

        let text = serde_json::to_string(&run).unwrap();
        let mut back: AgentLoop = serde_json::from_str(&text).unwrap();
        assert_eq!(back, run);
        assert_eq!(back.usage().total_tokens(), 14);
        // And it resumes where it left off: the tools it was about to run.
        assert_eq!(
            back.next_step(),
            Step::CallTools {
                calls: vec![call("c1", "query_books")]
            }
        );
    }

    #[test]
    fn a_finished_run_continues_when_the_person_says_something_else() {
        let mut run = AgentLoop::new(20);
        run.push_user("hello").unwrap();
        run.next_step();
        run.model_answered(text_answer("hi")).unwrap();
        assert!(run.is_done());

        run.push_user("and again").unwrap();
        assert!(!run.is_done());
        let Step::CallModel { messages, step } = run.next_step() else {
            panic!("the second turn is a model call");
        };
        // The budget is the run's, not the turn's, and the history is kept.
        assert_eq!(step, 2);
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn a_message_is_refused_while_tools_are_running() {
        let mut run = AgentLoop::new(20);
        run.push_user("go").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "read")]))
            .unwrap();
        let err = run.push_user("actually, stop").unwrap_err();
        assert!(err.to_string().contains("mid-turn"), "{err}");
    }

    #[test]
    fn aborting_ends_the_run_and_keeps_the_transcript() {
        let mut run = AgentLoop::new(20);
        run.push_user("go").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "read")]))
            .unwrap();
        run.abort();
        assert_eq!(run.next_step(), Step::Done(Conclusion::Aborted));
        assert_eq!(run.messages().len(), 2);

        // A run that already answered is not re-concluded by a late stop.
        let mut answered = AgentLoop::new(20);
        answered.push_user("hi").unwrap();
        answered.next_step();
        answered.model_answered(text_answer("hello")).unwrap();
        answered.abort();
        assert_eq!(answered.conclusion().unwrap().answer(), Some("hello"));
    }

    #[test]
    fn an_answer_the_loop_did_not_ask_for_is_an_internal_error() {
        let mut run = AgentLoop::new(20);
        run.push_user("go").unwrap();
        run.next_step();
        run.model_answered(tool_answer(vec![call("c1", "read")]))
            .unwrap();
        // Now it wants tools, not another model answer.
        assert!(run.model_answered(text_answer("hi")).is_err());
    }
}
