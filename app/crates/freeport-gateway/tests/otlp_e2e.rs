//! OTLP 导出的端到端验证。
//!
//! 起一个桩 collector（POST /v1/traces），把网关的 OTLP 配置指向它，
//! 发一次真实 chat 请求，等导出循环把 span 推过来。盯三件事：
//! 1. payload 是合法的 OTLP/JSON（resourceSpans → scopeSpans → spans）
//! 2. span 属性带上了路由元数据（model / provider / ok / tokens）
//! 3. 自定义头（Bearer）按配置发出去

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

#[tokio::test]
async fn otlp_exports_request_spans_to_collector() {
    let dir = std::env::temp_dir().join(format!("fp-otlp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    // 桩 collector：收 span、收自定义头
    let seen: Arc<Mutex<Vec<(HeaderMap, Value)>>> = Default::default();
    let sink = seen.clone();
    let health_hits: Arc<Mutex<usize>> = Default::default();
    let h2 = health_hits.clone();

    // 桩上游：标准 OpenAI 兼容
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(json!({
                "id": "cmpl-1",
                "model": "m1",
                "choices": [{ "message": { "role": "assistant", "content": "done" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12 }
            }))
            .into_response()
        }),
    );
    let up_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_addr = up_listener.local_addr().unwrap();
    tokio::spawn(async move { let _ = axum::serve(up_listener, upstream).await; });

    // 桩 collector
    let collector = Router::new()
        .route(
            "/v1/traces",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push((headers, body));
                    StatusCode::OK
                }
            }),
        )
        .route("/healthz", get(move || {
            let h = h2.clone();
            async move {
                *h.lock().unwrap() += 1;
                "ok"
            }
        }));
    let c_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let c_addr = c_listener.local_addr().unwrap();
    tokio::spawn(async move { let _ = axum::serve(c_listener, collector).await; });

    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": {
                "port": 0,
                "host": "127.0.0.1",
                "otlp": {
                    "enabled": true,
                    "endpoint": format!("http://{c_addr}"),
                    "intervalSecs": 1,
                    "batchSize": 16,
                    "headers": ["Authorization: Bearer collector-secret"]
                }
            },
            "providers": [{
                "id": "a",
                "name": "a",
                "baseUrl": format!("http://{up_addr}/v1"),
                "tier": "A",
                "keyMask": "test••••key",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [{ "id": "m1", "ownedBy": "a" }],
                "health": { "status": "unknown" }
            }],
            "routing": { "chain": ["a:m1"] }
        })
        .to_string(),
    )
    .unwrap();
    freeport_gateway::secrets::set_key("a", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();

    let client = reqwest::Client::new();
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 等导出循环醒来（间隔 1s），最多等 10s
    let deadline = Instant::now() + Duration::from_secs(10);
    let batch = loop {
        let n = seen.lock().unwrap().len();
        if n > 0 {
            break seen.lock().unwrap().remove(0);
        }
        if Instant::now() > deadline {
            panic!("10s 内没有收到 OTLP 上报");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    gw.stop().await;

    let (headers, body) = batch;
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer collector-secret"),
        "自定义头必须按配置发"
    );

    let rs = body["resourceSpans"].as_array().expect("resourceSpans 数组");
    assert_eq!(rs.len(), 1);
    let svc = rs[0]["resource"]["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["key"] == "service.name")
        .expect("resource 必须带 service.name");
    assert_eq!(svc["value"]["stringValue"], "freeport-gateway");

    let span = &rs[0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["name"], "freeport.request");
    assert_eq!(span["status"]["code"], 1, "成功请求 status 应为 OK");
    assert!(span["traceId"].as_str().unwrap().len() == 32);
    // 64 位时间戳在 JSON 里必须是字符串
    assert!(span["startTimeUnixNano"].is_string());

    let attr = |k: &str| {
        span["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == k)
            .map(|a| a["value"].clone())
            .expect(k)
    };
    assert_eq!(attr("freeport.model")["stringValue"], "m1");
    assert_eq!(attr("freeport.provider")["stringValue"], "a");
    assert_eq!(attr("freeport.ok")["boolValue"], true);
    assert_eq!(attr("freeport.tokens.input")["intValue"], "9");
    assert_eq!(attr("url.path")["stringValue"], "/v1/chat/completions");

    let _ = std::fs::remove_dir_all(&dir);
}
