//! `/v1/route` 的池预演：它要能回答两个问题，而不只是把链路排出来。
//!
//! 1. 这个用途到底有几个号接着？——勾错标签是最常见的现场：链路全空，
//!    而「厂商」页一片绿，用户只会以为网关坏了。
//! 2. 排第一那个号余量够不够这单跑完？——Gate 只判「窗口有没有耗尽」，
//!    日窗口还剩 300 token 时一条 8k 的请求照样放行，然后撞 429。
//!
//! 这两条都是「账面上有、实际没有」的问题，只有预演能答；真实请求答的时候
//! 已经失败了。所以断言得打在预演的 JSON 上，而不是等 429。

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
                        "purposes": ["coding"],
                        // 日 token 很大：预演要测的是「这单装不下」，不是「窗口已耗尽」——
                        // 后者会被 Gate 直接拦掉，链变空，就问不出「够不够」了
                        "quota": { "rpd": 1000, "tpd": 100000 }
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

async fn upstream(seen: Arc<Mutex<Vec<String>>>) -> String {
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

#[tokio::test]
async fn preview_tells_who_answers_and_whether_the_quota_fits() {
    let dir = std::env::temp_dir().join(format!("fp-preview-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let base = upstream(seen.clone()).await;
    write_config(&dir, base);
    freeport_gateway::secrets::set_key("acct_code", "key-code").unwrap();
    freeport_gateway::secrets::set_key("acct_vis", "key-vis").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let route_url = format!("http://127.0.0.1:{port}/v1/route");
    let chat_url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let client = reqwest::Client::new();
    let preview = |q: String| {
        let client = client.clone();
        let url = format!("{route_url}?{q}");
        async move {
            let res = client
                .get(&url)
                .header("authorization", "Bearer legacy")
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{url}");
            res.json::<Value>().await.unwrap()
        }
    };

    // 1) chat 池是空的：两个号都标了别的用途。这时候链路必然为空，
    //    但预演必须说清「是标签挡的」——不然用户只会去查厂商，查不出东西。
    let v = preview("model=auto&purpose=chat".into()).await;
    assert!(v["chain"].as_array().unwrap().is_empty());
    assert_eq!(v["pool"]["accounts"]["total"], 2);
    assert_eq!(v["pool"]["accounts"]["admits"], 0);
    assert_eq!(v["pool"]["accounts"]["blocked"], 2);
    assert_eq!(v["pool"]["verdict"], "empty-pool", "{v}");
    assert!(v["pool"]["landsOn"].is_null(), "没候选就别编一个落地账号");

    // 2) 按 coding 看：一个号接、一个号在别的池里，落地账号要指名道姓
    let v = preview("model=auto&purpose=coding".into()).await;
    assert_eq!(v["pool"]["accounts"]["admits"], 1);
    assert_eq!(v["pool"]["accounts"]["blocked"], 1);
    assert_eq!(v["pool"]["landsOn"]["account"], "acct_code");
    assert_eq!(v["pool"]["landsOn"]["provider"], "p");
    assert_eq!(v["pool"]["verdict"], "ok", "{v}");
    // 没传 tokens 就不判装不装得下：猜一个数字去报红比不报更糟
    assert!(v["pool"]["fits"].is_null());
    assert!(v["pool"]["recent"].is_null(), "还没打过请求，不该编 0%");

    // 3) 打两单真实 coding 流量：预演的池战绩必须跟着动，
    //    而且要和摘要用同一份统计（否则两个数打架没人能解释）
    for _ in 0..2 {
        let res = client
            .post(&chat_url)
            .header("authorization", "Bearer legacy")
            .header("x-freeport-purpose", "coding")
            .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
    let v = preview("model=auto&purpose=coding".into()).await;
    assert_eq!(v["pool"]["recent"]["requests"], 2, "{v}");
    assert_eq!(v["pool"]["recent"]["errors"], 0);
    assert_eq!(v["pool"]["recent"]["okRate"], 1.0);

    // 4) 「余量够不够」：999999 token 装不进日窗口，必须在预演里就点出来，
    //    而不是等上游 429。
    let v = preview("model=auto&purpose=coding&tokens=999999".into()).await;
    assert_eq!(v["pool"]["fits"], false, "{v}");
    assert_eq!(v["pool"]["verdict"], "too-big", "{v}");
    assert_eq!(v["pool"]["shortfall"]["window"], "tpd");
    let left = v["pool"]["shortfall"]["left"].as_f64().unwrap();
    assert!(left > 0.0 && left < 100_000.0, "还剩 {left}");

    // 5) 装得下时不能报红：4 token 的请求对着 10 万的上限
    let v = preview("model=auto&purpose=coding&tokens=4".into()).await;
    assert_eq!(v["pool"]["fits"], true, "{v}");
    assert_eq!(v["pool"]["verdict"], "ok", "{v}");

    gw.stop().await;
}
