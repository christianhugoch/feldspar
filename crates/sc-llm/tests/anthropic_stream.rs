//! The `anthropic` adapter against a **stub HTTP server** replaying a recorded
//! messages-API SSE body — the twin of `openai_stream.rs`, and deliberately
//! asserting the same things.
//!
//! Asserting the same things is the point. The two vendors frame a streamed tool
//! call quite differently — Anthropic sends `content_block_start` with a
//! `tool_use` block and then `input_json_delta` fragments; OpenAI sends output
//! items and `function_call_arguments.delta` — and the whole claim `sc-llm`
//! makes is that a caller cannot tell. Two test files that check different
//! properties would leave that claim untested.

mod common;

use common::{Reply, serve, sse_events};
use sc_error::Result;
use sc_llm::anthropic::Anthropic;
use sc_llm::{LlmDelta, LlmProvider, LlmRequest, StopReason, ToolSpec, Usage};
use serde_json::{Value as Json, json};

/// The `message_start` event that opens every Anthropic response, carrying the
/// input-token count.
fn message_start(input_tokens: u64) -> Json {
    json!({
        "type": "message_start",
        "message": {
            "id": "msg_1",
            "role": "assistant",
            "content": [],
            "model": "claude-sonnet-4-5",
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": 0,
                "cache_read_input_tokens": null,
                "cache_creation_input_tokens": null,
            },
        }
    })
}

/// A `text_delta` inside content block `index`.
fn text_delta(index: usize, text: &str) -> Json {
    json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "text_delta", "text": text},
    })
}

/// A `thinking_delta` — Anthropic's extended thinking, which is reasoning and
/// not the answer.
fn thinking_delta(index: usize, thinking: &str) -> Json {
    json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "thinking_delta", "thinking": thinking},
    })
}

/// A `content_block_start` opening a text block.
fn text_block_start(index: usize) -> Json {
    json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {"type": "text", "text": ""},
    })
}

/// A `content_block_stop`.
fn block_stop(index: usize) -> Json {
    json!({"type": "content_block_stop", "index": index})
}

/// The `message_delta` that reports the stop reason and the output tokens —
/// what ends the response.
fn message_delta(output_tokens: u64) -> Json {
    json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn", "stop_sequence": null},
        "usage": {"output_tokens": output_tokens},
    })
}

/// Connect an adapter to a stub serving `events`.
async fn provider_for(events: &[Json]) -> Result<Anthropic> {
    let base = serve(Reply::sse(sse_events(events))).await?;
    Anthropic::new(&base, "sk-ant-not-a-real-key", "claude-sonnet-4-5")
}

/// Every delta the provider yields, or the first error.
async fn deltas(provider: &Anthropic, req: LlmRequest) -> Result<Vec<LlmDelta>> {
    let mut stream = provider.stream(req).await?;
    let mut out = Vec::new();
    while let Some(delta) = stream.next().await {
        out.push(delta?);
    }
    Ok(out)
}

#[tokio::test]
async fn text_deltas_arrive_in_order_and_end_in_a_stop_with_usage() {
    let provider = provider_for(&[
        message_start(31),
        text_block_start(0),
        text_delta(0, "The answer"),
        text_delta(0, " is 42."),
        block_stop(0),
        message_delta(7),
    ])
    .await
    .expect("the stub adapter");

    let deltas = deltas(&provider, LlmRequest::prompt("what is it?"))
        .await
        .expect("the stub's stream");

    assert_eq!(
        deltas,
        [
            LlmDelta::Text("The answer".to_owned()),
            LlmDelta::Text(" is 42.".to_owned()),
            LlmDelta::Stop {
                reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 31,
                    output_tokens: 7,
                    cached_input_tokens: 0,
                },
            },
        ]
    );
}

