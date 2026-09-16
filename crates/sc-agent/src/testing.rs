//! [`FakeProvider`]: a model that says what it was told to say.
//!
//! Behind the `testing` **feature**, not `#[cfg(test)]`, and deliberately
//! (decision 7): `sc-core-traits` tests its traits through a whole run, and
//! `sc-server` tests the chat socket through one, so a fake that existed only
//! inside this crate's own test build is one neither could reach. It is a
//! first-class part of the crate.
//!
//! **No test in this tree may need an API key or spend a token.** This is what
//! makes that possible — every path through the loop, every trait and the socket
//! itself are exercised against a script rather than a vendor.
//!
//! ```
//! # use sc_agent::testing::{FakeProvider, Reply};
//! let provider = FakeProvider::new([
//!     Reply::calls("query_books", serde_json::json!({"limit": 5})),
//!     Reply::says("There are five."),
//! ]);
//! ```

use std::sync::Mutex;

use sc_error::{Error, Result};
use sc_llm::{LlmDelta, LlmProvider, LlmRequest, LlmStream, StopReason, ToolCall, Usage};
use serde_json::Value as Json;

/// One scripted answer.
///
/// Named `Reply` rather than `Turn` to stay out of the way of
/// [`Turn`](crate::Turn), which is the thing a trait changes before a request
/// goes out — a different object at a different moment.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The model says this and ends its turn. Text arrives as several deltas so a
    /// caller that is testing streaming sees more than one.
    Says(String),
    /// The model calls these tools, in this order, saying `preamble` first.
    Calls {
        /// What it says before calling — often empty, which is what both vendors
        /// most often do.
        preamble: String,
        /// The calls, with ids generated in order.
        calls: Vec<(String, Json)>,
    },
    /// The provider fails: a rejected key, a rate limit, a cut connection.
    ///
    /// A first-class scripted outcome rather than something a test has to
    /// simulate, because "the provider refused" is a path the loop and the chat
    /// window both have to handle and neither is exercised by a happy script.
    Fails(String),
}

impl Reply {
    /// A turn that answers with text.
    pub fn says(text: impl Into<String>) -> Reply {
        Reply::Says(text.into())
    }

    /// A turn that calls one tool.
    pub fn calls(tool: impl Into<String>, arguments: Json) -> Reply {
        Reply::Calls {
            preamble: String::new(),
            calls: vec![(tool.into(), arguments)],
        }
    }

    /// A turn that calls several tools at once — the case whose *order* the loop
    /// must preserve.
    pub fn calls_many(calls: impl IntoIterator<Item = (String, Json)>) -> Reply {
        Reply::Calls {
            preamble: String::new(),
            calls: calls.into_iter().collect(),
        }
    }

    /// A turn that fails.
    pub fn fails(error: impl Into<String>) -> Reply {
        Reply::Fails(error.into())
    }

    /// Say something before calling the tools.
    pub fn with_preamble(mut self, text: impl Into<String>) -> Reply {
        if let Reply::Calls { preamble, .. } = &mut self {
            *preamble = text.into();
        }
        self
    }
}

