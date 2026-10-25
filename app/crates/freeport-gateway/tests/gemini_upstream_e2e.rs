//! Gemini 原生上游的端到端验证。
//!
//! 盯的是四件事：
//! 1. OpenAI 进来的请求被转成 Gemini 形态（systemInstruction / contents / functionDeclarations），
//!    且 API key 走 `x-goog-api-key`，不是 Bearer
//! 2. Gemini 的响应（含 thought / functionCall / usageMetadata）被转回 OpenAI 形态
//! 3. 流式：Gemini 的 SSE 帧被逐帧转成 OpenAI chunk，收尾带 usage、以 [DONE] 结束
//! 4. 工具结果按函数名回填 functionResponse（OpenAI 的 tool_call_id → Gemini 的函数名）

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
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
        "wire": "gemini",
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
    format!("http://{addr}")
}

#[tokio::test]
async fn gemini_native_upstream_speaks_gemini_both_ways() {
    let dir = std::env::temp_dir().join(format!("fp-gup-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    // 收到的请求存起来，测试尾部统一断言——断言写在 handler 里会把连接打崩
    let seen: Arc<Mutex<Vec<Value>>> = Default::default();
    let sink = seen.clone();
    let upstream = Router::new().route(
        "/v1beta/models/:seg",
        post(move |headers: HeaderMap, uri: axum::http::Uri, Json(body): Json<Value>| {
            let sink = sink.clone();
            async move {
                let streaming = uri.query().map(|q| q.contains("alt=sse")).unwrap_or(false);
                sink.lock().unwrap().push(json!({
                    "path": uri.path(),
                    "key": headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()).unwrap_or(""),
                    "bearer": headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or(""),
                    "body": body,
                }));

                // 带 tools 的请求回 functionCall，其余回纯文本
                if body.get("tools").is_some() {
                    return Json(json!({
                        "responseId": "r3",
                        "candidates": [{
                            "content": { "role": "model", "parts": [
                                { "functionCall": { "name": "read_file", "args": { "path": "/tmp" } } }
                            ]},
                            "finishReason": "STOP"
                        }],
                        "usageMetadata": { "promptTokenCount": 20, "candidatesTokenCount": 4 }
                    }))
                    .into_response();
                }

                if streaming {
                    let sse = concat!(
                        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"po\"}]}}],\"usageMetadata\":{\"promptTokenCount\":7,\"candidatesTokenCount\":1}}\n\n",
                        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ng\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":7,\"candidatesTokenCount\":2,\"thoughtsTokenCount\":3}}\n\n",
                    );
                    return ([("content-type", "text/event-stream")], sse.to_string())
                        .into_response();
                }

                Json(json!({
                    "responseId": "r1",
                    "candidates": [{
                        "content": { "role": "model", "parts": [
                            { "text": "想一想", "thought": true },
                            { "text": "done" }
                        ]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": { "promptTokenCount": 11, "candidatesTokenCount": 5, "thoughtsTokenCount": 2 }
                }))
                .into_response()
            }
        }),
    );
    let base = spawn(upstream).await;

    write_config(&dir, vec![provider("a", format!("{base}/v1beta"))]);
    freeport_gateway::secrets::set_key("a", "test-key").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    /* ── 非流式：OpenAI 进，OpenAI 出 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "max_tokens": 128,
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "done");
    assert_eq!(body["choices"][0]["message"]["reasoning_content"], "想一想");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    // 思考 token 也是真实输出，账本必须认账
    assert_eq!(body["usage"]["prompt_tokens"], 11);
    assert_eq!(body["usage"]["completion_tokens"], 7);

    /* ── 流式：OpenAI chunk 出，[DONE] 收尾 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
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
    assert!(sse.contains("chat.completion.chunk"), "必须是 OpenAI chunk：{sse}");
    assert!(sse.contains("\"content\":\"po\""), "第一段文本丢了：{sse}");
    assert!(sse.contains("\"content\":\"ng\""), "第二段文本丢了：{sse}");
    assert!(sse.contains("\"finish_reason\":\"stop\""), "缺收尾块：{sse}");
    assert!(sse.contains("\"completion_tokens\":5"), "usage 必须含思考 token：{sse}");
    assert!(sse.contains("data: [DONE]"), "必须以 [DONE] 收尾：{sse}");

    /* ── 工具往返：tool_call_id → functionResponse 按函数名回填 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "max_tokens": 64,
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "", "tool_calls": [
                    { "id": "call_x", "type": "function", "function": { "name": "read_file", "arguments": "{\"path\":\"/tmp\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_x", "content": "ok-result" }
            ],
            "tools": [{ "type": "function", "function": { "name": "read_file", "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } } }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(body["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(body["choices"][0]["message"]["tool_calls"][0]["id"], "call_0");

    /* ── 上游实际收到的形态 ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "应当有三次上游调用");

    assert!(
        seen[0]["path"].as_str().unwrap().ends_with("/models/m1:generateContent"),
        "非流式要打到 :generateContent：{}",
        seen[0]["path"]
    );
    assert_eq!(seen[0]["key"], "test-key", "Gemini 原生端点用 x-goog-api-key");
    assert_eq!(seen[0]["bearer"], "", "API key 绝不能同时出现在 Bearer 头里");
    assert_eq!(seen[0]["body"]["systemInstruction"]["parts"][0]["text"], "be brief");
    assert_eq!(seen[0]["body"]["contents"][0]["role"], "user");
    assert_eq!(seen[0]["body"]["generationConfig"]["maxOutputTokens"], 128);
    assert!(seen[0]["body"].get("stream_options").is_none(), "OpenAI 专属字段不能漏给 Gemini");

    assert!(
        seen[1]["path"].as_str().unwrap().contains(":streamGenerateContent"),
        "流式要打到 :streamGenerateContent：{}",
        seen[1]["path"]
    );

    let contents = seen[2]["body"]["contents"].as_array().unwrap();
    let last = &contents[contents.len() - 1];
    assert_eq!(last["role"], "user");
    let fr = &last["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], "read_file", "函数名来自配对的 assistant tool_calls");
    assert_eq!(fr["response"]["result"], "ok-result");
    let decls = &seen[2]["body"]["tools"][0]["functionDeclarations"];
    assert_eq!(decls[0]["name"], "read_file");
    assert_eq!(decls[0]["parameters"]["properties"]["path"]["type"], "STRING", "schema 枚举必须大写");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
