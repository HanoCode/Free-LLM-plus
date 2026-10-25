//! 中途接力的端到端验证：
//! 1. 上游把正文流断在半截（干净闭合但没有结束标记）→ 网关用 prefill 续写，
//!    客户端拿到连贯的完整回答，中间不出现 error 帧；
//! 2. 工具调用已下发时断流 → 不续写（半截工具参数续写等于让客户端执行
//!    没写完的工具），如实发 error 帧。
//!
//! 与 hold_failover 同样的约束：密钥库是进程级单例，两个场景放同一个 test。

use axum::body::Body;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn write_config(dir: &std::path::Path, providers: Vec<Value>) {
    let chain: Vec<String> = providers
        .iter()
        .map(|p| format!("{}:m1", p["id"].as_str().unwrap_or("")))
        .collect();
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

fn provider(id: &str, base: String) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-24T00:00:00Z",
        "models": [{ "id": "m1", "ownedBy": id }],
        "health": { "status": "unknown" }
    })
}

fn sse(frame: &str) -> String {
    format!("data: {frame}\n\n")
}

/// 正文流到一半就干净地关了：没有 [DONE]、没有 finish_reason。
/// 这是「半截断流」最常见的样子（hyper/axum 的响应体里带 io::Error
/// 会让连接连响应头都发不出来，所以用干净闭合来模拟）。
fn truncated_mid_stream() -> Body {
    let head = sse(r#"{"choices":[{"delta":{"content":"从前有"}}]}"#);
    Body::from(head)
}

#[tokio::test]
async fn broken_stream_continues_via_prefill_and_tool_calls_do_not() {
    let dir = std::env::temp_dir().join(format!(
        "fp-cont-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    /* ── 场景 1：正文断流 → prefill 续写 ── */
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_cl = hits.clone();
    let resume_seen = Arc::new(AtomicUsize::new(0));
    let resume_cl = resume_seen.clone();
    let flaky = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let hits = hits_cl.clone();
            let resume = resume_cl.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let last_role = body["messages"]
                    .as_array()
                    .and_then(|m| m.last())
                    .and_then(|m| m["role"].as_str())
                    .unwrap_or("");
                if last_role == "assistant" {
                    // 续写请求：确认 prefill 带上了已收到的正文
                    let prefill = body["messages"]
                        .as_array()
                        .and_then(|m| m.last())
                        .and_then(|m| m["content"].as_str())
                        .unwrap_or("")
                        .to_string();
                    assert_eq!(prefill, "从前有", "prefill 必须接上已发出去的正文");
                    resume.fetch_add(1, Ordering::SeqCst);
                    let a = sse(r#"{"choices":[{"delta":{"content":"座山，"}}]}"#);
                    let b = sse(r#"{"choices":[{"delta":{"content":"庙里有个老和尚。"}}]}"#);
                    return (
                        [("content-type", "text/event-stream")],
                        Body::from(format!("{a}{b}data: [DONE]\n\n")),
                    )
                        .into_response();
                }
                ([("content-type", "text/event-stream")], truncated_mid_stream())
                    .into_response()
            }
        }),
    );
    let a = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, flaky).await;
        });
        format!("http://{addr}/v1")
    };

    write_config(&dir, vec![provider("a", a.clone())]);
    freeport_gateway::secrets::set_key("a", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    // 诊断探针已确认假上游可达，正式请求交给网关
    let res = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "stream": true,
            "messages": [{ "role": "user", "content": "讲个故事" }]
        }))
        .send()
        .await
        .unwrap();

    let status = res.status();
    let body = res.text().await.unwrap();
    assert_eq!(status, axum::http::StatusCode::OK, "响应：{body}");
    assert!(
        body.contains("从前有") && body.contains("庙里有个老和尚。"),
        "客户端应当拿到续写后的完整正文：{body}"
    );
    assert!(
        !body.contains("freeport_stream_error") && !body.contains("freeport_stream_truncated"),
        "续写成功就不该给客户端发 error 帧：{body}"
    );
    assert_eq!(
        resume_seen.load(Ordering::SeqCst),
        1,
        "续写请求应当打到同一家上游"
    );

    /* ── 场景 2：工具调用后断流 → 不续写，如实报错 ── */
    let tool_hits = Arc::new(AtomicUsize::new(0));
    let tool_hits_cl = tool_hits.clone();
    let tool_and_die = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(_): Json<Value>| {
            let hits = tool_hits_cl.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let head = sse(r#"{"choices":[{"delta":{"content":"查一下"}}]}"#);
                let call = sse(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"ls","arguments":""}}]}}]}"#,
                );
                // 工具参数只发了一半就干净闭合：没有 [DONE]、没有 finish_reason
                Body::from(format!("{head}{call}"))
                    .into_response()
            }
        }),
    );
    let b = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, tool_and_die).await;
        });
        format!("http://{addr}/v1")
    };

    write_config(&dir, vec![provider("b", b)]);
    freeport_gateway::secrets::set_key("b", "k").unwrap();

    let gw2 = freeport_gateway::Gateway::new();
    gw2.load().await;
    let port2 = gw2.start().await.unwrap();

    let res = client
        .post(format!("http://127.0.0.1:{port2}/v1/chat/completions"))
        .json(&json!({
            "model": "auto",
            "stream": true,
            "messages": [{ "role": "user", "content": "列目录" }]
        }))
        .send()
        .await
        .unwrap();

    let body = res.text().await.unwrap();
    assert!(
        body.contains("stream_truncated"),
        "工具调用断流必须如实报错：{body}"
    );
    assert_eq!(
        tool_hits.load(Ordering::SeqCst),
        1,
        "工具调用场景不该发起续写请求"
    );

    gw.stop().await;
    gw2.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
