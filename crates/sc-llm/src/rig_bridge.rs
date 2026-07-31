//! The translation between this crate's vocabulary and `rig-core`'s — the only
//! module in the tree, besides the two thin adapters that use it, which names a
//! rig type.
//!
//! Both adapters share it because both providers reach rig through the *same*
//! generic `CompletionModel` interface: what differs between OpenAI's Responses
//! API and Anthropic's messages API is entirely inside rig, and by the time a
//! request or a stream reaches here the two are the same shape. Writing this
//! once is the point of decision 3 (streaming is the only shape) — the tool-call
//! assembly that providers do differently is assembled by rig, checked here, and
//! tested once.
//!
//! ## What this module is responsible for
//!
//! - **Merging consecutive tool results into one message.** Anthropic requires
//!   alternating roles, and rig does no merging: three tool results emitted as
//!   three user messages is a request Anthropic rejects outright. Our
//!   [`LlmMessage::ToolResult`] is one result, because that is the shape a loop
//!   produces them in, so the run of them is folded here.
//! - **Only emitting a tool call whose arguments parse.** rig assembles the
//!   partial JSON both providers stream, but it hands back whatever it
//!   assembled: a string that was never valid JSON arrives as a `Value::String`.
//!   A tool call with unparsed arguments would reach a trait's `call` as a
//!   string where an object was declared, so it is repaired here (a JSON string
//!   is parsed) or dropped with an error (a fragment that never parsed).
//! - **Deriving the stop reason**, which neither provider's streaming response
//!   carries through rig 0.41. See [`StopReason`].

use std::collections::BTreeMap;

use futures::StreamExt;
use rig_core::completion::message::{
    AssistantContent, Message, Text, ToolCall as RigToolCall, ToolFunction, ToolResult,
    ToolResultContent, UserContent,
};
use rig_core::completion::request::{CompletionRequest, GetTokenUsage, ToolDefinition};
use rig_core::one_or_many::OneOrMany;
use rig_core::streaming::{
    StreamedAssistantContent, StreamingCompletionResponse, ToolCallDeltaContent,
};
use sc_error::{Error, Result};
use serde_json::Value as Json;

use crate::message::{LlmDelta, LlmMessage, LlmRequest, StopReason, ToolCall, Usage};
use crate::provider::DeltaStream;

/// Build the rig request one [`LlmRequest`] describes.
///
/// Fails only when the conversation is empty: rig's `chat_history` is a
/// `OneOrMany`, so "at least one message" is its type, and a request with
/// nothing to answer is a caller's bug rather than something to invent a message
/// for.
pub(crate) fn to_rig_request(req: LlmRequest) -> Result<CompletionRequest> {
    let messages = to_rig_messages(req.messages);
    let chat_history = OneOrMany::many(messages)
        .map_err(|_| Error::invalid("an LLM request needs at least one message"))?;

    Ok(CompletionRequest {
        model: None,
        preamble: req.system,
        chat_history,
        documents: Vec::new(),
        tools: req
            .tools
            .into_iter()
            .map(|tool| ToolDefinition {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
            })
            .collect(),
        temperature: req.temperature,
        max_tokens: req.max_tokens.map(u64::from),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
        // Local observability policy, never part of the provider payload. Off:
        // switching it on would put prompts, tool arguments and model output on
        // OpenTelemetry spans, and a chat with an agent is exactly the content
        // §11 keeps inside the caller's own authority.
        record_telemetry_content: false,
    })
}

/// Our messages as rig's, folding each run of consecutive tool results into one
/// user message (see the module docs for why).
fn to_rig_messages(messages: Vec<LlmMessage>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    // The tool results seen since the last non-tool-result message.
    let mut pending: Vec<UserContent> = Vec::new();

    for message in messages {
        match message {
            LlmMessage::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                pending.push(UserContent::ToolResult(ToolResult {
                    id: tool_call_id,
                    call_id: None,
                    content: OneOrMany::one(ToolResultContent::Text(Text::from(content))),
                }));
                continue;
            }
            other => {
                flush_tool_results(&mut pending, &mut out);
                out.push(match other {
                    LlmMessage::User { content } => Message::User {
                        content: OneOrMany::one(UserContent::Text(Text::from(content))),
                    },
                    LlmMessage::Assistant {
                        content,
                        tool_calls,
                    } => Message::Assistant {
                        id: None,
                        content: assistant_content(content, tool_calls),
                    },
                    // Handled above; the compiler cannot see that.
                    LlmMessage::ToolResult { .. } => continue,
                });
            }
        }
    }
    flush_tool_results(&mut pending, &mut out);
    out
}

