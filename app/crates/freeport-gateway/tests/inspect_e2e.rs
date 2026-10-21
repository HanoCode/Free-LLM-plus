//! `/v1/freeport/inspect` 的端到端验证。
//!
//! 这个接口的存在意义是给**同机的另一个进程**（MCP server）看实时状态，
//! 所以它必须同时满足两件互相拉扯的事：
//!
//! 1. 拿不到凭据的人**一定**进不来（否则本机任何一个网页都能读你的运行状态）；
//! 2. 拿到控制凭据的人**拿不到**发请求的能力（否则为了看日志，把「烧额度」的权力交出去了）。
//!
//! 第 2 条特别容易被写错：顺手复用 `authToken` 最省事，也就等于把数据面凭据
//! 复制了一份到日志接口上。这里用「控制凭据当 Bearer 发业务请求必须 401」把它钉住。

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

async fn upstream(hits: Arc<AtomicUsize>) -> String {
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn inspect_needs_the_control_token_and_never_grants_data_plane_access() {
    let dir = std::env::temp_dir().join(format!("fp-inspect-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let hits = Arc::new(AtomicUsize::new(0));
    let base = upstream(hits.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_a", "key-a").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let root = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // 先制造一条真实日志：凭空断言「日志是空的」没有意义
    let ok = client
        .post(format!("{root}/v1/chat/completions"))
        .header("authorization", "Bearer legacy")
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let token = freeport_gateway::control::read_token(&dir).expect("启动后应当写下了控制凭据");

    // 1) 没有凭据
    let anon = client
        .get(format!("{root}/v1/freeport/inspect"))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // 2) 拿数据面的 token 当控制凭据：必须被拒（两套凭据不能互换）
    let status = gw.status().await;
    let wrong = client
        .get(format!("{root}/v1/freeport/inspect"))
        .header("x-freeport-control", &status.auth_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong.status(),
        StatusCode::UNAUTHORIZED,
        "authToken 能发业务请求，就不该同时是自省凭据"
    );

    // 3) 正确凭据
    let res = client
        .get(format!("{root}/v1/freeport/inspect?limit=5"))
        .header("x-freeport-control", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["running"], true);
    assert_eq!(body["paused"], false);
    assert_eq!(body["port"].as_u64(), Some(port as u64), "端口必须是真实绑定的那个");
    assert_eq!(body["inflight"], 0);
    assert!(
        body["logTotal"].as_u64().unwrap_or(0) >= 1,
        "刚打完一次请求，实时日志里必须有它：{body}"
    );
    let logs = body["logs"].as_array().expect("logs 必须是数组");
    assert!(!logs.is_empty());
    assert_eq!(logs[0]["requestedModel"], "auto");
    assert!(body["stats"]["requests24h"].as_u64().unwrap_or(0) >= 1);

    // 4) limit 只裁剪返回条数，不改 logTotal
    let one = client
        .get(format!("{root}/v1/freeport/inspect?limit=1"))
        .header("x-freeport-control", &token)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(one["logs"].as_array().unwrap().len(), 1);
    assert_eq!(one["logTotal"], body["logTotal"]);

    // 5) 控制凭据反向当 Bearer 发业务请求：不认
    gw.set_config(freeport_core::dto::ConfigPatch {
        require_auth: Some(true),
        ..Default::default()
    })
    .await;
    let leak = client
        .post(format!("{root}/v1/chat/completions"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        leak.status(),
        StatusCode::UNAUTHORIZED,
        "控制凭据不能当数据面 key 用——它就是为「只能看不能花」而存在的"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "被拒的请求不该打到上游");

    // 6) 进程内的快捷路径也得能用（MCP 用的就是它）
    let live = gw.inspect_live().await.expect("同进程应当能拿到实时视图");
    assert_eq!(live["running"], true);
    gw.stop().await;
}