#[tokio::test]
async fn extended_thinking_is_reasoning_not_the_answer() {
    let provider = provider_for(&[
        message_start(5),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": ""},
        }),
        thinking_delta(0, "weighing it up"),
        block_stop(0),
        text_block_start(1),
        text_delta(1, "42"),
        block_stop(1),
        message_delta(3),
    ])
    .await
    .expect("the stub adapter");

    let msg = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("the stub's stream")
        .collect()
        .await
        .expect("collecting");

    assert_eq!(msg.content, "42");
    assert!(
        msg.reasoning.contains("weighing it up"),
        "{:?}",
        msg.reasoning
    );
    // What goes back to the model on the next turn is the answer alone.
    assert_eq!(msg.message(), sc_llm::LlmMessage::assistant("42"));
}

#[tokio::test]
async fn a_tool_call_split_across_chunks_is_emitted_once_its_arguments_parse() {
    // Anthropic streams tool input as `input_json_delta` fragments. None of
    // these four is valid JSON alone.
    let json_delta = |index: usize, partial: &str| {
        json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": partial},
        })
    };
    let provider = provider_for(&[
        message_start(120),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "query_books",
                "input": {},
            },
        }),
        json_delta(0, "{\"where\":"),
        json_delta(0, " {\"author\""),
        json_delta(0, ": \"Melville\"}"),
        json_delta(0, ", \"limit\": 5}"),
        block_stop(0),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use", "stop_sequence": null},
            "usage": {"output_tokens": 25},
        }),
    ])
    .await
    .expect("the stub adapter");

    let req = LlmRequest::prompt("how many Melville?").tools([ToolSpec::new(
        "query_books",
        "Query the books table",
        json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    )]);
    let deltas = deltas(&provider, req).await.expect("the stub's stream");

    let calls: Vec<_> = deltas
        .iter()
        .filter_map(|d| match d {
            LlmDelta::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "got {deltas:?}");
    assert_eq!(calls[0].id, "toolu_1");
    assert_eq!(calls[0].name, "query_books");
    assert_eq!(
        calls[0].arguments,
        json!({"where": {"author": "Melville"}, "limit": 5})
    );

    assert_eq!(
        deltas.last(),
        Some(&LlmDelta::Stop {
            reason: StopReason::ToolCalls,
            usage: Usage {
                input_tokens: 120,
                output_tokens: 25,
                cached_input_tokens: 0,
            },
        })
    );
}

#[tokio::test]
async fn cached_input_tokens_are_reported_where_the_provider_sends_them() {
    // Prompt caching is not used yet (it is carried past this milestone), but
    // what the provider reports must not be dropped on the way through — the
    // number is what any later decision about caching will be measured against.
    let provider = provider_for(&[
        message_start(10),
        text_block_start(0),
        text_delta(0, "hi"),
        block_stop(0),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {
                "output_tokens": 2,
                "cache_read_input_tokens": 900,
                "cache_creation_input_tokens": 0,
            },
        }),
    ])
    .await
    .expect("the stub adapter");

    let msg = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("the stub's stream")
        .collect()
        .await
        .expect("collecting");
    assert_eq!(msg.usage.cached_input_tokens, 900);
}

#[tokio::test]
async fn a_rejected_key_surfaces_the_providers_own_words() {
    let base = serve(Reply::error(
        401,
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    ))
    .await
    .expect("the stub");
    let provider = Anthropic::new(&base, "sk-ant-wrong", "claude-sonnet-4-5").expect("the adapter");

    let err = match provider.stream(LlmRequest::prompt("hi")).await {
        Err(e) => e,
        Ok(stream) => stream
            .collect()
            .await
            .expect_err("a 401 must not read as an empty answer"),
    };
    let text = sc_error::format_causes(&err);
    assert!(
        text.contains("invalid x-api-key") || text.contains("401"),
        "{text}"
    );
}

#[tokio::test]
async fn a_malformed_event_is_an_error_rather_than_a_silent_gap() {
    let base = serve(Reply::sse("data: {not json at all\n\n"))
        .await
        .expect("the stub");
    let provider = Anthropic::new(&base, "sk-ant-x", "claude-sonnet-4-5").expect("the adapter");

    let result = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("the stream starts")
        .collect()
        .await;

    let err = result.expect_err("an undecodable event must not read as an answer");
    let text = sc_error::format_causes(&err);
    assert!(text.contains("provider"), "{text}");
}

