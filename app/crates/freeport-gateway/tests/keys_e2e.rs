//! 虚拟 key 的端到端验证：鉴权链路 + 每 key 配额。
//!
//! 重点不是「能不能新建 key」，而是：
//! 1. 老 authToken 仍然可用（迁移不能砸掉现有客户端）；
//! 2. 每个 key 的配额独立，一个吃满不影响另一个。

use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use freeport_core::dto::KeyInput;
use serde_json::json;

fn write_config(dir: &std::path::Path, base: String) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": {
                "port": 0,
                "host": "127.0.0.1",
                "authToken": "legacy-ok",
                "requireAuth": true
            },
            "providers": [{
                "id": "p",
                "name": "p",
                "baseUrl": base,
                "tier": "A",
                "keyMask": "test••••key",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
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

#[tokio::test]
async fn legacy_token_still_works_and_each_key_has_its_own_quota() {
    let dir = std::env::temp_dir().join(format!("fp-keys-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            axum::Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "ok" } }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
            }))
        }),
    );
    let base = spawn(upstream).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("p", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let body = json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] });

    // 没有 Authorization → 401
    let res = client.post(&url).json(&body).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "开了鉴权就必须带 token");

    // 老 token 仍然通过（load 时迁移成了 key_default）
    let res = client
        .post(&url)
        .bearer_auth("legacy-ok")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "老 authToken 不能被迁移砸掉");

    // 新建一个每日 1 次的 key
    let created = gw
        .create_key(KeyInput {
            name: "ci".into(),
            rpm_limit: None,
            daily_limit: Some(1),
            daily_token_limit: None,
            allowed_models: Vec::new(),
            expires_at: None,
        })
        .await
        .expect("建 key 应当成功");
    assert!(created.secret.starts_with("fp-"), "明文前缀应当可辨认");
    assert_eq!(created.key.prefix, &created.secret[..11]);

    let res = client
        .post(&url)
        .bearer_auth(&created.secret)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "新 key 第一次应当通过");

    // 第二次超配额，并且要给出人话原因
    let res = client
        .post(&url)
        .bearer_auth(&created.secret)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    let text = res.text().await.unwrap();
    assert!(text.contains("今日 1 次"), "错误要能解释原因：{text}");

    // 另一个 key 不受影响
    let other = gw
        .create_key(KeyInput {
            name: "other".into(),
            rpm_limit: None,
            daily_limit: Some(5),
            daily_token_limit: None,
            allowed_models: Vec::new(),
            expires_at: None,
        })
        .await
        .unwrap();
    let res = client
        .post(&url)
        .bearer_auth(&other.secret)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "配额是按 key 独立算的");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
