//! 额度按凭据算：同一厂商的两条 key 各有各的 rpd。
//!
//! 单独一个测试文件（而不是和冷却测试放一起），因为两者都要改
//! `FREEPORT_DATA_DIR`——同一进程里并行跑会互相读到对方的配置和密钥库。

use axum::http::StatusCode;
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

async fn always_ok_upstream() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|Json(_b): Json<Value>| async move {
            Json(json!({
                "id": "cmpl-1",
                "model": "m1",
                "choices": [{ "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4 }
            }))
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
async fn quota_is_counted_per_credential() {
    let dir = fresh_dir("quota");
    let base = always_ok_upstream().await;
    write_config(&dir, base, Some(json!({ "rpd": 1 })));
    keys();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    for _ in 0..2 {
        let res = client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("authorization", "Bearer legacy")
            .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "两条凭据各有一次额度");
    }

    // 每条凭据的 rpd 都单独打满
    for q in gw.quota().await {
        let rpd = q.windows.iter().find(|w| w.kind == "rpd").unwrap();
        assert_eq!(rpd.used_user, 1.0, "{} 的日请求数应当各记各的", q.account_id);
        assert!(rpd.exhausted, "{} 的日额度应当各自耗尽", q.account_id);
    }

    // 第三次：两条都耗尽 → 本地拒，不打上游
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer legacy")
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
