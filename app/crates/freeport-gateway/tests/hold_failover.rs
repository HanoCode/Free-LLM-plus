//! 判定点后移的端到端验证：
//! 上游回 200 但没有可用内容时，必须换下一家，而不是把这次记成成功。
//!
//! 两个场景放同一个 test 里：密钥库是进程级单例，数据目录靠环境变量指定，
//! 并行跑会互相串目录。

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

fn write_config(dir: &std::path::Path, providers: Vec<Value>, chain: Vec<String>) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1", "holdIdleMs": 1500, "holdTotalMs": 4000 },
            "providers": providers,
            "routing": { "chain": chain }
        })
        .to_string(),
    )
    .unwrap();
}

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

#[tokio::test]
async fn empty_or_error_200_fails_over_to_the_next_provider() {
    let dir = std::env::temp_dir().join(format!("fp-hold-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    /* ── 场景 1：200 + 只有 role delta，没有任何内容 ── */
    let empty = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }),
    );
    let good = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello-from-b\"}}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }),
    );
    let a = spawn(empty).await;
    let b = spawn(good).await;

    write_config(
        &dir,
        vec![provider("a", a), provider("b", b)],
        vec!["a:m1".into(), "b:m1".into()],
    );
    freeport_gateway::secrets::set_key("a", "k").unwrap();
    freeport_gateway::secrets::set_key("b", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({ "model": "auto", "stream": true, "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-freeport-provider").unwrap(),
        "b",
        "空流必须换到下一家"
    );
    let body = res.text().await.unwrap();
    assert!(body.contains("hello-from-b"), "客户端应当拿到 b 的内容：{body}");

    /* ── 场景 2：200 + body 里藏 error ── */
    let liar = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                StatusCode::OK,
                axum::Json(json!({ "error": { "message": "free tier exhausted", "code": "quota" } })),
            )
                .into_response()
        }),
    );
    let plain = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            axum::Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "from-plain" } }],
                "usage": { "prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5 }
            }))
            .into_response()
        }),
    );
    let c = spawn(liar).await;
    let d = spawn(plain).await;

    write_config(
        &dir,
        vec![provider("c", c), provider("d", d)],
        vec!["c:m1".into(), "d:m1".into()],
    );
    freeport_gateway::secrets::set_key("c", "k").unwrap();
    freeport_gateway::secrets::set_key("d", "k").unwrap();

    let gw2 = freeport_gateway::Gateway::new();
    gw2.load().await;
    let port2 = gw2.start().await.unwrap();

    let res = client
        .post(format!("http://127.0.0.1:{port2}/v1/chat/completions"))
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-freeport-provider").unwrap(),
        "d",
        "200 里的 error 必须被识别并换家"
    );
    let body = res.text().await.unwrap();
    assert!(body.contains("from-plain"), "应当拿到 d 的内容：{body}");

    gw.stop().await;
    gw2.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