/// A provider that replays a script, and records what it was asked.
///
/// Interior-mutable and `Send + Sync`, because [`LlmProvider::stream`] takes
/// `&self` — which it does because the real adapters hold a connection pool.
pub struct FakeProvider {
    model: String,
    script: Mutex<std::collections::VecDeque<Reply>>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl FakeProvider {
    /// A provider that will answer with these turns, in order.
    pub fn new(script: impl IntoIterator<Item = Reply>) -> FakeProvider {
        FakeProvider {
            model: "fake-model".to_owned(),
            script: Mutex::new(script.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// A provider that answers **every** call the same way — for testing a loop
    /// that has to be stopped by its step budget rather than by the model.
    pub fn repeating(turn: Reply, times: usize) -> FakeProvider {
        FakeProvider::new(std::iter::repeat_n(turn, times))
    }

    /// Report a different model name, for a test that asserts what answered.
    pub fn as_model(mut self, model: impl Into<String>) -> FakeProvider {
        self.model = model.into();
        self
    }

    /// Every request it was sent, in order — which is how a test asserts what the
    /// agent actually offered the model: the system prompt a trait appended to,
    /// the tools, the history.
    pub fn requests(&self) -> Vec<LlmRequest> {
        lock(&self.seen).clone()
    }

    /// The last request it was sent.
    pub fn last_request(&self) -> Option<LlmRequest> {
        self.requests().pop()
    }

    /// How many turns of the script are left unused.
    ///
    /// A test that scripted three turns and used two has learned something: the
    /// loop stopped earlier than it meant to.
    pub fn remaining(&self) -> usize {
        lock(&self.script).len()
    }
}

#[async_trait::async_trait]
impl LlmProvider for FakeProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn stream(&self, req: LlmRequest) -> Result<LlmStream> {
        lock(&self.seen).push(req);
        let reply = lock(&self.script)
            .pop_front()
            // A script that ran out is the test's bug, not the loop's, and saying
            // so beats an empty answer the loop would treat as a real one.
            .ok_or_else(|| Error::msg("the fake provider's script ran out of turns"))?;

        let deltas = match reply {
            Reply::Fails(error) => {
                // Fails where a real provider most often does: after the request
                // was accepted, as an error inside the stream.
                let items: Vec<Result<LlmDelta>> = vec![Err(Error::msg(error))];
                return Ok(LlmStream::new(Box::pin(futures::stream::iter(items))));
            }
            Reply::Says(text) => {
                let mut deltas: Vec<LlmDelta> = split(&text)
                    .into_iter()
                    .map(|part| LlmDelta::Text(part.to_owned()))
                    .collect();
                deltas.push(LlmDelta::Stop {
                    reason: StopReason::EndTurn,
                    usage: usage(),
                });
                deltas
            }
            Reply::Calls { preamble, calls } => {
                let mut deltas: Vec<LlmDelta> = Vec::new();
                if !preamble.is_empty() {
                    deltas.push(LlmDelta::Text(preamble));
                }
                for (i, (name, arguments)) in calls.into_iter().enumerate() {
                    deltas.push(LlmDelta::ToolCall(ToolCall {
                        id: format!("fake_call_{}", i + 1),
                        name,
                        arguments,
                    }));
                }
                deltas.push(LlmDelta::Stop {
                    reason: StopReason::ToolCalls,
                    usage: usage(),
                });
                deltas
            }
        };
        Ok(LlmStream::from_deltas(deltas))
    }
}

/// A lock that survives a poisoned mutex.
///
/// A test that panicked while holding one has already failed; refusing to answer
/// the *next* test because of it would turn one failure into a cascade nobody can
/// read.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Text as more than one delta, so a streaming caller sees a stream.
fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mid = text.len() / 2;
    // Never split inside a character.
    let mid = (mid..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    if mid == 0 || mid == text.len() {
        vec![text]
    } else {
        vec![&text[..mid], &text[mid..]]
    }
}

/// Plausible, non-zero usage — so a test asserting that a run accumulates it has
/// something to accumulate.
fn usage() -> Usage {
    Usage {
        input_tokens: 10,
        output_tokens: 5,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_scripted_turn_streams_text_and_stops() {
        let provider = FakeProvider::new([Reply::says("There are five.")]);
        let answer = provider
            .stream(LlmRequest::prompt("how many?"))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(answer.content, "There are five.");
        assert_eq!(answer.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(answer.usage.total_tokens(), 15);
        assert_eq!(provider.remaining(), 0);
        assert_eq!(provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_scripted_tool_call_arrives_whole_and_in_order() {
        let provider = FakeProvider::new([Reply::calls_many([
            ("read".to_owned(), json!({"path": "a"})),
            ("write".to_owned(), json!({"path": "b"})),
        ])
        .with_preamble("Doing both.")]);
        let answer = provider
            .stream(LlmRequest::prompt("go"))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        assert_eq!(answer.content, "Doing both.");
        assert_eq!(answer.stop_reason, Some(StopReason::ToolCalls));
        let names: Vec<&str> = answer.tool_calls.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["read", "write"]);
        assert_ne!(answer.tool_calls[0].id, answer.tool_calls[1].id);
    }

    #[tokio::test]
    async fn a_scripted_failure_arrives_inside_the_stream() {
        let provider = FakeProvider::new([Reply::fails("401 invalid x-api-key")]);
        let err = provider
            .stream(LlmRequest::prompt("hi"))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");
    }

    #[tokio::test]
    async fn an_exhausted_script_says_so() {
        let provider = FakeProvider::new([]);
        let err = match provider.stream(LlmRequest::prompt("hi")).await {
            Err(e) => e,
            Ok(_) => panic!("an exhausted script must not answer"),
        };
        assert!(err.to_string().contains("script"), "{err}");
    }
}
