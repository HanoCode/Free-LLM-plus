//! 巡检的端到端验证：真起一个上游，真发一次探针，真落一次状态。
//!
//! 两个场景放在同一个测试里跑：密钥库是进程级单例，数据目录用环境变量指定，
//! 并行跑两个 test 会互相串目录。

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use freeport_core::dto::GwModel;
use freeport_gateway::{secrets, Gateway};
use serde_json::json;

fn health_of(models: &[GwModel], id: &str) -> String {
    models
        .iter()
        .find(|m| m.id == id)
        .and_then(|m| m.freeport.clone())
        .and_then(|f| f.health)
        .unwrap_or_default()
}

fn write_config(dir: &std::path::Path, provider: serde_json::Value) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "providers": [provider],
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn patrol_probes_and_drives_the_health_state_machine() {
    let dir = std::env::temp_dir().join(format!("fp-patrol-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    /* ── 场景 1：健康上游 ── */

    let ok_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            // 本机回环常常 <1ms，计时取整后会是 0；给 2ms 让「带延迟」这条断言稳定
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "pong" } }],
                "usage": { "prompt_tokens": 3, "completion_tokens": 8, "total_tokens": 11 }
            }))
        }),
    );
    let ok_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ok_addr = ok_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(ok_listener, ok_app).await;
    });

    write_config(
        &dir,
        json!({
            "id": "probe-me",
            "name": "Probe Me",
            "baseUrl": format!("http://{ok_addr}/v1"),
            "tier": "A",
            "keyMask": "test••••key",
            "enabled": true,
            "addedAt": "2026-10-19T00:00:00Z",
            "models": [{ "id": "m1", "ownedBy": "probe-me" }],
            "health": { "status": "unknown" }
        }),
    );
    secrets::set_key("probe-me", "test-key").unwrap();

    let gw = Gateway::new();
    gw.load().await;

    let report = gw.patrol_pass(false).await;
    assert_eq!(report.ran, 1, "应当探一条：{report:?}");
    assert_eq!(report.ok, 1, "假上游应当成功：{report:?}");

    let models = gw.models().await;
    assert_eq!(health_of(&models, "probe-me:m1"), "healthy");
    let latency = models
        .iter()
        .find(|m| m.id == "probe-me:m1")
        .and_then(|m| m.freeport.clone())
        .and_then(|f| f.latency_ms)
        .unwrap_or(0);
    assert!(latency > 0, "成功探针必须带延迟");

    let pat = gw.patrol().await;
    assert_eq!(pat.used_today, 1, "预算应当被扣一次");
    assert_eq!(pat.probes_ok, 1);
    // 免费开放时代（pro::FREE_FOR_ALL）预算走 Pro 路径：空配额目录钳到下限 60。
    // 原断言（免费固定 5）保留在注释里，恢复收费时改回。
    // assert_eq!(pat.budget_today, 5, "免费巡检预算固定 5");
    assert_eq!(pat.budget_today, 60, "免费时代按 Pro 路径，下限 60");

    let again = gw.patrol_pass(false).await;
    assert_eq!(again.ran, 0, "刚探过不该再探：{again:?}");

    /* ── 场景 2：上游持续 500 ── */

    let bad_app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))) }),
    );
    let bad_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bad_addr = bad_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(bad_listener, bad_app).await;
    });

    write_config(
        &dir,
        json!({
            "id": "bad",
            "name": "Bad",
            "baseUrl": format!("http://{bad_addr}/v1"),
            "tier": "A",
            "keyMask": "k••••k",
            "enabled": true,
            "addedAt": "2026-10-19T00:00:00Z",
            "models": [{ "id": "m1" }],
            "health": { "status": "unknown" }
        }),
    );
    secrets::set_key("bad", "k").unwrap();

    let gw = Gateway::new();
    gw.load().await;

    // 第一次失败：只降级，不判死
    let _ = gw.patrol_pass(true).await;
    assert_eq!(health_of(&gw.models().await, "bad:m1"), "degraded");

    // 第二次失败：判 unavailable
    let _ = gw.patrol_pass(true).await;
    assert_eq!(health_of(&gw.models().await, "bad:m1"), "unavailable");

    // 判死之后不在候选链里
    let models = gw.models().await;
    let plan = freeport_core::plan(
        &{
            let mut d = freeport_core::model::AppData::default();
            d.providers.push(freeport_core::model::Provider {
                id: "bad".into(),
                name: "Bad".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                tier: "A".into(),
                homepage: None,
                note: None,
                key_mask: "k".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                accounts: vec![freeport_core::model::Account {
                    id: "bad".into(),
                    name: "default".into(),
                    key_mask: "k".into(),
                    enabled: true,
                    added_at: String::new(),
                    quota: None,
                    health: None,
                    purposes: Vec::new(),
                }],
                proxy: None,
                wire: None,
                models: vec![freeport_core::model::ModelInfo {
                    id: "m1".into(),
                    owned_by: "bad".into(),
                    context: None,
                    created: None,
                    free: None,
                    caps: Default::default(),
                    candidate: false,
                }],
                health: Default::default(),
            });
            d.model_health.insert(
                "bad:m1".into(),
                freeport_core::model::ModelHealth {
                    status: freeport_core::model::HealthStatus::Unavailable,
                    ..Default::default()
                },
            );
            d
        },
        "auto",
        &freeport_core::cooldown::Cooldowns::new(),
        &|_| true,
        &freeport_core::quota::Ledger::default(),
    );
    assert!(plan.is_empty(), "unavailable 的模型不该进候选链");
    assert!(!models.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
