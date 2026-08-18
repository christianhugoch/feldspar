//! The `openai_responses` adapter against a **stub HTTP server** replaying a
//! recorded Responses-API SSE body (TODO Phase 1).
//!
//! What is under test is the whole path an answer takes: rig frames the SSE,
//! assembles the partial-JSON tool arguments, and hands back its own events;
//! `sc-llm` turns those into [`LlmDelta`]s. Asserting on the deltas asserts on
//! all of it, which is the point — a caller only ever sees deltas.
//!
//! The event payloads are the Responses API's real ones, spelled out in full
//! (`sequence_number`, `output_index`, the stringified `arguments`) because rig
//! decodes them strictly and **silently skips** a chunk it cannot deserialise.
//! A fixture that were merely close enough would therefore produce an empty
//! stream and a test that passed for the wrong reason.

mod common;

use common::{Reply, serve, sse_events};
use sc_error::Result;
use sc_llm::openai::OpenAiResponses;
use sc_llm::{LlmDelta, LlmProvider, LlmRequest, StopReason, ToolSpec, Usage};
use serde_json::{Value as Json, json};

/// A `response.output_text.delta` event.
fn text_delta(seq: u64, delta: &str) -> Json {
    json!({
        "type": "response.output_text.delta",
        "output_index": 0,
        "content_index": 0,
        "sequence_number": seq,
        "delta": delta,
    })
}

/// A `response.reasoning_summary_text.delta` event.
fn reasoning_delta(seq: u64, delta: &str) -> Json {
    json!({
        "type": "response.reasoning_summary_text.delta",
        "output_index": 0,
        "summary_index": 0,
        "sequence_number": seq,
        "delta": delta,
    })
}

/// A `function_call` output item, as `output_item.added`/`.done` carry it.
fn function_call_item(arguments: &str, status: &str) -> Json {
    json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "query_books",
        // The Responses API carries tool arguments as a JSON *string*.
        "arguments": arguments,
        "status": status,
    })
}

/// The terminal `response.completed` event, carrying the usage the provider
/// reports.
fn completed(seq: u64, usage_in: u64, usage_out: u64) -> Json {
    json!({
        "type": "response.completed",
        "sequence_number": seq,
        "response": {
            "id": "resp_1",
            "object": "response",
            "created_at": 1_700_000_000u64,
            "status": "completed",
            "error": null,
            "incomplete_details": null,
            "instructions": null,
            "max_output_tokens": null,
            "model": "gpt-5.1",
            "usage": {
                "input_tokens": usage_in,
                "output_tokens": usage_out,
                "total_tokens": usage_in + usage_out,
            },
            "output": [],
            "tools": [],
        }
    })
}

/// Connect an adapter to a stub serving `events`.
async fn provider_for(events: &[Json]) -> Result<OpenAiResponses> {
    let base = serve(Reply::sse(sse_events(events))).await?;
    OpenAiResponses::new(&format!("{base}/v1"), "sk-not-a-real-key", "gpt-5.1")
}

/// Every delta the provider yields, or the first error.
async fn deltas(provider: &OpenAiResponses, req: LlmRequest) -> Result<Vec<LlmDelta>> {
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
        text_delta(1, "The answer"),
        text_delta(2, " is 42."),
        completed(3, 31, 7),
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
async fn a_reasoning_block_is_its_own_delta_not_part_of_the_answer() {
    let provider = provider_for(&[
        reasoning_delta(1, "weighing it up"),
        text_delta(2, "42"),
        completed(3, 1, 1),
    ])
    .await
    .expect("the stub adapter");

    let deltas = deltas(&provider, LlmRequest::prompt("hi"))
        .await
        .expect("the stub's stream");
    assert_eq!(deltas[0], LlmDelta::Reasoning("weighing it up".to_owned()));
    assert_eq!(deltas[1], LlmDelta::Text("42".to_owned()));

    // And it does not reach the assistant message's content, which is what goes
    // back to the model on the next turn.
    let msg = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("a second stream")
        .collect()
        .await
        .expect("collecting");
    assert_eq!(msg.content, "42");
    assert_eq!(msg.reasoning, "weighing it up");
}

#[tokio::test]
async fn a_tool_call_split_across_chunks_is_emitted_once_its_arguments_parse() {
    // The arguments arrive in four fragments, none of which is valid JSON on its
    // own. Nothing may be emitted until they are whole.
    let arg_delta = |seq: u64, delta: &str| {
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_1",
            "output_index": 0,
            "sequence_number": seq,
            "delta": delta,
        })
    };
    let provider = provider_for(&[
        json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "sequence_number": 1,
            "item": function_call_item("", "in_progress"),
        }),
        arg_delta(2, "{\"where\":"),
        arg_delta(3, " {\"author\""),
        arg_delta(4, ": \"Melville\"}"),
        arg_delta(5, ", \"limit\": 5}"),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 6,
            "item": function_call_item(
                "{\"where\": {\"author\": \"Melville\"}, \"limit\": 5}",
                "completed",
            ),
        }),
        completed(7, 120, 25),
    ])
    .await
    .expect("the stub adapter");

    let req = LlmRequest::prompt("how many Melville?").tools([ToolSpec::new(
        "query_books",
        "Query the books table",
        json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    )]);
    let deltas = deltas(&provider, req).await.expect("the stub's stream");

    // Exactly one tool call, whole, and no half-formed one before it.
    let calls: Vec<_> = deltas
        .iter()
        .filter_map(|d| match d {
            LlmDelta::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "got {deltas:?}");
    assert_eq!(calls[0].name, "query_books");
    assert_eq!(
        calls[0].arguments,
        json!({"where": {"author": "Melville"}, "limit": 5})
    );

    // And the turn stopped *to call it*, which is what tells the loop to
    // continue rather than hand back to the user.
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
async fn a_tool_call_keeps_the_call_id_the_next_turn_has_to_send_back() {
    // The Responses API pairs a `function_call` with its `function_call_output`
    // by `call_id`, and **rejects outright** a request whose history holds
    // either without one ("Assistant tool call `call_id` is required for OpenAI
    // Responses API"). The `fc_…` item id beside it is not that id. So a turn
    // that called a tool has to keep the `call_id`, and the turn after it has
    // to put it back on the wire — which is what a run of the admin copilot got
    // wrong on its *second* step, after the first had worked.
    let stream = sse_events(&[
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 1,
            "item": function_call_item("{\"limit\": 3}", "completed"),
        }),
        completed(2, 10, 2),
    ]);
    let (base, mut requests) = common::serve_capturing(Reply::sse(stream))
        .await
        .expect("the stub");
    let provider =
        OpenAiResponses::new(&format!("{base}/v1"), "sk-x", "gpt-5.1").expect("the adapter");

    // First turn: the model asks for a tool.
    let msg = provider
        .stream(LlmRequest::prompt("how many?"))
        .await
        .expect("the stub's stream")
        .collect()
        .await
        .expect("collecting");
    let call = &msg.tool_calls[0];
    // The correlation id, not the item id — those are different values here for
    // exactly the reason this test exists.
    assert_eq!(call.id, "call_1", "the item id `fc_1` is not the call id");
    let _ = requests.next_body().await.expect("the first request");

    // Second turn: the history goes back with the call and its result.
    let req = LlmRequest {
        messages: vec![
            sc_llm::LlmMessage::user("how many?"),
            msg.message(),
            sc_llm::LlmMessage::tool_result(call, "3"),
        ],
        ..LlmRequest::default()
    };
    provider
        .stream(req)
        .await
        .expect("a second stream")
        .collect()
        .await
        .expect("collecting the second turn");

    let body = requests.next_body().await.expect("the second request");
    let input = body["input"].as_array().expect("an input array");
    let call_item = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap_or_else(|| panic!("no function_call in the history: {body}"));
    assert_eq!(call_item["call_id"], "call_1", "{body}");
    let output_item = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap_or_else(|| panic!("no function_call_output in the history: {body}"));
    // The pair has to agree, or the vendor cannot match the result to the call.
    assert_eq!(output_item["call_id"], "call_1", "{body}");
}

