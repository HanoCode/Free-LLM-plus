//! 凭据级校验：一条 key 挂了，不该把另一条也刷红。
//!
//! 多账号之前 `probe()` 只写厂商级 `health`，于是「校验第二把 key 失败」
//! 在界面上表现为整家挂掉 —— 而实际上第一把还是好的。这个用例把这条守住。

use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use serde_json::json;

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
                "keyMask": "sk-••••good",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [{ "id": "m1", "ownedBy": "p" }],
                "accounts": [
                    {
                        "id": "acct_good",
                        "name": "key-1",
                        "keyMask": "sk-••••good",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:00Z"
                    },
                    {
                        "id": "acct_bad",
                        "name": "key-2",
                        "keyMask": "sk-••••bad",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:00Z"
                    }
                ],
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

/// 只认 `Bearer good`，其他一律 401 —— 模拟「第二把 key 被吊销」。
async fn models(headers: HeaderMap) -> (StatusCode, axum::Json<serde_json::Value>) {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth == "Bearer good" {
        (
            StatusCode::OK,
            axum::Json(json!({ "data": [{ "id": "m1", "owned_by": "p" }] })),
        )
    } else {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": { "message": "invalid api key" } })),
        )
    }
}

#[tokio::test]
async fn a_dead_key_only_marks_itself() {
    let dir = std::env::temp_dir().join(format!("fp-probe-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let base = spawn(Router::new().route("/v1/models", get(models))).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_good", "good").unwrap();
    freeport_gateway::secrets::set_key("acct_bad", "bad").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;

    let ok = gw.probe_account("p", "acct_good").await;
    assert!(ok.ok, "第一把 key 应当通过：{:?}", ok.error);

    let bad = gw.probe_account("p", "acct_bad").await;
    assert!(!bad.ok, "第二把 key 是 401，必须失败");
    assert!(
        bad.error.as_deref().unwrap_or_default().contains("invalid api key"),
        "错误原文要透出来，便于判断是吊销还是网络：{:?}",
        bad.error
    );

    let p = gw
        .providers()
        .await
        .into_iter()
        .find(|x| x.id == "p")
        .expect("provider 还在");

    let good = p.accounts.iter().find(|a| a.id == "acct_good").unwrap();
    let dead = p.accounts.iter().find(|a| a.id == "acct_bad").unwrap();
    assert_eq!(
        good.health.as_ref().map(|h| h.status.clone()),
        Some(freeport_core::model::HealthStatus::Healthy),
        "好 key 的状态不能被另一条 key 的失败覆盖"
    );
    assert_eq!(
        dead.health.as_ref().map(|h| h.status.clone()),
        Some(freeport_core::model::HealthStatus::Error)
    );
    assert_eq!(
        p.health.status,
        freeport_core::model::HealthStatus::Healthy,
        "还有一条好的，厂商级就该是好的"
    );

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
