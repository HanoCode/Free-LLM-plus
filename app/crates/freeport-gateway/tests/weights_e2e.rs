//! 三维权重（可靠性 / 速度 / 智能）的端到端闸门。
//!
//! 这条链路容易断在三处，且每一处都「看起来正常」：
//! 1. `set_routing` 收下权重却没落盘 —— 重启后静默恢复成旧公式；
//! 2. 落盘了但 `order()` 没读 —— 界面上权重条在动，排序岿然不动；
//! 3. 只把评分发给界面、没真的乘进 weight —— 数字好看，行为没变。
//!
//! 所以断言必须打在 `/v1/route` 排出来的**第一条**上，而不是发回来的配置里。

use serde_json::{json, Value};

fn write_config(dir: &std::path::Path) {
    std::fs::write(
        dir.join("freeport.json"),
        json!({
            "version": 1,
            "gateway": { "port": 0, "host": "127.0.0.1" },
            "providers": [
                {
                    "id": "slow",
                    "name": "Slow",
                    "baseUrl": "http://127.0.0.1:9/v1",
                    "tier": "A",
                    "keyMask": "sk-slow",
                    "enabled": true,
                    "accounts": [{
                        "id": "acct_slow",
                        "name": "default",
                        "keyMask": "sk-slow",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:00Z"
                    }],
                    "models": [{ "id": "steady", "ownedBy": "slow" }],
                    "health": { "status": "healthy" }
                },
                {
                    "id": "fast",
                    "name": "Fast",
                    "baseUrl": "http://127.0.0.1:9/v1",
                    "tier": "A",
                    "keyMask": "sk-fast",
                    "enabled": true,
                    "accounts": [{
                        "id": "acct_fast",
                        "name": "default",
                        "keyMask": "sk-fast",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:01Z"
                    }],
                    // 会推理、上下文大，但被探针标成慢机（degraded）
                    "models": [{
                        "id": "quick",
                        "ownedBy": "fast",
                        "context": 128000,
                        "caps": { "reasoning": true, "tools": true }
                    }],
                    "health": { "status": "degraded" }
                }
            ],
            "modelHealth": {
                "slow:steady": { "status": "healthy", "latencyMs": 2000 },
                "fast:quick": { "status": "degraded", "latencyMs": 80 }
            },
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
}

async fn chain_head(client: &reqwest::Client, port: u16) -> Value {
    let res = client
        .get(format!("http://127.0.0.1:{port}/v1/route?model=auto"))
        .header("authorization", "Bearer legacy")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v = res.json::<Value>().await.unwrap();
    v["chain"][0].clone()
}

#[tokio::test]
async fn weights_persist_and_reorder_the_chain() {
    let dir = std::env::temp_dir().join(format!("fp-weights-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    write_config(&dir);
    freeport_gateway::secrets::set_key("acct_slow", "key-slow").unwrap();
    freeport_gateway::secrets::set_key("acct_fast", "key-fast").unwrap();

    let gw = freeport_gateway::Gateway::new();
    gw.load().await;
    let port = gw.start().await.unwrap();
    let client = reqwest::Client::new();

    // 0) 未启用：旧公式只看健康，稳的那个先上，且不报 blend
    let head = chain_head(&client, port).await;
    assert_eq!(head["provider"], "slow", "{head}");
    assert!(
        head["factors"]["blend"].is_null(),
        "没启用权重就不该有合成评分：{head}"
    );

    // 1) 可靠性 70：仍然稳的先上
    gw.set_routing(freeport_core::dto::RoutingPatch {
        strategy: None,
        chain: None,
        chains: None,
        purpose_chain: None,
        weights: Some(freeport_core::model::RouteWeights {
            enabled: true,
            reliability: 70,
            speed: 15,
            intelligence: 15,
        }),
    })
    .await;
    let head = chain_head(&client, port).await;
    assert_eq!(head["provider"], "slow", "{head}");
    assert!(
        head["factors"]["blend"].as_f64().unwrap() > 0.5,
        "健康又稳的号评分不该低：{head}"
    );

    // 2) 速度压满：慢机标签不再是死刑，快的那个必须顶上来
    gw.set_routing(freeport_core::dto::RoutingPatch {
        strategy: None,
        chain: None,
        chains: None,
        purpose_chain: None,
        weights: Some(freeport_core::model::RouteWeights {
            enabled: true,
            reliability: 0,
            speed: 100,
            intelligence: 0,
        }),
    })
    .await;
    let head = chain_head(&client, port).await;
    assert_eq!(head["provider"], "fast", "速度压满时该选快的：{head}");

    // 3) 落盘断言：重启（重新 load）后仍是用户设的那份，而不是默认值
    let raw = std::fs::read_to_string(dir.join("freeport.json")).unwrap();
    assert!(raw.contains("\"weights\""), "权重必须落盘：{raw}");
    let back = freeport_gateway::Gateway::new();
    back.load().await;
    let r = back.routing().await;
    assert!(r.weights.enabled);
    assert_eq!(r.weights.speed, 100);

    // 4) 关掉即回旧公式：不能留下「关了一半」的状态
    gw.set_routing(freeport_core::dto::RoutingPatch {
        strategy: None,
        chain: None,
        chains: None,
        purpose_chain: None,
        weights: Some(freeport_core::model::RouteWeights {
            enabled: false,
            reliability: 70,
            speed: 15,
            intelligence: 15,
        }),
    })
    .await;
    let head = chain_head(&client, port).await;
    assert_eq!(head["provider"], "slow", "关掉权重应退回「只看健康」：{head}");
    assert!(head["factors"]["blend"].is_null(), "{head}");

    gw.stop().await;
}