/// Emit the accumulated tool results as one user message, if there are any.
fn flush_tool_results(pending: &mut Vec<UserContent>, out: &mut Vec<Message>) {
    if let Ok(content) = OneOrMany::many(std::mem::take(pending)) {
        out.push(Message::User { content });
    }
}

/// An assistant turn's content: its text (dropped when empty — a turn that was
/// only tool calls must not carry an empty text block, which Anthropic rejects)
/// followed by its tool calls in order.
fn assistant_content(content: String, tool_calls: Vec<ToolCall>) -> OneOrMany<AssistantContent> {
    let mut items: Vec<AssistantContent> = Vec::with_capacity(tool_calls.len() + 1);
    if !content.is_empty() {
        items.push(AssistantContent::Text(Text::from(content)));
    }
    for call in tool_calls {
        items.push(AssistantContent::ToolCall(RigToolCall {
            id: call.id,
            call_id: None,
            function: ToolFunction {
                name: call.name,
                arguments: call.arguments,
            },
            signature: None,
            additional_params: None,
        }));
    }
    OneOrMany::many(items)
        .unwrap_or_else(|_| OneOrMany::one(AssistantContent::Text(Text::from(""))))
}

/// Turn rig's stream into ours: [`LlmDelta`]s ending in exactly one
/// [`Stop`](LlmDelta::Stop).
///
/// The `Stop` is synthesised at the end of the stream rather than forwarded from
/// a provider event, because neither provider's streaming response carries a
/// finish reason through rig 0.41. Usage comes from the final response object
/// where the provider sent one, and is zero where it did not — which is what
/// [`Usage`] documents zero to mean.
pub(crate) fn map_stream<R>(mut response: StreamingCompletionResponse<R>) -> DeltaStream
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
{
    // Tool-call fragments, keyed by rig's internal call id: a name arrives in
    // one event and the arguments across several. This is the fallback path —
    // both providers do emit a complete `ToolCall` at the end of a function-call
    // item — and it is what keeps a provider that only ever streams deltas from
    // silently dropping every tool the model asked for.
    let mut partial: BTreeMap<String, PartialToolCall> = BTreeMap::new();
    let mut emitted: Vec<String> = Vec::new();
    let mut usage = Usage::default();
    let mut saw_tool_call = false;
    // One rig event can produce several of ours (the tail produces a tool call
    // per unfinished fragment plus the `Stop`), and `poll_next` returns one at a
    // time, so what is ready but not yet handed out waits here.
    let mut queued: std::collections::VecDeque<Result<LlmDelta>> =
        std::collections::VecDeque::new();
    let mut ended = false;

    Box::pin(futures::stream::poll_fn(move |cx| {
        loop {
            if let Some(item) = queued.pop_front() {
                return std::task::Poll::Ready(Some(item));
            }
            if ended {
                return std::task::Poll::Ready(None);
            }
            match response.poll_next_unpin(cx) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(Some(item)) => match item {
                    Err(e) => queued.push_back(Err(provider_error(&e))),
                    Ok(StreamedAssistantContent::Text(text)) => {
                        if !text.text.is_empty() {
                            queued.push_back(Ok(LlmDelta::Text(text.text)));
                        }
                    }
                    Ok(StreamedAssistantContent::ReasoningDelta { reasoning, .. }) => {
                        if !reasoning.is_empty() {
                            queued.push_back(Ok(LlmDelta::Reasoning(reasoning)));
                        }
                    }
                    Ok(StreamedAssistantContent::Reasoning(reasoning)) => {
                        let text = reasoning_text(&reasoning);
                        if !text.is_empty() {
                            queued.push_back(Ok(LlmDelta::Reasoning(text)));
                        }
                    }
                    Ok(StreamedAssistantContent::ToolCallDelta {
                        id,
                        internal_call_id,
                        content,
                    }) => {
                        let entry = partial.entry(internal_call_id).or_default();
                        if !id.is_empty() {
                            entry.id = id;
                        }
                        match content {
                            ToolCallDeltaContent::Name(name) => entry.name.push_str(&name),
                            ToolCallDeltaContent::Delta(delta) => entry.arguments.push_str(&delta),
                        }
                    }
                    Ok(StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    }) => {
                        // rig has assembled it; the fragments for this call are
                        // now redundant, whatever state they are in.
                        partial.remove(&internal_call_id);
                        match complete_tool_call(&tool_call) {
                            Ok(call) => {
                                saw_tool_call = true;
                                emitted.push(call.id.clone());
                                queued.push_back(Ok(LlmDelta::ToolCall(call)));
                            }
                            Err(e) => queued.push_back(Err(e)),
                        }
                    }
                    Ok(StreamedAssistantContent::Final(final_response)) => {
                        let reported = final_response.token_usage();
                        usage = Usage {
                            input_tokens: reported.input_tokens,
                            output_tokens: reported.output_tokens,
                            cached_input_tokens: reported.cached_input_tokens,
                        };
                    }
                    // A provider-native item rig does not model — a hosted web
                    // search, a code-interpreter call. Nothing here can act on
                    // one, and passing it through as text would put provider
                    // internals in the answer.
                    Ok(StreamedAssistantContent::Unknown(_)) => {}
                },
                std::task::Poll::Ready(None) => {
                    ended = true;
                    // The tail: anything rig never completed, then exactly one
                    // `Stop`.
                    for (_, fragment) in std::mem::take(&mut partial) {
                        if emitted.contains(&fragment.id) {
                            continue;
                        }
                        match fragment.finish() {
                            Ok(call) => {
                                saw_tool_call = true;
                                queued.push_back(Ok(LlmDelta::ToolCall(call)));
                            }
                            Err(e) => queued.push_back(Err(e)),
                        }
                    }
                    queued.push_back(Ok(LlmDelta::Stop {
                        reason: if saw_tool_call {
                            StopReason::ToolCalls
                        } else {
                            StopReason::EndTurn
                        },
                        usage,
                    }));
                }
            }
        }
    }))
}

