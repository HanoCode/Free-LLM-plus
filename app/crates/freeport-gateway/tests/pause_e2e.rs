//! 暂停闸的端到端验证。
//!
//! 暂停必须做到三件事，少一件就是假开关：
//! 1. 业务 POST 一律 503，并且**不往上游发**（用命中计数证明）；
//! 2. `/health` 与 `/v1/models` 仍然 200——否则监控与客户端会以为进程死了；
//! 3. 恢复后立刻能正常转发，不需要重启。
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
                    "id": "acct_a",
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

/// 上游桩：返回 200，并记下被打了几次
async fn counting_upstream(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(_b): Json<Value>| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "id": "cmpl-1",
                        "model": "m1",
                        "choices": [{ "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
                    }))
                }
            }),
        )
        .route("/v1/models", get(|| async { Json(json!({ "data": [] })) }));
    spawn(app).await
}

#[tokio::test]
async fn pause_stops_business_traffic_but_keeps_the_control_surface_alive() {
    let dir = std::env::temp_dir().join(format!("fp-pause-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let hits = Arc::new(AtomicUsize::new(0));
    let base = counting_upstream(hits.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_a", "key-a").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let chat = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] });

    let first = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "暂停之前应当真的打到上游");

    assert!(gw.set_paused(true));
    let status = gw.status().await;
    assert!(status.paused, "状态里必须能看到暂停，否则 UI 只能靠猜");

    let blocked = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "被暂停的请求绝不能偷偷发给上游——否则用户以为停了，额度还在掉"
    );

    // 控制面还得活着：暂停不等于进程挂了
    let health = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let models = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);

    assert!(!gw.set_paused(false));
    let resumed = client
        .post(&chat)
        .header("authorization", "Bearer legacy")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK, "恢复后不需要重启就该能用");
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
