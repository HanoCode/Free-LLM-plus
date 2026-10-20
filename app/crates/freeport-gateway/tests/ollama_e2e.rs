//! `/api/*`（Ollama 兼容）的端到端验证。
//!
//! 盯的是四件事：
//! 1. `/api/tags` 能列出模型（Zed 靠它建下拉框）
//! 2. `/api/chat` 非流式包回 `message` + `done`
//! 3. 流式是 **NDJSON**，一行一个 JSON，最后一行 `done: true`
//! 4. `/api/generate` 的 prompt/system 被拼成对话

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
    "data: {\"choices\":[{\"delta\":{\"content\":\"po\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"ng\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn ollama_endpoints_speak_ndjson() {
    let dir = std::env::temp_dir().join(format!("fp-oll-{}", uuid::Uuid::new_v4().simple()));
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
                    "choices": [{
                        "message": { "role": "assistant", "content": "pong" },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 }
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

    /* ── /api/tags ── */
    let res = client.get(format!("{root}/api/tags")).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    let models = body["models"].as_array().expect("models 必须是数组");
    assert!(!models.is_empty(), "模型列表不能为空：{body}");
    assert!(models[0]["name"].is_string());

    /* ── /api/chat 非流式 ── */
    let res = client
        .post(format!("{root}/api/chat"))
        .json(&json!({
            "model": "auto",
            "stream": false,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["done"], true);
    assert_eq!(body["message"]["content"], "pong");
    assert_eq!(body["message"]["role"], "assistant");
    assert_eq!(body["prompt_eval_count"], 9);
    assert_eq!(body["eval_count"], 3);

    /* ── /api/chat 流式（Ollama 默认就是流式） ── */
    let res = client
        .post(format!("{root}/api/chat"))
        .json(&json!({
            "model": "auto",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/x-ndjson"
    );
    let text = res.text().await.unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|_| panic!("不是合法 JSON 行：{l}")))
        .collect();
    assert!(lines.len() >= 2, "至少两行：{text}");
    assert_eq!(lines[0]["done"], false);
    assert_eq!(lines[0]["message"]["content"], "po");
    let last = lines.last().unwrap();
    assert_eq!(last["done"], true, "最后一行必须 done=true：{text}");
    assert_eq!(last["eval_count"], 2);
    assert!(!text.contains("data:"), "NDJSON 不该带 SSE 前缀：{text}");

    /* ── /api/generate ── */
    let res = client
        .post(format!("{root}/api/generate"))
        .json(&json!({
            "model": "auto",
            "system": "be brief",
            "prompt": "ping",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["response"], "pong");
    assert_eq!(body["done"], true);

    /* ── 上游实际收到的形态 ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "应当有三次上游调用");
    assert_eq!(seen[0]["messages"][0]["content"], "hi");
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[1]["stream"], true);
    assert_eq!(seen[2]["messages"][0]["role"], "system");
    assert_eq!(seen[2]["messages"][1]["content"], "ping");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
