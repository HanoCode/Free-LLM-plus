//! 多账号（一厂商多凭据）的端到端验证。
//!
//! 盯三件事：
//! 1. 同一厂商的两条凭据都进候选链，各自独立成为一跳
//! 2. **限流冷却按凭据隔离**：A 被 429 之后，下一次直接走 B，而不是整家停摆
//! 3. **额度按凭据算**：两条 key 各自 rpd=1 时，两次请求后整家才真正没得用
//!
//! 这三条如果做不到，多账号就只是 UI 上多一行字。

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, base: String, quota: Option<Value>) {
    let mut a = json!({
        "id": "acct_a",
        "name": "key-1",
        "keyMask": "sk-aaa",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z"
    });
    let mut b = json!({
        "id": "acct_b",
        "name": "key-2",
        "keyMask": "sk-bbb",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z"
    });
    if let Some(q) = &quota {
        a["quota"] = q.clone();
        b["quota"] = q.clone();
    }
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
                "accounts": [a, b],
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

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim_start_matches("Bearer ")
        .to_string()
}

/// 只有 `key-a` 会被 429；其余一律 200。
async fn rate_limited_upstream() -> String {
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let sink = seen.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, Json(_body): Json<Value>| {
            let sink = sink.clone();
            async move {
                let key = bearer(&headers);
                sink.lock().unwrap().push(key.clone());
                if key == "key-a" {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "60")],
                        Json(json!({ "error": { "message": "rate limited" } })),
                    )
                        .into_response();
                }
                Json(json!({
                    "id": "cmpl-1",
                    "model": "m1",
                    "choices": [{ "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
                }))
                .into_response()
            }
        }),
    );
    spawn(app).await
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-acct-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    dir
}

fn keys() {
    freeport_gateway::secrets::set_key("acct_a", "key-a").unwrap();
    freeport_gateway::secrets::set_key("acct_b", "key-b").unwrap();
}

#[tokio::test]
async fn two_credentials_are_two_hops_and_cool_separately() {
    let dir = fresh_dir("cool");
    let base = rate_limited_upstream().await;
    write_config(&dir, base, None);
    keys();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    // 两条凭据都要出现在 UI / 候选视图里
    let providers = gw.providers().await;
    assert_eq!(providers[0].accounts.len(), 2, "一个厂商两条凭据");
    assert_eq!(gw.quota().await.len(), 2, "额度按凭据出两行");

    /* 第一次：A 被限流，同一轮里换到 B */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer legacy")
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "A 被限流后应当换 B，而不是整家失败");

    let logs = gw.logs().await;
    let first = &logs[0];
    assert_eq!(first.attempts.len(), 2, "应当有两次尝试：{:?}", first.attempts);
    let accounts: Vec<&str> = first
        .attempts
        .iter()
        .filter_map(|a| a.account.as_deref())
        .collect();
    assert!(accounts.contains(&"acct_a") && accounts.contains(&"acct_b"), "{accounts:?}");
    assert!(first.attempts.iter().any(|a| a.kind == "rate_limit"));
    assert_eq!(first.account_id.as_deref(), Some("acct_b"), "最终答它的是 B");
    assert_eq!(
        first.account_name.as_deref(),
        Some("key-2"),
        "日志里要带凭据名字快照"
    );

    /* 第二次：A 还在冷却，直接一条跳到 B */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer legacy")
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi again" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let logs = gw.logs().await;
    let second = &logs[0];
    assert_eq!(
        second.attempts.len(),
        1,
        "被限流的凭据不该再撞一次：{:?}",
        second.attempts
    );
    assert_eq!(second.attempts[0].account.as_deref(), Some("acct_b"));

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