/// A tool call rig streamed in fragments: the provider's id, the name, and the
/// partial JSON arguments as text.
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl PartialToolCall {
    /// The assembled call, or an error naming what did not parse.
    ///
    /// An unparseable fragment is an **error**, not a call with empty
    /// arguments: running a tool with arguments the model did not ask for is a
    /// worse outcome than telling it the call was malformed.
    fn finish(self) -> Result<ToolCall> {
        if self.name.is_empty() {
            return Err(Error::msg(
                "the provider streamed tool-call arguments without a tool name",
            ));
        }
        let arguments = if self.arguments.trim().is_empty() {
            Json::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&self.arguments).map_err(|e| {
                Error::msg(format!(
                    "the provider's arguments for tool `{}` are not valid JSON: {e}",
                    self.name
                ))
            })?
        };
        Ok(ToolCall {
            id: self.id,
            name: self.name,
            arguments,
        })
    }
}

/// One of rig's assembled tool calls as ours, repairing the shape providers
/// disagree about: arguments that arrive as a JSON *string* rather than an
/// object are parsed, and a null becomes the empty object a no-argument tool
/// declares.
fn complete_tool_call(call: &RigToolCall) -> Result<ToolCall> {
    let arguments = match &call.function.arguments {
        Json::Null => Json::Object(serde_json::Map::new()),
        Json::String(text) if text.trim().is_empty() => Json::Object(serde_json::Map::new()),
        Json::String(text) => serde_json::from_str(text).map_err(|e| {
            Error::msg(format!(
                "the provider's arguments for tool `{}` are not valid JSON: {e}",
                call.function.name
            ))
        })?,
        other => other.clone(),
    };
    Ok(ToolCall {
        id: call.id.clone(),
        name: call.function.name.clone(),
        arguments,
    })
}

/// The readable half of a structured reasoning block. An encrypted or redacted
/// block yields nothing: there is no text in it to show.
fn reasoning_text(reasoning: &rig_core::completion::message::Reasoning) -> String {
    use rig_core::completion::message::ReasoningContent;
    reasoning
        .content
        .iter()
        .filter_map(|content| match content {
            ReasoningContent::Text { text, .. } => Some(text.clone()),
            ReasoningContent::Summary(text) => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// A rig error as ours, keeping the provider's own words.
///
/// The provider's message is the actionable part — "invalid x-api-key",
/// "model not found", "context length exceeded" — so it is preserved verbatim
/// rather than replaced by a category. That is also what the admin UI's *Test
/// connection* shows, which is the point of having it (§11.1).
pub(crate) fn provider_error(e: &rig_core::completion::CompletionError) -> Error {
    Error::msg(format!("the LLM provider failed: {e}"))
}
