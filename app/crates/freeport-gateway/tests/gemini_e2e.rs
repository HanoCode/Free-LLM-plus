//! `/v1beta` 的端到端验证。
//!
//! 盯的是五件事：
//! 1. `x-goog-api-key` 能当鉴权用（Gemini CLI 就是这么带的）
//! 2. `contents` / `systemInstruction` / `tools` 被摊成 OpenAI 形态
//! 3. 非流式响应包回 `candidates`
//! 4. 流式响应是 Gemini 的 `data:` 帧，且结束帧带 `finishReason`
//! 5. `countTokens` 不打上游

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
async fn beta_endpoint_speaks_gemini_both_ways() {
    let dir = std::env::temp_dir().join(format!("fp-gem-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

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
                            "content": "pong",
                            "tool_calls": [{
                                "id": "call_a",
                                "type": "function",
                                "function": { "name": "list_dir", "arguments": "{\"path\":\"/\"}" }
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
    let root = format!("http://127.0.0.1:{port}");

    /* ── 模型列表：Gemini CLI 启动先拉这个 ── */
    let res = client
        .get(format!("{root}/v1beta/models"))
        .header("x-goog-api-key", "fp-test")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        body["models"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "模型列表不能是空的：{body}"
    );
    assert!(body["models"][0]["name"].as_str().unwrap().starts_with("models/"));

    /* ── 非流式：Gemini 进，Gemini 出 ── */
    let res = client
        .post(format!("{root}/v1beta/models/auto:generateContent"))
        .header("x-goog-api-key", "fp-test")
        .json(&json!({
            "systemInstruction": { "parts": [{ "text": "be brief" }] },
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
            "tools": [{ "functionDeclarations": [{
                "name": "list_dir",
                "description": "l",
                "parameters": { "type": "object", "properties": {} }
            }]}],
            "generationConfig": { "maxOutputTokens": 64, "temperature": 0.3 }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["candidates"][0]["content"]["parts"][0]["text"], "pong");
    assert_eq!(
        body["candidates"][0]["content"]["parts"][1]["functionCall"]["name"],
        "list_dir"
    );
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert_eq!(body["usageMetadata"]["promptTokenCount"], 11);
    assert_eq!(body["usageMetadata"]["totalTokenCount"], 16);

    /* ── 流式：必须是 Gemini 的 data 帧 ── */
    let res = client
        .post(format!("{root}/v1beta/models/auto:streamGenerateContent?alt=sse"))
        .header("x-goog-api-key", "fp-test")
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let sse = res.text().await.unwrap();
    assert!(sse.contains("\"text\":\"po\""), "第一段文本丢了：{sse}");
    assert!(sse.contains("\"text\":\"ng\""), "第二段文本丢了：{sse}");
    assert!(sse.contains("\"finishReason\":\"STOP\""), "缺结束帧：{sse}");
    assert!(sse.contains("\"candidates\""), "不该把 OpenAI 原始块漏出去：{sse}");
    assert!(!sse.contains("chat.completion.chunk"), "漏了 OpenAI 分块：{sse}");

    /* ── countTokens：本地估算，不打上游 ── */
    let before = seen.lock().unwrap().len();
    let res = client
        .post(format!("{root}/v1beta/models/auto:countTokens"))
        .header("x-goog-api-key", "fp-test")
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(body["totalTokens"].as_u64().unwrap() > 0);
    assert_eq!(seen.lock().unwrap().len(), before, "countTokens 不该打上游");

    /* ── 上游实际收到的形态 ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "应当只有两次上游调用");
    assert_eq!(seen[0]["messages"][0]["role"], "system");
    assert_eq!(seen[0]["messages"][0]["content"], "be brief");
    assert_eq!(seen[0]["tools"][0]["function"]["name"], "list_dir");
    assert_eq!(seen[0]["max_tokens"], 64);
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[1]["stream"], true);

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
