//! 会话粘性的端到端验证。
//!
//! 要证明两件事：
//! 1. 同一段对话的第二轮仍然落在**上一轮答它的那家**（保住 prompt cache）；
//! 2. 换一段对话时，SWRR 会把它轮转到另一家 —— 否则「粘性」可能只是「从来只选一家」。
//!
//! 两个请求都打同一个 mock 上游，所以差异只可能来自路由本身。

use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

fn provider(id: &str, base: &str) -> Value {
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
async fn same_conversation_sticks_and_new_one_rotates() {
    let dir = std::env::temp_dir().join(format!("fp-affinity-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            axum::Json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "ok" } }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
            }))
        }),
    );
    let base = spawn(upstream).await;

    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": [provider("pa", &base), provider("pb", &base)],
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
    freeport_gateway::secrets::set_key("pa", "k").unwrap();
    freeport_gateway::secrets::set_key("pb", "k").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");

    let ask = |text: &str| {
        let body = json!({
            "model": "auto",
            "messages": [{ "role": "user", "content": text }]
        });
        client.post(&url).json(&body)
    };
    let who = |res: &reqwest::Response| {
        res.headers()
            .get("x-freeport-provider")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };

    // 第一轮：SWRR 从等权的两家里挑一个（平手按 id 定序 → pa）
    let first = ask("这段对话的第一句").send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let home = who(&first);
    assert!(!home.is_empty(), "响应必须带 x-freeport-provider");

    // 第二轮：同一段对话（多了一条 assistant 消息），必须回到第一家
    let second = client
        .post(&url)
        .json(&json!({
            "model": "auto",
            "messages": [
                { "role": "user", "content": "这段对话的第一句" },
                { "role": "assistant", "content": "ok" },
                { "role": "user", "content": "继续说" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        who(&second),
        home,
        "同一段对话必须粘在上一轮答它的那家，否则 prompt cache 白丢"
    );

    // 第三轮：换一段全新对话，SWRR 应当轮到另一家
    let third = ask("一段完全不同的新对话").send().await.unwrap();
    assert_eq!(third.status(), StatusCode::OK);
    assert_ne!(
        who(&third),
        home,
        "新对话不该被旧会话的粘性带走，否则等于退化成单provider"
    );

    // 调试接口必须能解释「为什么它排在这里」：三因子、余量、健康都在
    let dbg: Value = client
        .get(format!("http://127.0.0.1:{port}/v1/route?model=auto"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let chain = dbg.get("chain").and_then(|v| v.as_array()).expect("chain 必须有");
    assert_eq!(chain.len(), 2, "两家都要在解释里出现");
    for w in chain {
        assert!(w.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
        assert!(w.get("health").and_then(|v| v.as_str()).is_some());
        let f = w.get("factors").expect("factors 必须摊开");
        let prod = f.get("tier").and_then(|v| v.as_f64()).unwrap_or(0.0)
            * f.get("headroom").and_then(|v| v.as_f64()).unwrap_or(0.0)
            * f.get("health").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let wv = w.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(
            (prod - wv).abs() < 0.01,
            "三因子相乘必须等于权重：{prod} vs {wv}"
        );
    }

    // 预演不能推进轮转：连查两次，链首不该变
    let again: Value = client
        .get(format!("http://127.0.0.1:{port}/v1/route?model=auto"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        dbg.get("chain").and_then(|v| v.as_array()).map(|a| a
            .iter()
            .filter_map(|h| h.get("provider").and_then(|p| p.as_str()))
            .collect::<Vec<_>>()),
        again.get("chain").and_then(|v| v.as_array()).map(|a| a
            .iter()
            .filter_map(|h| h.get("provider").and_then(|p| p.as_str()))
            .collect::<Vec<_>>()),
        "查调试接口不该改变链路顺序"
    );

    gw.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
