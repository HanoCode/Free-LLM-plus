//! `/v1/embeddings` 的端到端验证。
//!
//! 盯四件事：
//! 1. 请求原样转发到上游 `{base}/embeddings`，只带认识且必要的字段
//! 2. 响应是上游的 embeddings 形状（`data[].embedding`），网关不加工
//! 3. usage 回填账本；日志按真实入口记账
//! 4. 拒绝而不是猜：缺 model / 空 input / 形状不对的 input 一律 400；
//!    订阅号（非 OpenAI 兼容线）记 `no_embeddings_endpoint`，不打上游
//!
//! 注意只有一个 #[test]：`FREEPORT_DATA_DIR` 是进程级环境变量，
//! 同一二进制里两个测试并行会互相踩配置（store 那条已踩过的坑）。

use std::sync::{Arc, Mutex};

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
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
}

fn provider(id: &str, base: String, models: &[&str]) -> Value {
    json!({
        "id": id,
        "name": id,
        "baseUrl": base,
        "tier": "A",
        "keyMask": "test••••key",
        "enabled": true,
        "addedAt": "2026-10-19T00:00:00Z",
        "models": models.iter().map(|m| json!({ "id": m, "ownedBy": id })).collect::<Vec<_>>(),
        "health": { "status": "unknown" }
    })
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

const EMBEDDING_OK: &str = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}],"model":"text-embed-1","usage":{"prompt_tokens":4,"total_tokens":4}}"#;

#[tokio::test]
async fn embeddings_endpoint_forwards_and_settles_usage() {
    let dir = std::env::temp_dir().join(format!("fp-emb-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let seen: Arc<Mutex<Vec<Value>>> = Default::default();
    let sink = seen.clone();
    let upstream = Router::new().route(
        "/v1/embeddings",
        post(move |Json(body): Json<Value>| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(body);
                Json(serde_json::from_str::<Value>(EMBEDDING_OK).unwrap()).into_response()
            }
        }),
    );
    let base = spawn(upstream).await;

    // claude-sub 是内置的 Anthropic 原生线：embeddings 没有它的份
    let mut sub = provider("claude-sub", "http://127.0.0.1:9".into(), &["text-embed-1"]);
    sub["tier"] = json!("C");
    write_config(
        &dir,
        vec![
            provider("a", format!("{base}/v1"), &["text-embed-1", "text-embed-2"]),
            sub,
        ],
    );
    freeport_gateway::secrets::set_key("a", "k").unwrap();
    freeport_gateway::secrets::set_key("claude-sub", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let ep = format!("http://127.0.0.1:{port}/v1/embeddings");

    /* ── 正常转发 ── */
    let res = client
        .post(&ep)
        .header("authorization", "Bearer whatever")
        .json(&json!({
            "model": "a:text-embed-1",
            "input": "hello world",
            "dimensions": 3,
            "encoding_format": "float",
            "user": "u-1"
        }))
        .send()
        .await
        .unwrap();

    let status = res.status();
    let provider_hdr = res
        .headers()
        .get("x-freeport-provider")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "转发失败：{body}");
    assert_eq!(provider_hdr, "a");
    assert_eq!(body["object"], "list", "上游形状原样透出");
    assert_eq!(body["data"][0]["embedding"], json!([0.1, 0.2, 0.3]));
    assert_eq!(body["usage"]["total_tokens"], 4);

    /* ── 上游收到的是裁剪过的请求 ── */
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["model"], "text-embed-1", "要换成候选的模型名");
        assert_eq!(seen[0]["input"], "hello world");
        assert_eq!(seen[0]["dimensions"], 3);
        assert_eq!(seen[0]["encoding_format"], "float");
        assert!(
            seen[0].get("user").is_none(),
            "没认过的字段不该透给上游：多一个键就是一次 400 换家"
        );
    }

    /* ── 订阅号（原生线）被跳过，一次上游调用都没有 ── */
    let res = client
        .post(&ep)
        .header("authorization", "Bearer whatever")
        .json(&json!({ "model": "claude-sub:text-embed-1", "input": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 502, "唯一候选被跳过：没有能打的了");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "订阅号不该被真的打一次 embeddings"
    );

    /* ── 拒绝而不是猜 ── */
    for (bad, code) in [
        (json!({ "input": "hi" }), "missing_model"),
        (json!({ "model": "a:text-embed-1", "input": "" }), "unsupported_parameter"),
        (json!({ "model": "a:text-embed-1" }), "unsupported_parameter"),
        (
            json!({ "model": "a:text-embed-1", "input": [{ "text": "hi" }] }),
            "unsupported_parameter",
        ),
    ] {
        let res = client
            .post(&ep)
            .header("authorization", "Bearer whatever")
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "应当拒绝：{bad}");
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["error"]["code"], code, "错误码要对：{bad}");
    }

    /* ── usage 回填账本 + 日志按真实入口记账 ── */
    let observed = gw.observed(24).await;
    // 观测按日志条数统计：转发成功 1 单 + 订阅号被跳过 1 单
    assert_eq!(observed.requests, 2);
    assert_eq!(observed.ok, 1);
    assert_eq!(observed.errors, 1);

    let logs = gw.logs().await;
    assert!(logs.iter().any(|l| l.path == "/v1/embeddings" && l.ok));
    let skip_log = logs
        .iter()
        .find(|l| l.requested_model == "claude-sub:text-embed-1")
        .expect("订阅号那单要留日志");
    assert_eq!(skip_log.attempts[0].kind, "no_embeddings_endpoint");
    assert_eq!(skip_log.attempts[0].status, 0, "没打上游就没状态码");

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