#[tokio::test]
async fn collect_reassembles_exactly_what_the_stream_emitted() {
    let provider = provider_for(&[
        text_delta(1, "Looking"),
        text_delta(2, " it up."),
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 3,
            "item": function_call_item("{\"limit\": 3}", "completed"),
        }),
        completed(4, 10, 2),
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

    assert_eq!(msg.content, "Looking it up.");
    assert_eq!(msg.tool_calls.len(), 1);
    assert_eq!(msg.tool_calls[0].arguments, json!({"limit": 3}));
    assert_eq!(msg.stop_reason, Some(StopReason::ToolCalls));
    assert_eq!(msg.usage.total_tokens(), 12);
    // The message that goes back into the history keeps both halves.
    assert!(matches!(
        msg.message(),
        sc_llm::LlmMessage::Assistant { ref tool_calls, .. } if tool_calls.len() == 1
    ));
}

#[tokio::test]
async fn a_rejected_key_surfaces_the_providers_own_words() {
    let base = serve(Reply::error(
        401,
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
    ))
    .await
    .expect("the stub");
    let provider = OpenAiResponses::new(&format!("{base}/v1"), "sk-wrong", "gpt-5.1")
        .expect("building the adapter");

    let err = match provider.stream(LlmRequest::prompt("hi")).await {
        Err(e) => e,
        Ok(stream) => stream
            .collect()
            .await
            .expect_err("a 401 must not read as an empty answer"),
    };
    let text = sc_error::format_causes(&err);
    // The actionable part is the provider's own message, not a category: this is
    // what the admin form's Test connection shows.
    assert!(
        text.contains("Incorrect API key provided") || text.contains("401"),
        "{text}"
    );
}

#[tokio::test]
async fn a_malformed_body_never_becomes_invented_text() {
    let base = serve(Reply::sse("data: this is not json\n\ndata: {oops\n\n"))
        .await
        .expect("the stub");
    let provider =
        OpenAiResponses::new(&format!("{base}/v1"), "sk-x", "gpt-5.1").expect("the adapter");

    let result = provider
        .stream(LlmRequest::prompt("hi"))
        .await
        .expect("the stream starts")
        .collect()
        .await;

    // Unparseable events carry no content. Either outcome is honest — an error,
    // or an empty answer that says it ended — and what must *not* happen is text
    // appearing that the provider never sent.
    match result {
        Ok(msg) => {
            assert_eq!(msg.content, "");
            assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
        }
        Err(e) => {
            let text = sc_error::format_causes(&e);
            assert!(text.contains("provider"), "{text}");
        }
    }
}

#[tokio::test]
async fn a_truncated_stream_is_an_error_rather_than_a_short_answer() {
    // The provider promised more bytes than it sent and hung up mid-answer. A
    // partial answer returned as though it were whole is a wrong answer nobody
    // notices, so this has to fail.
    let base = serve(Reply::sse(sse_events(&[text_delta(1, "The answer is")])).truncated())
        .await
        .expect("the stub");
    let provider =
        OpenAiResponses::new(&format!("{base}/v1"), "sk-x", "gpt-5.1").expect("the adapter");

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
async fn the_model_the_adapter_reports_is_the_one_it_will_call() {
    let provider = provider_for(&[completed(1, 0, 0)])
        .await
        .expect("the stub adapter");
    assert_eq!(provider.model(), "gpt-5.1");
}
