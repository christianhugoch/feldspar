//! The `openai_chat` adapter against a stub Chat Completions host: what the
//! request carries, and a streamed answer coming back.

use crate::common;

use common::{Reply, sse_events};
use sc_llm::openai_chat::OpenAiChat;
use sc_llm::{
    ImagePart, LlmMessage, LlmProvider, LlmRequest, ModelCapabilities, OPENAI_CHAT_BACKEND,
    StopReason, ToolCall, ToolSpec,
};
use serde_json::json;

/// A streamed chunk carrying `content`.
fn chunk(content: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000u64,
        "model": "llama3.2",
        "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}],
    })
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "view_app".to_owned(),
        arguments: json!({"path": "/"}),
    }
}

#[tokio::test]
async fn a_request_maps_onto_chat_completions() {
    let mut body = sse_events(&[chunk("Done"), chunk(".")]);
    body.push_str("data: [DONE]\n\n");
    let (base, mut requests) = common::serve_capturing(Reply::sse(body))
        .await
        .expect("the stub");
    // A vision model, so the image travels; no key, as a local host has none.
    let caps = ModelCapabilities::built_in(OPENAI_CHAT_BACKEND, "qwen2.5-vl-7b");
    let provider =
        OpenAiChat::new(&format!("{base}/v1"), "", "qwen2.5-vl-7b", caps).expect("the adapter");

    let mut req = LlmRequest {
        system: Some("You build apps.".to_owned()),
        messages: vec![
            LlmMessage::user("look at the home page"),
            LlmMessage::assistant_with_calls("", vec![call("c1"), call("c2")]),
            LlmMessage::ToolResult {
                tool_call_id: "c1".to_owned(),
                name: "view_app".to_owned(),
                content: "a snapshot".to_owned(),
                images: vec![ImagePart::new("image/png", b"png-bytes".to_vec())],
            },
            LlmMessage::tool_result(&call("c2"), "second"),
        ],
        tools: vec![ToolSpec::new("view_app", "Look", json!({"type": "object"}))],
        ..LlmRequest::default()
    };
    req.parallel_tool_calls = Some(false);
    req.prompt_cache_key = Some("k".to_owned());

    let msg = provider
        .stream(req)
        .await
        .expect("the stream")
        .collect()
        .await
        .expect("collecting");
    assert_eq!(msg.content, "Done.");
    assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));

    let body = requests.next_body().await.expect("the request");
    assert_eq!(body["model"], "qwen2.5-vl-7b", "{body}");
    assert_eq!(body["parallel_tool_calls"], json!(false), "{body}");
    assert!(body.get("prompt_cache_key").is_none(), "{body}");

    let roles: Vec<&str> = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?"))
        .collect();
    // Each tool result is its own `tool` message, right after the call, and the
    // image follows them in a user message.
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "tool", "user"],
        "{body}"
    );
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages[3]["tool_call_id"], "c1", "{body}");
    assert_eq!(messages[4]["tool_call_id"], "c2", "{body}");
    let image_message = messages[5].to_string();
    assert!(
        image_message.contains("c1"),
        "labelled with its call: {body}"
    );
    assert!(image_message.contains("data:image/png;base64,"), "{body}");
}
