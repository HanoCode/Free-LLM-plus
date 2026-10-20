//! 两种「变量表」形态：同一份 Claude 环境变量，装进不同的容器。
//!
//! - VS Code 扩展要 `claudeCode.environmentVariables: [{name, value}]`
//! - JetBrains ACP 要 `agent_servers.<名字>.env: {NAME: value}`
//!
//! 两者写的是**同一份** [`spec::CLAUDE_KEYS`]，所以不各抄一遍：`ptr` 在这里只用来
//! 取最后一段当变量名（`/env/ANTHROPIC_BASE_URL` → `ANTHROPIC_BASE_URL`）。
//! 抄两份的代价是往后再加一个变量，得记得改三处——总有一处会被忘掉。
//!
//! [`spec::CLAUDE_KEYS`]: crate::spec::CLAUDE_KEYS

use serde_json::{json, Map, Value};

use crate::journal;
use crate::spec::KeySpec;
use crate::{json_edit, value_of, Ctx};

/// `/env/ANTHROPIC_MODEL` → `ANTHROPIC_MODEL`
fn env_name(k: &KeySpec) -> &'static str {
    k.ptr.rsplit('/').next().unwrap_or(k.ptr)
}

fn parse(old: &str) -> Result<Value, String> {
    if old.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(old)
        .map_err(|_| "目标文件不是合法 JSON，为免写坏已停止（可先手工整理格式）".to_string())
}

fn finish(doc: &Value) -> Result<String, String> {
    let mut out = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

/* ── VS Code 扩展（settings.json）─────────────── */

/// 写 `claudeCode.environmentVariables`，保留用户自己加的其他变量。
///
/// 为什么不能整组替换：这个数组是用户手写过的（比如他自己加的代理变量），
/// 整组覆盖等于把别人的配置删了。按 `name` 认领我们那几条，其余原样留在原位，
/// 我们的一律追加在末尾——顺序变了不影响 VS Code，但用户的阅读顺序不该被动。
pub fn vscode_extension(
    old: &str,
    keys: &[KeySpec],
    ctx: &Ctx,
) -> Result<(String, Vec<journal::Prev>), String> {
    let mut doc = parse(old)?;
    // 注意这里是**扁平键** `claudeCode.environmentVariables`，不是嵌套对象。
    // VS Code 的 settings.json 用带点号的扁平键；写成 `{"claudeCode":{...}}`
    // 扩展一个字都读不到，而且不会报错——补 e2e 时才发现写错了一层。
    let ptr = "/claudeCode.environmentVariables";
    let before = json_edit::get(&doc, ptr).cloned();

    let ours: Vec<&str> = keys.iter().map(env_name).collect();
    let mut list: Vec<Value> = match before.as_ref() {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .filter(|e| {
                !e.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| ours.contains(&n))
            })
            .cloned()
            .collect(),
        // 形状不对就停：把别人的对象写成数组，扩展会整块配置失效
        Some(_) => return Err("claudeCode.environmentVariables 不是数组，停止写入".into()),
    };
    for k in keys {
        list.push(json!({ "name": env_name(k), "value": value_of(k.value, ctx) }));
    }
    json_edit::set(&mut doc, ptr, Value::Array(list));

    let mut prev = vec![journal::Prev {
        ptr: ptr.to_string(),
        value: before.unwrap_or(Value::Null),
    }];

    // 不关掉登录提示，扩展会先弹一次「请登录 Anthropic」，用户以为没注入成功
    let prompt_ptr = "/claudeCode.disableLoginPrompt";
    let old_prompt = json_edit::set(&mut doc, prompt_ptr, json!(true));
    prev.push(journal::Prev {
        ptr: prompt_ptr.into(),
        value: old_prompt.unwrap_or(Value::Null),
    });

    Ok((finish(&doc)?, prev))
}

/* ── JetBrains ACP（acp.json）────────────────── */

