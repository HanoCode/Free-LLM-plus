//! 账本的端到端验证。
//!
//! 要证明两件事：
//! 1. 预扣会被真实 usage 覆盖 —— 否则余量会被 max_tokens 长期虚占；
//! 2. RPD 打满后 Gate 直接拦，而且巡检的消耗不占用户的请求数。

use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, providers: Vec<Value>, chain: Vec<String>) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": providers,
            "routing": { "chain": chain }
        })
        .to_string(),
    )
    .unwrap();
}

fn provider(id: &str, base: String, quota: Value) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
        "models": [{ "id": "m1", "ownedBy": id }],
        "quota": quota,
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

#[tokio::test]
async fn reserve_is_settled_to_real_usage_and_rpd_gates() {
    let dir = std::env::temp_dir().join(format!("fp-quota-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            axum::Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "ok" } }],
                "usage": { "prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12 }
            }))
        }),
    );
    let base = spawn(upstream).await;

    // rpd = 2：第三次必须被 Gate 拦在本地，不打到上游
    write_config(
        &dir,
        vec![provider("p", base.clone(), json!({ "rpd": 2 }))],
        vec!["p:m1".into()],
    );
    freeport_gateway::secrets::set_key("p", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = json!({
        "model": "auto",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 4096   // 故意开大：预扣会很离谱，必须被 settle 修正
    });

    for _ in 0..2 {
        let res = client.post(&url).json(&body).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let q = gw.quota().await;
    let p = q.iter().find(|x| x.provider == "p").expect("p 应当在账本里");
    let rpd = p.windows.iter().find(|w| w.kind == "rpd").unwrap();
    assert_eq!(rpd.used_user, 2.0, "两次请求 → 请求数记 2");
    assert_eq!(rpd.used_probe, 0.0, "用户流量不混进巡检账");

    let tpd = p.windows.iter().find(|w| w.kind == "tpd").unwrap();
    assert_eq!(
        tpd.used_user, 24.0,
        "预扣 1 + 4096 必须被真实 usage 12 覆盖，实际 24"
    );
    assert!(rpd.exhausted, "用完 2 次即视为耗尽，下一次请求由 Gate 拦");
    assert!(rpd.remaining == 0.0);

    // 第三次：Gate 直接拦，不上游
    let res = client.post(&url).json(&body).send().await.unwrap();
    assert_ne!(res.status(), StatusCode::OK, "RPD 打满必须失败");
    let trail = res
        .headers()
        .get("x-freeport-trail")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(trail.is_empty() || !trail.contains("p/m1:ok"), "不该有成功记录：{trail}");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
