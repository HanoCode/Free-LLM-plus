//! `/v1/messages` 的端到端验证。
//!
//! 盯的是四件事：
//! 1. 请求确实被转成了 OpenAI 形态（上游收到的 body 能看出来）
//! 2. 非流式响应被转回 Anthropic Message
//! 3. 流式响应逐块转成 Anthropic 事件（message_start → content_block_delta → message_stop）
//! 4. 原始 OpenAI 分块不会漏给客户端

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, providers: Vec<Value>) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": providers,
            "routing": { "chain": ["a:m1"] }
        })
        .to_string(),
    )
    .unwrap();
}

fn provider(id: &str, base: String) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
        "models": [{ "id": "m1", "ownedBy": id }],
        "health": { "status": "unknown" }
    })
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

const SSE_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"po\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"ng\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn messages_endpoint_speaks_anthropic_both_ways() {
    let dir = std::env::temp_dir().join(format!("fp-msg-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    // 收到的请求体存起来，测试尾部统一断言——断言写在 handler 里会把连接打崩，
    // 网关只会看到 5xx，反而掩盖真正的问题。
    let seen: Arc<Mutex<Vec<Value>>> = Default::default();
    let sink = seen.clone();
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let sink = sink.clone();
            async move {
                let streaming = body["stream"].as_bool().unwrap_or(false);
                sink.lock().unwrap().push(body);

                if streaming {
                    return ([("content-type", "text/event-stream")], SSE_BODY.to_string())
                        .into_response();
                }

                Json(json!({
                    "id": "cmpl-1",
                    "model": "m1",
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "done",
                            "tool_calls": [{
                                "id": "call_a",
                                "type": "function",
                                "function": { "name": "read_file", "arguments": "{\"path\":\"/tmp\"}" }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 }
                }))
                .into_response()
            }
        }),
    );
    let base = spawn(upstream).await;

    write_config(&dir, vec![provider("a", base)]);
    freeport_gateway::secrets::set_key("a", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    /* ── 非流式：Anthropic 进，Anthropic 出 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", "anything") // Claude Code 用 x-api-key
        .json(&json!({
            "model": "auto",
            "max_tokens": 128,
            "system": "be brief",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "read_file", "description": "r", "input_schema": { "type": "object" } }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "done");
    assert_eq!(body["content"][1]["type"], "tool_use");
    assert_eq!(body["content"][1]["input"]["path"], "/tmp");
    assert_eq!(body["stop_reason"], "tool_use");
    assert_eq!(body["usage"]["input_tokens"], 11);

    /* ── 流式：必须是 Anthropic 事件，不是 OpenAI chunk ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "auto",
            "max_tokens": 64,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let sse = res.text().await.unwrap();
    assert!(sse.contains("event: message_start"), "缺 message_start：{sse}");
    assert!(sse.contains("event: content_block_delta"), "缺 content_block_delta：{sse}");
    assert!(sse.contains("\"text_delta\""), "增量必须是 text_delta：{sse}");
    assert!(sse.contains("\"text\":\"po\""), "第一段文本丢了：{sse}");
    assert!(sse.contains("\"text\":\"ng\""), "第二段文本丢了：{sse}");
    assert!(sse.contains("event: message_stop"), "缺 message_stop：{sse}");
    assert!(sse.contains("\"stop_reason\":\"end_turn\""), "缺 stop_reason：{sse}");
    assert!(!sse.contains("chat.completion.chunk"), "不该把 OpenAI 原始块漏出去");

    /* ── 签名往返：客户端带回来的 thought_signature 必须还给上游 ── */
    let sig = freeport_gateway::sig::seal(
        "gemini",
        "m1",
        &[json!({ "google": { "thought_signature": "SIG-E2E" } })],
    );
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .header("x-api-key", "anything")
        .json(&json!({
            "model": "auto",
            "max_tokens": 64,
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "想一下", "signature": sig },
                    { "type": "tool_use", "id": "call_a", "name": "read_file", "input": { "path": "/tmp" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "call_a", "content": "ok" }
                ]}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    /* ── 上游实际收到的形态 ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "应当有三次上游调用");
    assert_eq!(seen[0]["messages"][0]["role"], "system", "system 必须被搬进 messages");
    assert_eq!(seen[0]["messages"][0]["content"], "be brief");
    assert_eq!(seen[0]["tools"][0]["function"]["name"], "read_file");
    assert!(seen[0]["max_tokens"].is_number(), "max_tokens 要透传");
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[1]["stream"], true);
    // 少了这个字段，Gemini 3 的二次请求直接 400，而且照样烧额度
    assert_eq!(
        seen[2]["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "SIG-E2E"
    );
    assert!(
        seen[2]["messages"][1].get("content").is_none(),
        "思考文本不该被当成正文塞给上游"
    );

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