#[tokio::test]
async fn a_truncated_stream_is_an_error_rather_than_a_short_answer() {
    let base = serve(
        Reply::sse(sse_events(&[
            message_start(1),
            text_block_start(0),
            text_delta(0, "The answer is"),
        ]))
        .truncated(),
    )
    .await
    .expect("the stub");
    let provider = Anthropic::new(&base, "sk-ant-x", "claude-sonnet-4-5").expect("the adapter");

    let result = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("the stream starts")
        .collect()
        .await;
    let err = result.expect_err("a cut-off response must not collect cleanly");
    let text = sc_error::format_causes(&err);
    assert!(text.contains("provider"), "{text}");
}

#[tokio::test]
async fn consecutive_tool_results_reach_the_wire_as_one_user_message() {
    // Anthropic requires alternating roles, so the two tool results this history
    // holds have to reach the wire as **one** user message. rig does no merging,
    // so this is `sc-llm`'s to get right — and getting it wrong is a request the
    // vendor rejects outright, which no assertion on our own types would catch.
    // Hence the capturing stub: what is checked here is the body that was sent.
    let (base, mut requests) = common::serve_capturing(Reply::sse(sse_events(&[
        message_start(200),
        text_block_start(0),
        text_delta(0, "Three."),
        block_stop(0),
        message_delta(4),
    ])))
    .await
    .expect("the stub");
    let provider = Anthropic::new(&base, "sk-ant-x", "claude-sonnet-4-5").expect("the adapter");

    let call = |id: &str, name: &str| sc_llm::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: json!({}),
    };
    let req = LlmRequest {
        system: Some("You answer questions about books.".to_owned()),
        messages: vec![
            sc_llm::LlmMessage::user("how many?"),
            sc_llm::LlmMessage::Assistant {
                content: "Let me check.".to_owned(),
                tool_calls: vec![call("t1", "count_books"), call("t2", "count_authors")],
            },
            sc_llm::LlmMessage::tool_result(&call("t1", "count_books"), "3"),
            sc_llm::LlmMessage::tool_result(&call("t2", "count_authors"), "2"),
            sc_llm::LlmMessage::user("and how many by Melville?"),
        ],
        ..LlmRequest::default()
    };

    let msg = provider
        .stream(req)
        .await
        .expect("the stub's stream")
        .collect()
        .await
        .expect("collecting");
    assert_eq!(msg.content, "Three.");

    let body = requests.next_body().await.expect("the captured request");
    let messages = body["messages"].as_array().expect("a messages array");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        roles,
        ["user", "assistant", "user", "user"],
        "roles must alternate apart from the trailing prompt: {body}"
    );

    // The merged message carries *both* results, in order.
    let merged = messages[2]["content"].as_array().expect("content blocks");
    assert_eq!(merged.len(), 2, "{body}");
    assert_eq!(merged[0]["type"], "tool_result");
    assert_eq!(merged[0]["tool_use_id"], "t1");
    assert_eq!(merged[1]["tool_use_id"], "t2");

    // And the system prompt travels as the request's own field, not as a
    // message in the history. Anthropic accepts it as either a string or a list
    // of text blocks, and rig sends the latter — which is why this asserts on
    // the text rather than on the shape.
    assert!(
        body["system"]
            .to_string()
            .contains("You answer questions about books."),
        "the system prompt must be the request's own field: {body}"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m["role"] == "system" || m["role"] == "preamble"),
        "and never a message: {body}"
    );
}

#[tokio::test]
async fn the_model_the_adapter_reports_is_the_one_it_will_call() {
    let provider = provider_for(&[message_start(0), message_delta(0)])
        .await
        .expect("the stub adapter");
    assert_eq!(provider.model(), "claude-sonnet-4-5");
}
