//! MCP 协议层与工具行为的 e2e。
//!
//! 不走真的 stdin/stdout：那只能验「连得上」，验不了「非法 JSON 会不会把循环打断」
//! 「通知会不会被回包」这类真正会让客户端卡死的分叉。这里直接喂 `handle_line`，
//! 断言写回去的那一行。

use std::path::{Path, PathBuf};

use freeport_gateway::Gateway;
use serde_json::{json, Value};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("freeport-mcp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 两份配置：alpha 有一条支持 tools 的模型，beta 那条已经连续挂了 7 次。
fn seed(dir: &Path) {
    let data = json!({
        "providers": [
            {
                "id": "alpha",
                "name": "Alpha",
                "baseUrl": "https://alpha.example/v1",
                "tier": "A",
                "enabled": true,
                "accounts": [
                    { "id": "acct_alpha", "name": "main", "keyMask": "sk-••••1111", "enabled": true }
                ],
                "models": [
                    { "id": "alpha-tools", "context": 128000, "free": true, "caps": { "tools": true } },
                    { "id": "alpha-plain", "context": 32000 }
                ]
            },
            {
                "id": "beta",
                "name": "Beta",
                "baseUrl": "https://beta.example/v1",
                "tier": "B",
                "enabled": true,
                "accounts": [
                    { "id": "acct_beta", "name": "main", "keyMask": "sk-••••2222", "enabled": true }
                ],
                "models": [
                    { "id": "beta-dead", "context": 8000 }
                ]
            }
        ],
        "modelHealth": {
            "alpha:alpha-tools": { "status": "healthy", "caps": { "tools": true }, "latencyMs": 420 },
            "alpha:alpha-plain": { "status": "healthy" },
            "beta:beta-dead": { "status": "unavailable", "fails": 7 }
        }
    });
    let parsed: freeport_core::model::AppData = serde_json::from_value(data).unwrap();
    assert!(freeport_gateway::store::save_to(dir, &parsed), "写测试配置失败");
}

async fn gw(tag: &str) -> (Gateway, PathBuf) {
    let dir = temp_dir(tag);
    seed(&dir);
    let gw = Gateway::new();
    gw.load_from(&dir).await;
    (gw, dir)
}

async fn rpc(gw: &Gateway, v: Value) -> Value {
    let line = freeport_mcp::handle_line(gw, &v.to_string())
        .await
        .expect("这条请求应该有回包");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("回包不是 JSON：{e} / {line}"))
}

async fn call(gw: &Gateway, name: &str, args: Value) -> Value {
    let resp = rpc(
        gw,
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": name, "arguments": args } }),
    )
    .await;
    assert!(resp.get("error").is_none(), "{name} 报错了：{resp}");
    let result = &resp["result"];
    assert_eq!(result["isError"], false, "{name} 执行失败：{result}");
    // 工具结果是一段 JSON 文本；解析回来才能断言内容
    let text = result["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{name} 输出不是 JSON：{e}"))
}

#[tokio::test]
async fn initialize_negotiates_a_version_we_actually_implement() {
    let (gw, _dir) = gw("init").await;

    let resp = rpc(
        &gw,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                            "clientInfo": { "name": "test", "version": "0" } } }),
    )
    .await;

    let r = &resp["result"];
    assert_eq!(r["protocolVersion"], "2024-11-05", "客户端要的版本我们支持，就该回它");
    assert_eq!(r["serverInfo"]["name"], "freeport");
    assert_eq!(r["capabilities"]["tools"]["listChanged"], false);
    assert!(
        r["instructions"].as_str().unwrap().contains("只读"),
        "instructions 必须把「只读」说清楚，否则模型会试着改配置"
    );

    // 客户端要一个我们没实现的版本：回我们最新的，由它决定要不要断开
    let resp = rpc(
        &gw,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" } }),
    )
    .await;
    assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
}