/// 往**已存在**的 ACP agent 条目里注 env。
///
/// 只注 env、不建条目：ACPs 的 `command` / `args` 指向具体的 node 与 adapter 脚本，
/// 这两个路径每台机器都不一样。猜一个写进去，JetBrains 会拉不起来，
/// 而错误只会显示在 IDE 的日志里——用户根本不知道是我们写坏的。
pub fn acp_servers(
    old: &str,
    keys: &[KeySpec],
    ctx: &Ctx,
    marker: &str,
) -> Result<(String, Vec<journal::Prev>), String> {
    let mut doc = parse(old)?;
    let Some(servers) = doc.get("agent_servers").and_then(Value::as_object) else {
        return Err(
            "acp.json 里没有 agent_servers，先在 JetBrains 里装好 Claude Agent 并启动一次".into(),
        );
    };
    let name = pick(servers, marker)?;
    let base = format!("/agent_servers/{name}");

    let mut prev = Vec::new();
    for k in keys {
        let ptr = format!("{base}/env/{}", env_name(k));
        let before = json_edit::set(&mut doc, &ptr, value_of(k.value, ctx));
        prev.push(journal::Prev {
            ptr,
            value: before.unwrap_or(Value::Null),
        });
    }
    // 认领标记：下次注入靠它认出哪个条目是我们的，回滚也能确认没认错人
    let ptr = format!("{base}/env/{marker}");
    let before = json_edit::set(&mut doc, &ptr, json!("1"));
    prev.push(journal::Prev {
        ptr,
        value: before.unwrap_or(Value::Null),
    });

    Ok((finish(&doc)?, prev))
}

/// 挑出要注的那个 agent 条目。
///
/// 三条，越靠后越保守：
/// 1. 带我们标记的（第二次注入）
/// 2. 名字 / command / args 里带 `claude` 的
/// 3. 其余一律报错——宁可让用户手工确认，也不要把别的 agent 悄悄指向网关
fn pick(servers: &Map<String, Value>, marker: &str) -> Result<String, String> {
    let mut ours = Vec::new();
    let mut claude = Vec::new();
    for (name, v) in servers {
        // JSON Pointer 的 `~` / `/` 需要转义，而 json_edit 不做转义：
        // 名字里带这两个字符的条目直接跳过，不冒险写偏
        if name.contains('/') || name.contains('~') || !v.is_object() {
            continue;
        }
        let marked = v
            .get("env")
            .and_then(|e| e.get(marker))
            .and_then(Value::as_str)
            == Some("1");
        if marked {
            ours.push(name.clone());
        } else if looks_claude(name, v) {
            claude.push(name.clone());
        }
    }
    match (ours.len(), claude.len()) {
        (1, _) => Ok(ours.remove(0)),
        (0, 1) => Ok(claude.remove(0)),
        (0, 0) => {
            let mut names: Vec<&str> = servers.keys().map(String::as_str).collect();
            names.sort_unstable();
            Err(format!(
                "没找到 Claude Agent 条目（现有：{}）；在 JetBrains 里装好 Claude Agent 并启动一次再回来",
                names.join("、")
            ))
        }
        (0, n) => Err(format!(
            "有 {n} 个条目像 Claude Agent（{}），先手工确认在用的那个",
            claude.join("、")
        )),
        _ => Err(format!(
            "有多个条目带着我们的标记（{}），先手工删掉多余的",
            ours.join("、")
        )),
    }
}

