//! What a model call prints, driven through the **production path**: a stored
//! provider definition, [`connect_model`], a stub server replaying a
//! recorded SSE body (§16).
//!
//! The unit tests beside [`LoggedProvider`](sc_llm::LoggedProvider) pin the
//! lines; what can only be checked here is that the lines are *emitted at all*
//! for a provider an admin configured — the wrapping happens inside
//! `connect_model`, so a refactor that returned the bare adapter would leave
//! every other test passing and the log empty.
//!
//! The lines are read back through `sc-log`'s capture sink (its `capture`
//! feature, a dev-dependency), so what is asserted is what was printed rather
//! than what a formatter returned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::{Reply, serve, sse_events};
use sc_error::Result;
use sc_llm::{
    CFG_API_KEY, CFG_BASE_URL, LlmModelDef, LlmProviderDef, LlmRequest, OPENAI_RESPONSES_BACKEND,
    ToolSpec, connect_model,
};
use sc_log::{Verbosity, capture};
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

/// The terminal `response.completed` event.
fn completed(seq: u64) -> Json {
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
            "usage": { "input_tokens": 1200, "output_tokens": 300, "total_tokens": 1500 },
            "output": [],
            "tools": [],
        }
    })
}

/// One request, sent through a provider connected exactly as the server
/// connects one, with the log at `level`. Answers the lines it printed.
async fn logged_call(level: Verbosity) -> Result<Vec<String>> {
    let base = serve(Reply::sse(sse_events(&[
        text_delta(1, "There are"),
        text_delta(2, " 42 books."),
        completed(3),
    ])))
    .await?;

    // The admin's record, as it would be stored — and the same call
    // `sc_agent::driver::connect` makes.
    let def = LlmProviderDef::new("house", OPENAI_RESPONSES_BACKEND)
        .with(CFG_API_KEY, "sk-not-a-real-key")
        .with(CFG_BASE_URL, format!("{base}/v1"));
    let model = LlmModelDef::new(def.id, "gpt-5.1").default_model();
    let provider = connect_model(&def, &model)?.provider;

    let request = LlmRequest::prompt("how many books are there?")
        .system("You are a librarian.")
        .tools([ToolSpec::new(
            "query_books",
            "Query the books table",
            json!({ "type": "object", "properties": { "limit": { "type": "integer" } } }),
        )]);

    // The switches are the process's, so this test holds them for the duration
    // — without it, the test beside this one setting `verbose` would put its
    // arrival line in this one's log.
    let _log = capture::guard(level);
    let answer = provider.stream(request).await?.collect().await?;
    let lines = capture::take();

    assert_eq!(answer.content, "There are 42 books.", "the call itself");
    Ok(lines)
}

/// The requirement, end to end: at `trace`, the **whole request and the whole
/// response** are in the log — the system prompt, the user's message, the tool
/// schema that was offered, and the answer that came back.
#[tokio::test]
async fn trace_logs_the_whole_request_and_the_whole_response() -> Result<()> {
    let log = logged_call(Verbosity::Trace).await?.join("\n");

    // The request, in full.
    assert!(log.contains("llm request to house (gpt-5.1)"), "{log}");
    assert!(log.contains("You are a librarian."), "{log}");
    assert!(log.contains("how many books are there?"), "{log}");
    assert!(log.contains("query_books"), "{log}");
    // Including the tool's parameter schema, which is the half of a request
    // that is never in the transcript anywhere else.
    assert!(log.contains("\"limit\""), "{log}");

    // The response, in full.
    assert!(log.contains("llm response from house (gpt-5.1)"), "{log}");
    assert!(log.contains("There are 42 books."), "{log}");
    Ok(())
}

/// At `info` the call still reports itself — what it was, what it cost, how
/// long it took — without the transcript.
#[tokio::test]
async fn info_logs_one_line_with_the_token_cost_and_no_transcript() -> Result<()> {
    let lines = logged_call(Verbosity::Info).await?;
    assert_eq!(lines.len(), 1, "one line per call: {lines:?}");

    let line = &lines[0];
    assert!(line.contains("llm ← house (gpt-5.1)"), "{line}");
    assert!(line.contains("1 messages"), "{line}");
    assert!(line.contains("1 tools"), "{line}");
    assert!(line.contains("stopped EndTurn"), "{line}");
    assert!(line.contains("1200 in / 300 out tokens"), "{line}");

    // The transcript is what `trace` is for; `info` is a log an installation
    // can leave on, and one that carried every user's messages would not be.
    assert!(!line.contains("how many books are there?"), "{line}");
    assert!(!line.contains("There are 42 books."), "{line}");
    Ok(())
}

/// `verbose` adds the line that says a call has *started* — the one that tells
/// a hanging run from a slow one.
#[tokio::test]
async fn verbose_logs_the_call_as_it_is_issued() -> Result<()> {
    let lines = logged_call(Verbosity::Verbose).await?;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("llm → house"), "{lines:?}");
    assert!(lines[1].starts_with("llm ← house"), "{lines:?}");
    Ok(())
}

/// The default: an installation nobody has turned logging up on says nothing
/// about its model calls, and still gets its answer.
#[tokio::test]
async fn a_quiet_server_logs_no_model_calls() -> Result<()> {
    assert!(logged_call(Verbosity::Warning).await?.is_empty());
    Ok(())
}
