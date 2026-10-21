//! `Idempotency-Key` 端到端验证（§3.6.11）。
//!
//! 三件事，少一件这条链路就只是摆设：
//! 1. 同键同指纹第二次不打上游，直接重放上次的响应（用上游命中计数证明）；
//! 2. 同键不同请求体 → 409，绝不静默复用别的问题的答案；
//! 3. 流式请求不缓存——同一键发两次流式就是两次真实生成。
//!
//! 单独一个文件：`FREEPORT_DATA_DIR` 是进程级变量，同进程并行跑会互相踩。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, base: String) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": [{
                "id": "p",
                "name": "P",
                "baseUrl": base,
                "tier": "A",
                "keyMask": "sk-aaa",
                "enabled": true,
                "accounts": [{
                    "id": "acct_idem",
                    "name": "key-1",
                    "keyMask": "sk-aaa",
                    "enabled": true,
                    "addedAt": "2026-10-19T00:00:00Z"
                }],
                "models": [{ "id": "m1", "ownedBy": "p" }],
                "health": { "status": "unknown" }
            }],
            "routing": { "chain": ["p:m1"] }
        })
        .to_string(),
    )
    .unwrap();
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

/// 上游桩：每次被调用就 +1，并回一份可辨认的答案
async fn counting_upstream(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(_b): Json<Value>| {
                let hits = hits.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    Json(json!({
                        "id": format!("cmpl-{n}"),
                        "model": "m1",
                        "choices": [{ "message": { "role": "assistant", "content": format!("answer-{n}") }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
                    }))
                }
            }),
        )
        .route("/v1/models", get(|| async { Json(json!({ "data": [] })) }));
    spawn(app).await
}

#[tokio::test]
async fn same_key_replays_and_different_body_conflicts() {
    let dir = std::env::temp_dir().join(format!("fp-idem-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let hits = Arc::new(AtomicUsize::new(0));
    let base = counting_upstream(hits.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_idem", "key-a").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let chat = format!("http://127.0.0.1:{port}/v1/chat/completions");

    let body = json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] });

    let first = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .header("idempotency-key", "req-abc")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = first.text().await.unwrap();
    assert!(first_body.contains("answer-1"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let replay = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .header("idempotency-key", "req-abc")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay
            .headers()
            .get("x-freeport-idempotent-replay")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "重放必须自报身份，否则排查时分不清是缓存还是真的又生成了一次"
    );
    assert_eq!(replay.text().await.unwrap(), first_body, "必须逐字节相同");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "重放绝不能打到上游——否则用户白扣一次额度"
    );

    let other = json!({ "model": "auto", "messages": [{ "role": "user", "content": "别的请求" }] });
    let conflict = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .header("idempotency-key", "req-abc")
        .json(&other)
        .send()
        .await
        .unwrap();
    assert_eq!(
        conflict.status(),
        StatusCode::CONFLICT,
        "同键不同体必须显式拒绝，不能把上一个问题的答案交给它"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // 流式不缓存：同一个键发两次流式，就是两次真实生成
    let stream_body = json!({
        "model": "auto",
        "stream": true,
        "messages": [{ "role": "user", "content": "hi" }]
    });
    for _ in 0..2 {
        let res = client
            .post(&chat)
            .header("authorization", "Bearer legacy")
            .header("idempotency-key", "req-stream")
            .json(&stream_body)
            .send()
            .await
            .unwrap();
        let _ = res.text().await.unwrap();
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        3,
        "流式请求必须每次真实生成（1 次首答 + 2 次流式）"
    );

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
