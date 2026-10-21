//! `/v1/completions`（旧版补全协议）的端到端验证。
//!
//! 盯四件事：
//! 1. `prompt` 变成一条 user 消息，`echo` 被本地剥掉不透给上游
//! 2. 非流式响应是 `object: "text_completion"` + `choices[].text`
//! 3. 流式每块也是 `text`，role-only 首块被丢掉，结束时 `[DONE]`
//! 4. 拒绝而不是猜：批量 prompt / token id / suffix 一律 400

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

/// 首块只有 role（没有内容），旧协议里没有对应物——必须被丢掉。
const SSE_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"m1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"m1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"po\"}}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"m1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ng\"}}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"m1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2,\"total_tokens\":9}}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn completions_endpoint_speaks_the_legacy_protocol() {
    let dir = std::env::temp_dir().join(format!("fp-cmpl-{}", uuid::Uuid::new_v4().simple()));
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
                    "id": "chatcmpl-9",
                    "object": "chat.completion",
                    "created": 1700,
                    "model": "m1",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": " world" },
                        "finish_reason": "length"
                    }],
                    "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
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

    /* ── 非流式 + echo ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/completions"))
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "auto",
            "prompt": "hello",
            "max_tokens": 8,
            "echo": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["object"], "text_completion");
    assert_eq!(body["id"], "cmpl-9", "chat 的 id 前缀要换成旧协议的");
    assert_eq!(body["choices"][0]["text"], "hello world", "echo 要把 prompt 拼回去");
    assert_eq!(body["choices"][0]["finish_reason"], "length");
    assert_eq!(body["usage"]["total_tokens"], 5);
    assert!(body.get("messages").is_none(), "不该把 chat 的字段漏出去");

    /* ── 流式：每块是 text，不是 delta ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/completions"))
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "auto",
            "prompt": "hi",
            "stream": true,
            "echo": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let sse = res.text().await.unwrap();
    assert!(sse.contains("\"object\":\"text_completion\""), "块形状不对：{sse}");
    assert!(sse.contains("\"text\":\"hipo\""), "echo 应挂在第一段内容上：{sse}");
    assert!(sse.contains("\"text\":\"ng\""), "第二段丢了：{sse}");
    assert!(sse.contains("\"finish_reason\":\"stop\""), "收尾块丢了：{sse}");
    assert!(sse.contains("data: [DONE]"), "缺结束标记：{sse}");
    assert!(!sse.contains("\"delta\""), "不该把 chat 的 delta 漏出去：{sse}");
    assert!(!sse.contains("\"role\":\"assistant\""), "role-only 首块应当被丢掉：{sse}");

    /* ── 上游实际收到的形态：单条 user 消息，没有 echo ── */
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "应当有两次上游调用");
    assert_eq!(seen[0]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(seen[0]["messages"][0]["role"], "user");
    assert_eq!(seen[0]["messages"][0]["content"], "hello");
    assert_eq!(seen[0]["max_tokens"], 8);
    assert!(seen[0].get("echo").is_none(), "echo 不该透给上游");
    assert!(seen[0].get("prompt").is_none(), "prompt 不该透给上游");
    drop(seen);

    /* ── 拒绝而不是猜 ── */
    for bad in [
        json!({ "prompt": ["a", "b"] }),
        json!({ "prompt": [1, 2, 3] }),
        json!({ "prompt": "a", "suffix": "b" }),
    ] {
        let res = client
            .post(format!("http://127.0.0.1:{port}/v1/completions"))
            .header("authorization", "Bearer whatever")
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "应当拒绝：{bad}");
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], "unsupported_parameter");
    }

    /* ── 日志按真实入口记账 ── */
    let logs = gw.logs().await;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].path, "/v1/completions");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
