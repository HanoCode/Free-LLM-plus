//! 工具输出压缩的端到端验证：断言的是**上游真的收到了什么**。
//!
//! 纯函数单测只能证明「算法会剪」，证明不了「剪完的那份真的被发出去」。
//! 这里用桩上游把请求体抓下来看，顺带验两件事：
//! - 关着的时候一个字符都不许动（默认关，不能偷偷生效）；
//! - 开着的时候响应头要明说省了多少，用户才能对上「模型说没看到中间那段」。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

/// 桩上游：把收到的 body 留下来，供断言「发出去的到底是什么」
async fn capturing_upstream(seen: Arc<Mutex<Vec<Value>>>, hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(b): Json<Value>| {
                let seen = seen.clone();
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    seen.lock().unwrap().push(b);
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

fn long_tool_output() -> String {
    // 中间那段是「噪音」，两头是「有用信息」——压缩该留的正是两头
    format!(
        "HEAD-MARK{}MIDDLE{}TAIL-MARK",
        "a".repeat(2000),
        "b".repeat(6000)
    )
}

fn body_with(output: &str) -> Value {
    json!({
        "model": "auto",
        "messages": [
            { "role": "user", "content": "跑一下测试" },
            { "role": "assistant", "content": "", "tool_calls": [{
                "id": "c1", "type": "function",
                "function": { "name": "run", "arguments": "{}" }
            }] },
            { "role": "tool", "tool_call_id": "c1", "content": output }
        ]
    })
}

#[tokio::test]
async fn compression_is_off_by_default_and_reports_what_it_dropped_when_on() {
    let dir = std::env::temp_dir().join(format!("fp-compress-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(AtomicUsize::new(0));
    let base = capturing_upstream(seen.clone(), hits.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_a", "key-a").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let client = reqwest::Client::new();
    let original = long_tool_output();

    // 1) 默认关：上游必须收到一模一样的工具输出
    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .json(&body_with(&original))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers().get("x-freeport-compressed").is_none(),
        "没压就不该带这个头，否则用户会以为自己的请求被改过"
    );
    {
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0]["messages"][2]["content"].as_str().unwrap(),
            original,
            "默认关着的时候，一个字符都不许动"
        );
    }

    // 2) 打开：只留头尾，中间换成说明
    gw.set_config(freeport_core::dto::ConfigPatch {
        compress: Some(true),
        compress_max_tool_chars: Some(1_000),
        ..Default::default()
    })
    .await;

    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .json(&body_with(&original))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let sent = {
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 2);
        got[1]["messages"][2]["content"].as_str().unwrap().to_string()
    };
    assert!(sent.starts_with("HEAD-MARK"), "开头必须留着：那通常是命令与参数");
    assert!(sent.ends_with("TAIL-MARK"), "结尾必须留着：报错栈与退出码都在尾巴上");
    assert!(sent.contains("Freeport 压缩"), "省略处要有可见的说明");
    assert!(sent.contains("不要根据这段摘要臆测"), "得告诉模型别自己编");
    assert!(sent.len() < original.len() / 2);

    let saved = res
        .headers()
        .get("x-freeport-compressed")
        .expect("压了就要在响应头里说")
        .to_str()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!(saved > 3_000, "省的字符数要如实写出来，实际 {saved}");

    // 用户说话的正文一个字都没动（只动工具产出）
    let got = seen.lock().unwrap();
    assert_eq!(got[1]["messages"][0]["content"], "跑一下测试");

    drop(got);
    gw.stop().await;
}
