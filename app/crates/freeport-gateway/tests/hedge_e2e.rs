//! hedge 端到端验证（§3.6.8）。
//!
//! 真实故障里最常见的是**上游不回包也不报错**：socket 挂着，30 秒没有首字节。
//! 如果预算只控「是否开始下一次重试」，用户会卡死在第一跳。
//!
//! 这个测试用一个「永远不回」的上游打第一跳，验证：
//! 1. 这一跳会在切片内被掐掉，而不是耗掉整个预算；
//! 2. 后面的候选真的被尝试，并且答案来自第二家；
//! 3. 整条链在预算量级内返回，不挂死。
//!
//! 单独一个文件：`FREEPORT_DATA_DIR` 是进程级变量，同进程并行跑会互相踩。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

fn provider(id: &str, tier: &str, base: String) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": tier,
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
        "accounts": [{
            "id": format!("{id}_acct"),
            "name": "k",
            "keyMask": "test••••key",
            "enabled": true,
            "addedAt": "2026-10-19T00:00:00Z"
        }],
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

/// 挂住不回：比预算长得多，只有被掐掉才能让测试通过
async fn hanging_upstream(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(_b): Json<Value>| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Json(json!({ "choices": [] }))
                }
            }),
        )
        .route("/v1/models", get(|| async { Json(json!({ "data": [] })) }));
    spawn(app).await
}

async fn fast_upstream(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(_b): Json<Value>| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "id": "cmpl-fast",
                        "model": "m1",
                        "choices": [{ "message": { "role": "assistant", "content": "from-hop-2" }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
                    }))
                }
            }),
        )
        .route("/v1/models", get(|| async { Json(json!({ "data": [] })) }));
    spawn(app).await
}

#[tokio::test]
async fn a_stalled_hop_is_aborted_so_the_next_one_gets_a_chance() {
    let dir = std::env::temp_dir().join(format!("fp-hedge-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let slow_hits = Arc::new(AtomicUsize::new(0));
    let fast_hits = Arc::new(AtomicUsize::new(0));
    let slow = hanging_upstream(slow_hits.clone()).await;
    let fast = fast_upstream(fast_hits.clone()).await;

    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            // 3s 总预算：切片 = 3s/2 = 1.5s，头回不来就掐，不能让它吃满
            "gateway": { "port": 0, "host": "127.0.0.1", "requestBudgetMs": 3000 },
            "providers": [provider("slow", "A", slow), provider("fast", "B", fast)],
            "routing": { "chain": ["slow:m1", "fast:m1"] }
        })
        .to_string(),
    )
    .unwrap();

    freeport_gateway::secrets::set_key("slow_acct", "k-slow").unwrap();
    freeport_gateway::secrets::set_key("fast_acct", "k-fast").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    let t = Instant::now();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer legacy")
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK, "第二跳必须接上");
    let body = res.text().await.unwrap();
    assert!(body.contains("from-hop-2"), "答案应当来自第二家：{body}");
    assert_eq!(slow_hits.load(Ordering::SeqCst), 1);
    assert_eq!(fast_hits.load(Ordering::SeqCst), 1);

    let elapsed = t.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "整条链必须按预算收口，实际耗时 {elapsed:?}——说明第一跳没被掐"
    );

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
