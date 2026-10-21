//! Freeport 的 MCP server（stdio）。
//!
//! 存在的理由：agent 现在只能看见「调用失败」这一个事实，看不见「为什么失败的、
//! 还有谁能接」。Freeport 恰好知道这些——它就站在所有厂商前面。把这份认知用
//! MCP 暴露出去，agent 才可能自己换一个能用的模型，而不是把 502 原样转给用户。
//!
//! 三条设计约束：
//!
//! 1. **只读**。这里不提供任何写操作——改配置必须由用户在界面里做。MCP 的调用方
//!    是一个语言模型，把「删凭据」「改路由」这类能力挂上去，等于把用户的额度
//!    交给一段可能被 prompt 注入的文本。
//! 2. **不吐密钥**。`status` 只给 `baseUrl` 与是否需要鉴权，从不返回 `authToken`。
//!    agent 需要真正发请求时，用已注入的客户端配置，不需要从我们这里拿。
//! 3. **stdout 只能是 JSON-RPC**。日志一律走 stderr（见 `src-tauri` 的 `--mcp`
//!    分支），否则客户端会在半行日志上解析失败，表现成「服务器连不上」。

use freeport_gateway::Gateway;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

pub mod tools;

const SERVER_NAME: &str = "freeport";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 我们支持的协议版本，从新到旧。
///
/// 只声明 `tools` 能力，而工具调用这套在这几个版本之间没有破坏性变化，
/// 所以客户端要哪个就回哪个；都不认识时回最新的，由客户端决定要不要断开。
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-06-18", "2025-03-26", "2024-11-05", "2024-10-07"];

const INSTRUCTIONS: &str = "\
Freeport 是本机的大模型额度聚合网关。这些工具只读，用来回答「现在能用什么模型、\
这一单会走谁、刚才为什么失败」。

- 想换模型之前先看 freeport_models：它给的是**此刻真的可用**的模型，已排除冷却中、
  不可用与已退役的条目。
- 想看某次失败的原因用 freeport_recent_errors：里面每一跳的 provider / 凭据 /
  状态码都在。
- 不要试图通过这里改配置。用户要加凭据、改路由、开关厂商，都在 Freeport 界面里做。
- 不要把网关端口暴露到局域网，也不要把它写进任何会被提交的配置文件。";

/// 跑起来，直到 stdin 关闭。宿主进程退出即退出。
pub async fn serve(gw: &Gateway) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = BufWriter::new(tokio::io::stdout());

    while let Some(line) = lines.next_line().await? {
        let Some(resp) = handle_line(gw, &line).await else {
            continue;
        };
        out.write_all(resp.as_bytes()).await?;
        out.write_all(b"\n").await?;
        // 每条都要立刻 flush：MCP 客户端是「发一条等一条」的，
        // 攒着不写会表现成客户端超时，而不是慢。
        out.flush().await?;
    }
    Ok(())
}

/// 处理一行输入，返回要写回去的那一行（`None` = 这条不需要回包）。
///
/// 抽出来是为了让协议细节能被单测覆盖：真的走 stdin/stdout 的测试只能验「通」，
/// 验不了「非法 JSON 会不会把循环打断」「通知会不会被回包」这类分叉。
pub async fn handle_line(gw: &Gateway, line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            // 没有 id 可回，按 JSON-RPC 规定用 null
            return Some(render(err(Value::Null, -32700, &format!("JSON 解析失败：{e}"))));
        }
    };

    // 批量请求在 MCP 里不用；真收到了也不能当成单条处理，否则客户端会拿到
    // 一个 id 对不上的回包，然后一直等。
    if req.is_array() {
        return Some(render(err(Value::Null, -32600, "不支持批量请求")));
    }

    // 通知（没有 id）不需要回包。`notifications/initialized` 走这里。
    let id = req.get("id").cloned()?;
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    let resp = match method {
        "initialize" => ok(id, initialize_result(&params)),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, tools::list()),
        "tools/call" => match tools::call(gw, &params).await {
            Ok(result) => ok(id, result),
            Err(tools::CallError::Unknown(name)) => {
                // 未知工具是协议层错误（客户端拼错了名字），不是「工具执行失败」
                err(id, -32602, &format!("未知工具：{name}"))
            }
            Err(tools::CallError::Failed(msg)) => ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true,
                }),
            ),
        },
        // 没声明 resources / prompts 能力，但有些客户端会先探一下再决定怎么展示，
        // 回空列表比回「方法不存在」友好，也不会让它们以为服务器坏了。
        "resources/list" => ok(id, json!({ "resources": [] })),
        "resources/templates/list" => ok(id, json!({ "resourceTemplates": [] })),
        "prompts/list" => ok(id, json!({ "prompts": [] })),
        "logging/setLevel" => ok(id, json!({})),
        other => err(id, -32601, &format!("未实现的方法：{other}")),
    };
    Some(render(resp))
}

fn initialize_result(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = match asked {
        Some(v) if PROTOCOL_VERSIONS.contains(&v) => v,
        _ => PROTOCOL_VERSIONS[0],
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "title": "Freeport", "version": VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn render(v: Value) -> String {
    // `Value` 已经保证是合法 JSON；真序列化失败也只能退回一条错误，
    // 不能让整个循环 panic —— 崩了客户端只会看到「连接中断」。
    serde_json::to_string(&v).unwrap_or_else(|e| {
        format!(
            r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32603,"message":"序列化失败：{e}"}}}}"#
        )
    })
}