#[tokio::test]
async fn notifications_get_no_reply_and_garbage_does_not_kill_the_loop() {
    let (gw, _dir) = gw("proto").await;

    // `notifications/initialized` 没有 id，回包会让客户端以为我们发了请求
    assert!(
        freeport_mcp::handle_line(&gw, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );
    assert!(freeport_mcp::handle_line(&gw, "   ").await.is_none());

    // 半行 JSON：必须回 -32700 而不是 panic（panic 在客户端看来是「连接中断」）
    let bad: Value =
        serde_json::from_str(&freeport_mcp::handle_line(&gw, "{\"jsonrpc\":").await.unwrap())
            .unwrap();
    assert_eq!(bad["error"]["code"], -32700);
    assert_eq!(bad["id"], Value::Null);

    // 批量请求：MCP 不用，但收到也不能按单条处理
    let batch: Value =
        serde_json::from_str(&freeport_mcp::handle_line(&gw, "[{\"jsonrpc\":\"2.0\",\"id\":1}]")
            .await
            .unwrap())
        .unwrap();
    assert_eq!(batch["error"]["code"], -32600);

    // 未知方法
    let resp = rpc(&gw, json!({ "jsonrpc": "2.0", "id": 3, "method": "sampling/createMessage" })).await;
    assert_eq!(resp["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_list_exposes_exactly_the_read_only_tools() {
    let (gw, _dir) = gw("list").await;

    let resp = rpc(&gw, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "freeport_digest",
            "freeport_models",
            "freeport_quota",
            "freeport_recent_errors",
            "freeport_route",
            "freeport_status",
        ],
        "工具清单变了要同步改这条断言——它同时也是「没有偷偷加写操作」的闸门"
    );
    for t in tools {
        assert!(t["inputSchema"]["type"] == "object", "每个工具都要有对象入参");
    }

    // 客户端探 resources 时给空列表，而不是报方法不存在
    let resp = rpc(&gw, json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" })).await;
    assert_eq!(resp["result"]["resources"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn models_hides_what_cannot_serve_right_now() {
    let (gw, _dir) = gw("models").await;

    let out = call(&gw, "freeport_models", json!({})).await;
    let ids: Vec<&str> = out["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();

    assert!(ids.contains(&"alpha:alpha-tools"));
    assert!(ids.contains(&"alpha:alpha-plain"));
    assert!(
        !ids.contains(&"beta:beta-dead"),
        "不可用的模型不该出现在「现在能用什么」里"
    );
    assert_eq!(out["skippedUnhealthy"], 1);
    assert!(
        !ids.contains(&"freeport/auto") && !ids.iter().any(|i| *i == "alpha-tools"),
        "虚拟模型与裸 id 副本是噪音：同一件事只该出现一次"
    );

    // 显式要求看不可用的（排查用）
    let out = call(&gw, "freeport_models", json!({ "includeUnavailable": true })).await;
    let ids: Vec<&str> = out["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"beta:beta-dead"));
}

#[tokio::test]
async fn capability_filter_treats_unknown_as_not_supported() {
    let (gw, _dir) = gw("caps").await;

    let out = call(&gw, "freeport_models", json!({ "capability": "tools" })).await;
    let ids: Vec<&str> = out["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();

    assert_eq!(ids, vec!["alpha:alpha-tools"]);
    // 未知 ≠ 支持：alpha-plain 没声明 tools，就不能被推荐去做工具调用
    assert!(!ids.contains(&"alpha:alpha-plain"));
}

#[tokio::test]
async fn status_never_leaks_the_gateway_token() {
    let (gw, _dir) = gw("status").await;

    let resp = rpc(
        &gw,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "freeport_status", "arguments": {} } }),
    )
    .await;
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("authToken") && !text.contains("auth_token"),
        "状态里绝不能带出网关密钥：MCP 的调用方是一个语言模型"
    );
    let out: Value = serde_json::from_str(text).unwrap();
    assert_eq!(out["running"], false, "测试里没有 start()，网关不可能是运行态");
}

#[tokio::test]
async fn quota_and_digest_answer_without_a_running_gateway() {
    let (gw, _dir) = gw("quota").await;

    let out = call(&gw, "freeport_quota", json!({})).await;
    let accounts = out["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2, "两条凭据各一行");
    // 四个窗口都要在，界面上是四根条
    assert_eq!(accounts[0]["windows"].as_array().unwrap().len(), 4);
    assert!(accounts[0]["windows"][0].get("resetAtIso").is_some());

    let out = call(&gw, "freeport_digest", json!({})).await;
    assert!(out["headline"].is_string(), "摘要至少要有一句话");
}

#[tokio::test]
async fn recent_errors_is_empty_but_explains_why() {
    let (gw, _dir) = gw("errors").await;

    let out = call(&gw, "freeport_recent_errors", json!({})).await;
    assert_eq!(out["count"], 0);
    assert!(
        out["hint"].as_str().unwrap().contains("进程内"),
        "空结果要说清是「进程内日志」，否则用户会以为历史错误被吞了"
    );
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error_not_an_empty_result() {
    let (gw, _dir) = gw("unknown").await;

    let resp = rpc(
        &gw,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "freeport_delete_everything", "arguments": {} } }),
    )
    .await;
    assert_eq!(resp["error"]["code"], -32602);
}

#[tokio::test]
async fn route_preview_reports_something_for_a_known_model() {
    let (gw, _dir) = gw("route").await;

    let out = call(&gw, "freeport_route", json!({ "model": "alpha:alpha-tools" })).await;
    // 预演是纯读：字段具体长什么样由 core 决定，这里只保证「有链路信息可看」
    assert!(out.is_object() && !out.as_object().unwrap().is_empty());
}

/// 「独立进程也能看到实时状态」——这是 `/v1/freeport/inspect` 存在的唯一理由。
///
/// 用两个 `Gateway` 实例模拟两个进程：一个起服务，一个只加载配置（MCP 就是后者）。
/// 断言点挑的是**磁盘上根本不存在**的东西——`paused` 是纯内存态。
/// 如果 MCP 读到 `paused = true`，那它一定不是从磁盘快照里编出来的。
#[tokio::test]
async fn a_second_instance_sees_the_live_memory_state_of_the_running_gateway() {
    let dir = temp_dir("live");
    seed(&dir);

    // 端口交给内核分配，测试之间不抢 8787
    let mut cfg = freeport_gateway::store::load_from(&dir);
    cfg.gateway.port = 0;
    assert!(freeport_gateway::store::save_to(&dir, &cfg));

    let runner = Gateway::new();
    runner.load_from(&dir).await;
    let port = runner.start().await.unwrap();

    // 宿主启动后会把真实端口写回配置；这一步是「另一个进程怎么找到我」的全部秘密
    let mut cfg = freeport_gateway::store::load_from(&dir);
    cfg.gateway.port = port;
    assert!(freeport_gateway::store::save_to(&dir, &cfg));

    // MCP：只加载，不起网关
    let mcp = Gateway::new();
    mcp.load_from(&dir).await;

    runner.set_paused(true);
    let st = call(&mcp, "freeport_status", json!({})).await;
    assert_eq!(st["source"], "gateway", "应当走了实时接口：{st}");
    assert_eq!(st["gatewayOnline"], true);
    assert_eq!(st["port"].as_u64(), Some(port as u64));
    assert_eq!(
        st["paused"], true,
        "paused 只在内存里，磁盘快照读不到——能读到它就说明这是实时视图"
    );

    // 失败日志也应当从那边来（哪怕现在是空的，也要标明来源）
    let errs = call(&mcp, "freeport_recent_errors", json!({})).await;
    assert_eq!(errs["source"], "gateway");
    assert_eq!(errs["gatewayOnline"], true);

    // 打一单进来（没有凭据，必然 503），日志会落在**网关**内存里。
    // MCP 自己的日志环是空的——摘要里的池战绩必须跨进程从那边取，
    // 否则用户会看到「今天没有任何请求」，而事实是刚才才失败过一条。
    //
    // 先解除暂停：暂停中间件会在进入 chat() 之前就把请求挡掉，那样一条日志都不会产生。
    runner.set_paused(false);
    let res = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&json!({ "model": "auto", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .expect("要能打到运行中的网关");
    assert_ne!(res.status(), reqwest::StatusCode::OK);

    let dg = call(&mcp, "freeport_digest", json!({})).await;
    assert!(
        dg["logScanned"].as_u64().unwrap_or(0) >= 1,
        "摘要要看到活网关的日志，而不是本进程那个空环：{dg}"
    );
    let pools = dg["pools"].as_array().cloned().unwrap_or_default();
    let chat = pools
        .iter()
        .find(|p| p["purpose"] == "chat")
        .unwrap_or_else(|| panic!("这条 503 应记进 chat 池：{dg}"));
    assert!(chat["requests"].as_u64().unwrap_or(0) >= 1);
    assert!(
        chat["errors"].as_u64().unwrap_or(0) >= 1,
        "失败必须记成失败，不能因为「没打上游」就当没发生：{chat}"
    );

    runner.set_paused(false);
    runner.stop().await;
    // 关掉之后必须退回磁盘快照，并且**明说**自己看不到实时状态
    let after = call(&mcp, "freeport_status", json!({})).await;
    assert_eq!(after["source"], "disk");
    assert_eq!(after["gatewayOnline"], false);
    assert!(
        after["hint"].as_str().unwrap().contains("连不上网关"),
        "离线时必须说清楚，别把「看不到」写成「没有」：{after}"
    );

    // 摘要同理：连不上网关时池战绩必然空，必须自己说明「空 ≠ 各池正常」
    let dg_off = call(&mcp, "freeport_digest", json!({})).await;
    assert_eq!(dg_off["logScanned"].as_u64(), Some(0));
    assert!(
        dg_off["poolHint"].as_str().unwrap_or("").contains("网关可能没在运行"),
        "空统计要主动解释，否则会被读成「每个池都健康」：{dg_off}"
    );
}
