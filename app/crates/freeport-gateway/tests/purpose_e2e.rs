//! 账号按用途分池的端到端验证：断言的是**哪把 key 真的被发到上游**。
//!
//! 纯函数单测只能证明 `plan_with` 会过滤，证明不了「网关真的按用途挑了账号」——
//! 中间还隔着 purpose 判定（header / 图片 / Agent 身份）这一层。
//! 所以这里用桩上游把 `Authorization` 抓下来，逐个场景对：
//!
//! 1. 没标注 → 两个号都不接 chat，应该 503 并说清是标签挡的；
//! 2. 显式 `x-freeport-purpose: coding` → 走 coding 号；
//! 3. 带图（无声明）→ 走 vision 号；带图 + 显式 coding → 仍走 coding（声明是逃生舱）；
//! 4. 用注入的 `agent:*` key 发普通文本 → 零配置识别成 coding。
//!
//! 第 4 条是这套机制真正的价值：用户不需要改任何客户端配置，
//! 「IDE 里的 Agent」和「随手 curl」就自动分开了。

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
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
                "accounts": [
                    {
                        "id": "acct_code",
                        "name": "coding-only",
                        "keyMask": "sk-code",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:00Z",
                        "purposes": ["coding"]
                    },
                    {
                        "id": "acct_vis",
                        "name": "vision-only",
                        "keyMask": "sk-vis",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:01Z",
                        "purposes": ["vision"]
                    }
                ],
                "models": [{ "id": "m1", "ownedBy": "p" }],
                "health": { "status": "unknown" }
            }],
            "routing": { "chain": ["p:m1"] }
        })
        .to_string(),
    )
    .unwrap();
}

/// 桩上游：把每次请求用的 `Authorization` 记下来。
async fn auth_upstream(seen: Arc<Mutex<Vec<String>>>) -> String {
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, _: Json<Value>| {
                let seen = seen.clone();
                async move {
                    let who = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .trim_start_matches("Bearer ")
                        .to_string();
                    seen.lock().unwrap().push(who);
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

fn text_body() -> Value {
    json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] })
}

fn image_body() -> Value {
    json!({
        "model": "auto",
        "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "这是什么" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,xx" } }
        ]}]
    })
}

#[tokio::test]
async fn requests_land_on_the_account_that_admits_their_purpose() {
    let dir = std::env::temp_dir().join(format!("fp-purpose-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let base = auth_upstream(seen.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_code", "key-code").unwrap();
    freeport_gateway::secrets::set_key("acct_vis", "key-vis").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let client = reqwest::Client::new();

    // 1) 普通对话：两个号都不是 chat 池的 → 没有候选，且原因必须点明是标签
    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .json(&text_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("用途标签"), "要说清是标签挡的，实际：{msg}");
    assert!(msg.contains("chat"), "要说明这次按哪个用途处理，实际：{msg}");
    assert_eq!(body["error"]["purpose"], "chat");
    assert!(seen.lock().unwrap().is_empty(), "没有候选就不该打上游");
    // 被挡下的这一条也要留痕：不然用户只看到 503，不知道是按哪个池判的
    {
        let logs = gw.logs().await;
        let l = &logs[0];
        assert_eq!(l.purpose, freeport_core::model::Purpose::Chat);
        assert_eq!(
            l.purpose_source,
            Some(freeport_core::model::PurposeSource::Default),
            "没声明、无图、无 agent 身份 → 依据应该是 default"
        );
        assert_eq!(l.status, 503);
    }

    // 2) 显式声明 coding → 走 coding-only 那把 key
    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .header("x-freeport-purpose", "coding")
        .json(&text_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(seen.lock().unwrap().as_slice(), ["key-code"]);
    {
        // 终端排障不必回界面：trail 第一段就写明这次按哪个池、凭哪条规则走的
        let trail = res
            .headers()
            .get("x-freeport-trail")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            trail.starts_with("[coding:header]"),
            "非 chat 请求的 trail 要带池前缀，实际：{trail}"
        );
        let logs = gw.logs().await;
        assert_eq!(logs[0].purpose.as_str(), "coding");
        assert_eq!(
            logs[0].purpose_source.as_ref().map(|s| s.as_str()),
            Some("header"),
            "日志必须说清这个池是用户点名的，而不是猜的"
        );
    }

    // 3) 带图 → 走 vision-only。这里**不带** x-freeport-purpose：
    //    图片是请求的客观属性，应该盖过「没有身份线索 → chat」的兜底。
    seen.lock().unwrap().clear();
    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .json(&image_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(seen.lock().unwrap().as_slice(), ["key-vis"]);
    {
        let logs = gw.logs().await;
        assert_eq!(logs[0].purpose.as_str(), "vision");
        assert_eq!(logs[0].purpose_source.as_ref().map(|s| s.as_str()), Some("image"));
    }

    // 3b) 显式声明是逃生舱：写了 coding 就连图片也归 coding。
    //     第 1 顺位存在的意义就是「用户说了算」，不能因为它和图片冲突就无视它。
    seen.lock().unwrap().clear();
    let res = client
        .post(&url)
        .header("authorization", "Bearer legacy")
        .header("x-freeport-purpose", "coding")
        .json(&image_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["key-code"],
        "显式声明必须压过自动推断，包括图片"
    );

    // 4) 已注入 Agent 的 key + 普通文本 → 零配置判成 coding
    seen.lock().unwrap().clear();
    let created = gw
        .create_key(freeport_core::dto::KeyInput {
            name: "agent:claude-code".into(),
            rpm_limit: None,
            daily_limit: None,
            ..Default::default()
        })
        .await
        .expect("要能建出虚拟 key");
    let res = client
        .post(&url)
        .header("authorization", format!("Bearer {}", created.secret))
        .json(&text_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["key-code"],
        "agent 身份必须自动落进 coding 池，不需要任何客户端配置"
    );
    {
        let logs = gw.logs().await;
        assert_eq!(logs[0].purpose.as_str(), "coding");
        assert_eq!(
            logs[0].purpose_source.as_ref().map(|s| s.as_str()),
            Some("agent-key"),
            "零配置识别要能自证：日志上要写清是 agent key 判出来的，而不是用户声明"
        );
    }

    // 5) 摘要里的池战绩必须对得上真实流量 —— 这是「我的 coding 号是不是挂了」的数据源。
    //    统计不出来，界面和 MCP 就都只能说「没有异常」。
    {
        use freeport_core::model::Purpose;
        let dg = gw.digest().await;
        assert_eq!(dg.log_scanned, 5, "五次请求都该进样本：{:?}", dg.pools);

        let coding = dg
            .pools
            .iter()
            .find(|p| p.purpose == Purpose::Coding)
            .expect("coding 池应有记录");
        assert_eq!(coding.requests, 3, "header 一单 + 带图声明一单 + agent key 一单");
        assert_eq!(coding.errors, 0);

        let vision = dg
            .pools
            .iter()
            .find(|p| p.purpose == Purpose::Vision)
            .expect("vision 池应有记录");
        assert_eq!(vision.requests, 1);
        assert_eq!(vision.ok_rate, Some(1.0));

        // 那条 503 是 chat 池的请求，而且必须算失败——不能因为「没打上游」就当没发生
        let chat = dg
            .pools
            .iter()
            .find(|p| p.purpose == Purpose::Chat)
            .expect("503 那条也是 chat 池的请求");
        assert_eq!(chat.errors, 1);
        assert!(
            !dg.attention.iter().any(|s| s.contains("池最近")),
            "样本小且没有池挂掉时不该报警：{:?}",
            dg.attention
        );
    }

    gw.stop().await;
}