fn looks_claude(name: &str, v: &Value) -> bool {
    let hit = |s: &str| s.to_ascii_lowercase().contains("claude");
    hit(name)
        || v.get("command").and_then(Value::as_str).is_some_and(hit)
        || v.get("args")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().filter_map(Value::as_str).any(hit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec;

    fn ctx() -> Ctx<'static> {
        Ctx::new("http://127.0.0.1:8787", "fp-test-key", "auto")
    }

    const KEYS: &[KeySpec] = &[
        KeySpec {
            ptr: "/env/ANTHROPIC_BASE_URL",
            value: crate::spec::Src::Base,
        },
        KeySpec {
            ptr: "/env/ANTHROPIC_AUTH_TOKEN",
            value: crate::spec::Src::Key,
        },
    ];

    #[test]
    fn vscode_keeps_foreign_variables() {
        // 真实形状：扁平点号键。写成嵌套对象扩展读不到（曾经就是错的）
        let old =
            r#"{"claudeCode.environmentVariables":[{"name":"HTTP_PROXY","value":"http://p"}],"editor.fontSize":14}"#;
        let (out, prev) = vscode_extension(old, KEYS, &ctx()).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        let list = doc["claudeCode.environmentVariables"].as_array().unwrap();
        assert_eq!(list[0]["name"], "HTTP_PROXY", "用户的变量必须留在原位");
        assert_eq!(list[1]["name"], "ANTHROPIC_BASE_URL");
        assert_eq!(list[1]["value"], "http://127.0.0.1:8787");
        assert_eq!(doc["claudeCode.disableLoginPrompt"], true);
        assert_eq!(doc["editor.fontSize"], 14, "别的设置不许动");
        assert!(
            doc.get("claudeCode").is_none(),
            "不能多写一层嵌套对象——扩展只认扁平键"
        );
        // 回滚记的是整组旧值：数组逐项撤不掉「上次有、这次没有」的条目
        assert_eq!(prev[0].ptr, "/claudeCode.environmentVariables");
        assert_eq!(prev[0].value.as_array().unwrap().len(), 1);
    }

    #[test]
    fn vscode_is_idempotent() {
        let once = vscode_extension("", KEYS, &ctx()).unwrap().0;
        let twice = vscode_extension(&once, KEYS, &ctx()).unwrap().0;
        assert_eq!(once, twice);
    }

    #[test]
    fn vscode_refuses_non_array() {
        let err = vscode_extension(
            r#"{"claudeCode.environmentVariables":{}}"#,
            KEYS,
            &ctx(),
        )
        .unwrap_err();
        assert!(err.contains("不是数组"), "{err}");
    }

    const ACP: &str = r#"{
      "agent_servers": {
        "Gemini": {"command": "node", "args": ["gemini-acp.js"]},
        "Claude Code": {"command": "/usr/bin/node", "args": ["/x/claude-agent-acp.js"], "env": {"KEEP": "1"}}
      }
    }"#;

    #[test]
    fn acp_picks_the_claude_entry_and_keeps_its_env() {
        let (out, prev) = acp_servers(ACP, KEYS, &ctx(), "FREEPORT_ACP").unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        let env = &doc["agent_servers"]["Claude Code"]["env"];
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "fp-test-key");
        assert_eq!(env["KEEP"], "1", "条目自己的 env 不许动");
        assert_eq!(env["FREEPORT_ACP"], "1");
        assert!(
            doc["agent_servers"]["Gemini"].get("env").is_none(),
            "别的 agent 一点都不该碰"
        );
        assert!(
            !prev
                .iter()
                .any(|p| p.ptr == "/agent_servers/Claude Code/env/KEEP"),
            "没被我们改的变量不该进回滚日志"
        );
    }

    #[test]
    fn acp_is_idempotent_and_sticks_to_our_entry() {
        let once = acp_servers(ACP, KEYS, &ctx(), "FREEPORT_ACP").unwrap().0;
        let twice = acp_servers(&once, KEYS, &ctx(), "FREEPORT_ACP").unwrap().0;
        assert_eq!(once, twice, "第二次注入必须还是同一个条目");
    }

    #[test]
    fn acp_refuses_when_no_claude_entry() {
        let old = r#"{"agent_servers":{"Gemini":{"command":"node","args":["g.js"]}}}"#;
        let err = acp_servers(old, KEYS, &ctx(), "FREEPORT_ACP").unwrap_err();
        assert!(err.contains("没找到 Claude Agent"), "{err}");
        assert!(
            err.contains("Gemini"),
            "报错要把现有条目列出来，用户才知道该装什么"
        );
    }

    #[test]
    fn acp_refuses_ambiguous_entries() {
        let old = r#"{"agent_servers":{
            "Claude A":{"command":"node","args":["a.js"]},
            "Claude B":{"command":"node","args":["b.js"]}}}"#;
        let err = acp_servers(old, KEYS, &ctx(), "FREEPORT_ACP").unwrap_err();
        assert!(err.contains("2 个"), "{err}");
    }

    #[test]
    fn acp_without_servers_points_at_the_missing_step() {
        let err = acp_servers("", KEYS, &ctx(), "FREEPORT_ACP").unwrap_err();
        assert!(err.contains("启动一次"), "{err}");
        // 顺带确认这份「同一份变量表」用的是 CLAUDE_KEYS
        assert!(spec::CLAUDE_KEYS
            .iter()
            .any(|k| k.ptr == "/env/ANTHROPIC_MODEL"));
    }
}
