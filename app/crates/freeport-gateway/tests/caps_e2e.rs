//! 能力门禁的端到端验证。
//!
//! 盯三件事：
//! 1. 带图请求打到「能力未知」的模型，被 400 拒了之后要把这个事实记下来
//! 2. 第二次同类请求不再撞同一堵墙（而不是再烧一次额度）
//! 3. 学到的「不会看图」不能连坐文本请求，且 `/v1/route` 要能解释这次排除
//!
//! 这里刻意只接一家、且目录不写能力：只有「未知」才会真的试一次。
//! 「声明了 vision=true 的优先」属于排序，由 `core::route` 的单测覆盖。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, providers: Vec<Value>) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": providers,
            "routing": { "chain": ["a:m1"] }
        })
        .to_string(),
    )
    .unwrap();
}

/// 目录里没有 caps 字段：网关不知道它会不会看图。
fn provider(id: &str, base: String) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
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

/// 一个只会聊天的上游：任何请求都 400，文案与真实厂商一致。
fn blind_upstream(hits: Arc<AtomicUsize>) -> Router {
    Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": {
                            "message": "Invalid content type image_url: this model does not support images",
                            "type": "invalid_request_error"
                        }
                    })),
                )
                    .into_response()
            }
        }),
    )
}

#[tokio::test]
async fn vision_failure_is_learned_and_not_repeated() {
    let dir = std::env::temp_dir().join(format!("fp-caps-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let hits = Arc::new(AtomicUsize::new(0));
    let base = spawn(blind_upstream(hits.clone())).await;

    write_config(&dir, vec![provider("a", base)]);
    freeport_gateway::secrets::set_key("a", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    let vision = json!({
        "model": "auto",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "这张图里是什么" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
            ]
        }]
    });

    /* ── 第一次：未知能力，必须真的试一次才知道 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer x")
        .json(&vision)
        .send()
        .await
        .unwrap();
    assert!(!res.status().is_success(), "唯一候选不会看图，请求必然失败");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    /* ── 第二次：已经学到 vision=false，不该再发 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer x")
        .json(&vision)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "门禁拦下后就没有候选了"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "同一个模型不该为同一个原因烧两次额度"
    );

    /* ── 文本请求：学到的短板只对图像生效 ── */
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("authorization", "Bearer x")
        .json(&json!({
            "model": "auto",
            "messages": [{ "role": "user", "content": "纯文本" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "a 只是不会看图，不该被文本请求排除"
    );
    // 上游照旧 400，但那是它的回答方式，不是门禁的问题
    assert!(!res.status().is_success());

    /* ── /v1/route 要能解释这次排除 ── */
    let body: Value = client
        .get(format!("http://127.0.0.1:{port}/v1/route"))
        .query(&[("model", "auto"), ("vision", "1")])
        .header("authorization", "Bearer x")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rejected = body["rejected"].as_array().expect("要有 rejected");
    let hit = rejected
        .iter()
        .find(|r| r["kind"] == "capability")
        .expect("a 应当以能力原因被拦下");
    assert_eq!(hit["model"], "m1");
    assert!(hit["reason"].as_str().unwrap().contains("vision"));
    assert_eq!(body["needs"]["vision"], true);
}
