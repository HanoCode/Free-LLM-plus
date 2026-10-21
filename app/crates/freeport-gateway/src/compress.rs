//! 工具输出压缩。
//!
//! 一个 agent 跑久了，上下文里占地方的不是用户说的话，也不是模型的回答，而是
//! **工具的输出**：一次 `cargo build` 的 8 万字符、一次 `ls -R` 的全仓库清单。
//! 这些东西 99% 的内容对下一步决策没用，但会实打实占满 context、按 token 计费、
//! 还会把真正重要的那几行挤出模型的注意力。
//!
//! 所以这里只做一件事：**把超长的工具输出掐头去尾，中间换成一句说明**。
//!
//! 四条自我约束，比功能本身更重要：
//!
//! 1. **默认关**。压了就丢信息，不能让人在不知情的情况下开着。
//! 2. **只碰工具产出**。用户说的话、模型的回答一个字都不动——前者丢了没处找回，
//!    后者丢了模型自己都不知道。
//! 3. **必须留可见的痕迹**。替换处写清原长、丢了多少、怎么拿回来，并明确告诉模型
//!    「不要凭这段摘要臆测被省略的内容」。没有这句话，模型会把「中间省略」
//!    读成「中间没有」，然后一本正经地编。
//! 4. **省不到 20% 就不压**（算上说明文字）。留头留尾之后只剩一点点空间，
//!    压了纯属给模型添乱。
//!
//! 认两种形状：OpenAI 的 `role: "tool" | "function"`，和 Anthropic 的
//! `tool_result` 块（`content` 是字符串或 `[{type:"text"}]`）。Anthropic 客户端
//! 走 `/v1/messages` 时会被转成 OpenAI 形状再进 `chat()`，两种都认是为了
//! 「不管走哪个入口，行为一致」。

use freeport_core::model::CompressConfig;
use serde_json::Value;

/// 替换标记的开头。用它判断「这段已经压过了」，保证重复调用不会二次截断。
const MARK_PREFIX: &str = "[... Freeport 压缩";

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// 压过几段
    pub blocks: usize,
    pub chars_before: usize,
    pub chars_after: usize,
}

impl Outcome {
    pub fn changed(&self) -> bool {
        self.blocks > 0
    }

    pub fn saved(&self) -> usize {
        self.chars_before.saturating_sub(self.chars_after)
    }
}

/// 一次请求里压掉的量。写进响应头与日志，用户才能对上「省了多少」。
pub fn apply(payload: &mut Value, cfg: &CompressConfig) -> Outcome {
    let mut out = Outcome::default();
    if !cfg.enabled {
        return out;
    }
    // 参数写歪了就当没开：宁可多花 token，也不要把工具输出压成一句「已省略」。
    let keep = cfg.keep_head + cfg.keep_tail;
    if cfg.max_tool_chars == 0 || cfg.keep_head == 0 || keep >= cfg.max_tool_chars {
        return out;
    }

    let Some(msgs) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return out;
    };
    for msg in msgs.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let is_tool_msg = role == "tool" || role == "function";
        let Some(content) = msg.get_mut("content") else {
            continue;
        };
        match content {
            Value::String(s) if is_tool_msg => shrink(s, cfg, &mut out),
            Value::Array(parts) => {
                for part in parts.iter_mut() {
                    let kind = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    // `tool_result` 无论挂在谁身上都是机器产出（Anthropic 把它
                    // 放在 user 消息里）。`text` 只在 tool 消息里才动。
                    let tool_part = kind == "tool_result" || (is_tool_msg && kind == "text");
                    if !tool_part {
                        continue;
                    }
                    match part.get_mut("content") {
                        Some(Value::String(s)) => shrink(s, cfg, &mut out),
                        Some(Value::Array(inner)) => shrink_text_parts(inner, cfg, &mut out),
                        _ => {}
                    }
                    if let Some(Value::String(s)) = part.get_mut("text") {
                        shrink(s, cfg, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn shrink_text_parts(parts: &mut [Value], cfg: &CompressConfig, out: &mut Outcome) {
    for p in parts.iter_mut() {
        if p.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }
        if let Some(Value::String(s)) = p.get_mut("text") {
            shrink(s, cfg, out);
        }
    }
}

fn shrink(s: &mut String, cfg: &CompressConfig, out: &mut Outcome) {
    if s.contains(MARK_PREFIX) {
        return;
    }
    let before: Vec<char> = s.chars().collect();
    let len = before.len();
    if len <= cfg.max_tool_chars {
        return;
    }

    // 先试内容感知过滤：认出来了就按形态压，认不出来再退回头尾截断。
    // 顺序不能反——头尾截断会把 `git diff` 的 hunk 头一起砍掉，
    // 压完的文本连「改了哪个文件」都看不出来。
    if cfg.smart_filters {
        if let Some((kind, filtered)) = smart::filter(s) {
            let after = filtered.chars().count();
            // 同样的门槛：省不到 30% 不值得多一段说明
            if after * 10 <= len * 7 {
                let rebuilt = format!(
                    "{filtered}\n\n{mark}：{kind} — 原有 {len} 字符，已按内容过滤到 {after} 字符。\
                     需要完整原文请用更小的范围重新执行一次（分页 / --stat / head），\
                     不要根据这段摘要臆测被省略的部分。]",
                    mark = MARK_PREFIX
                );
                if rebuilt.chars().count() * 10 <= len * 8 {
                    out.blocks += 1;
                    out.chars_before += len;
                    out.chars_after += rebuilt.chars().count();
                    *s = rebuilt;
                    return;
                }
            }
        }
    }

    let keep = cfg.keep_head + cfg.keep_tail;
    // 省不到 30% 就是白折腾：模型照样读不完，还多了段说明文字
    if (len - keep) * 10 < len * 3 {
        return;
    }

    let head: String = before[..cfg.keep_head].iter().collect();
    let tail: String = before[len - cfg.keep_tail..].iter().collect();
    let dropped = len - keep;
    let rebuilt = format!(
        "{head}\n\n{mark}：原有 {len} 字符，此处省略中间 {dropped} 字符，只保留开头 {} 与结尾 {} 字符。\
         需要中间内容请用更小的范围重新读一次（分页 / grep / head），\
         不要根据这段摘要臆测被省略的部分。]\n\n{tail}",
        cfg.keep_head,
        cfg.keep_tail,
        mark = MARK_PREFIX
    );
    // 说明文字本身也要占 token。省不到 20% 就原样留着——
    // 压完只短了一点点，等于白丢信息还多付一段说明。
    let after = rebuilt.chars().count();
    if after * 10 > len * 8 {
        return;
    }
    *s = rebuilt;
    out.blocks += 1;
    out.chars_before += len;
    out.chars_after += after;
}

/// 内容感知过滤。
///
/// 只认四种「占了上下文却几乎不含决策信息」的形态。每条都遵守同一个约定：
/// **认错就返回 None**，让调用方退回头尾截断——宁可少省一点，也不能把
/// 模型真正要看的那几行删掉。
mod smart {
    /// 目录清单里保留的条目数上限（按深度筛过之后）。
    const LIST_KEEP: usize = 60;

    pub fn filter(s: &str) -> Option<(&'static str, String)> {
        let lines: Vec<&str> = s.lines().collect();
        if lines.len() < 20 {
            return None;
        }
        if let Some(t) = diff(&lines) {
            return Some(("git diff", t));
        }
        if let Some(t) = grep(&lines) {
            return Some(("grep 结果", t));
        }
        if let Some(t) = listing(&lines) {
            return Some(("目录清单", t));
        }
        dedup_log(&lines).map(|t| ("重复日志", t))
    }

    /// `git diff`：保留文件头、hunk 头与变更行，丢掉上下文行。
    ///
    /// 上下文行是 diff 里最占地方的部分，但「改了哪几行」通常在 `+`/`-` 里。
    fn diff(lines: &[&str]) -> Option<String> {
        if !lines.iter().take(40).any(|l| l.starts_with("diff --git ")) {
            return None;
        }
        let mut out = Vec::new();
        let mut changed = 0usize;
        for l in lines {
            let keep = l.starts_with("diff --git ")
                || l.starts_with("index ")
                || l.starts_with("--- ")
                || l.starts_with("+++ ")
                || l.starts_with("@@")
                || l.starts_with("new file")
                || l.starts_with("deleted file")
                || l.starts_with("rename ")
                || l.starts_with('+')
                || l.starts_with('-');
            if keep {
                if l.starts_with('+') || l.starts_with('-') {
                    changed += 1;
                }
                out.push(*l);
            }
        }
        // 变更行本来就占绝大多数时，过滤没有收益（还可能把 hunk 切碎）
        if changed * 10 > lines.len() * 7 {
            return None;
        }
        Some(out.join("\n"))
    }

    /// `grep` / `rg` 结果：每个文件最多留 3 行命中，其余折成计数。
    fn grep(lines: &[&str]) -> Option<String> {
        let looks_like_grep = lines
            .iter()
            .take(40)
            .filter(|l| {
                let Some((left, _)) = l.split_once(':') else {
                    return false;
                };
                left.contains('/') || left.contains('.')
            })
            .count();
        if looks_like_grep * 10 < lines.len().min(40) * 6 {
            return None;
        }

        let mut out: Vec<String> = Vec::new();
        let mut cur: Option<String> = None;
        let mut shown = 0usize;
        let mut hidden = 0usize;
        let flush = |out: &mut Vec<String>, cur: &Option<String>, hidden: &mut usize| {
            if *hidden > 0 {
                out.push(format!("  … 另有 {hidden} 处命中未列出"));
                *hidden = 0;
            }
            let _ = cur;
        };
        for l in lines {
            let file = l.split_once(':').map(|(f, _)| f).unwrap_or("");
            if Some(file.to_string()) != cur {
                flush(&mut out, &cur, &mut hidden);
                cur = Some(file.to_string());
                shown = 0;
            }
            if shown < 3 {
                out.push((*l).to_string());
                shown += 1;
            } else {
                hidden += 1;
            }
        }
        flush(&mut out, &cur, &mut hidden);
        Some(out.join("\n"))
    }

    /// 目录树 / `ls -R`：只留浅层条目，深层折成计数。
    fn listing(lines: &[&str]) -> Option<String> {
        let tree_markers = lines
            .iter()
            .filter(|l| {
                l.contains("├──") || l.contains("└──") || l.starts_with("|--") || l.contains("│  ")
            })
            .count();
        let ls_style = lines
            .iter()
            .filter(|l| l.starts_with('d') || l.starts_with('-') || l.starts_with("total "))
            .count();
        if tree_markers * 10 < lines.len() * 4 && ls_style * 10 < lines.len() * 5 {
            return None;
        }

        let mut out: Vec<String> = Vec::new();
        let mut skipped = 0usize;
        for l in lines {
            if out.len() < LIST_KEEP {
                out.push((*l).to_string());
            } else {
                skipped += 1;
            }
        }
        if skipped > 0 {
            out.push(format!("… 另有 {skipped} 行未列出（用更具体的目录重试）"));
        }
        Some(out.join("\n"))
    }

    /// 连续重复行折叠成 `xN`。构建日志、重试日志的典型形态。
    fn dedup_log(lines: &[&str]) -> Option<String> {
        let mut out: Vec<String> = Vec::new();
        let mut collapsed = 0usize;
        let mut i = 0usize;
        while i < lines.len() {
            let mut j = i + 1;
            while j < lines.len() && lines[j] == lines[i] {
                j += 1;
            }
            let n = j - i;
            if n >= 3 {
                out.push(format!("{}  [x{n}]", lines[i]));
                collapsed += n - 1;
            } else {
                for l in &lines[i..j] {
                    out.push((*l).to_string());
                }
            }
            i = j;
        }
        // 折叠不到 10% 就没意义
        if collapsed * 10 < lines.len() {
            return None;
        }
        Some(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn on() -> CompressConfig {
        CompressConfig {
            enabled: true,
            max_tool_chars: 100,
            keep_head: 40,
            keep_tail: 20,
            smart_filters: true,
        }
    }

    fn tool_msg(s: String) -> Value {
        json!([{ "role": "tool", "tool_call_id": "1", "content": s }])
    }

    #[test]
    fn long_tool_output_is_trimmed_with_a_visible_marker() {
        let body = "A".repeat(40) + &"B".repeat(200) + &"C".repeat(20);
        let mut p = json!({ "messages": tool_msg(body.clone()) });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 1);
        assert!(out.saved() > 0);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.starts_with(&"A".repeat(40)));
        assert!(got.ends_with(&"C".repeat(20)));
        assert!(got.contains(MARK_PREFIX));
        assert!(got.contains("不要根据这段摘要臆测"), "必须写清怎么拿回原文");
        assert!(got.chars().count() < body.chars().count() * 3 / 4);
        assert_eq!(out.chars_after, got.chars().count());
    }

    #[test]
    fn short_output_user_text_and_assistant_are_untouched() {
        let mut p = json!({
            "messages": [
                { "role": "system", "content": "x".repeat(500) },
                { "role": "user", "content": "y".repeat(500) },
                { "role": "assistant", "content": "z".repeat(500) },
                { "role": "tool", "content": "short" }
            ]
        });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 0, "非工具消息一律不动");
        assert_eq!(p["messages"][1]["content"], "y".repeat(500));
    }

    #[test]
    fn disabled_by_default_and_idempotent() {
        let body = "A".repeat(500);
        let mut p = json!({ "messages": tool_msg(body.clone()) });
        assert_eq!(apply(&mut p, &CompressConfig::default()).blocks, 0);
        assert_eq!(p["messages"][0]["content"], body);

        assert_eq!(apply(&mut p, &on()).blocks, 1);
        let once = p["messages"][0]["content"].as_str().unwrap().to_string();
        // 第二次不能再掐一刀——否则多跳重试会把尾巴也吃掉
        assert_eq!(apply(&mut p, &on()).blocks, 0);
        assert_eq!(p["messages"][0]["content"], once);
    }

    #[test]
    fn anthropic_tool_result_string_and_blocks_are_both_handled() {
        let long = "Q".repeat(400);
        let mut p = json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "看看这个" },
                    { "type": "tool_result", "tool_use_id": "t1", "content": long },
                    { "type": "tool_result", "tool_use_id": "t2",
                      "content": [{ "type": "text", "text": "R".repeat(400) }] }
                ]
            }]
        });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 2);
        assert_eq!(p["messages"][0]["content"][0]["text"], "看看这个");
        assert!(p["messages"][0]["content"][1]["content"]
            .as_str()
            .unwrap()
            .contains(MARK_PREFIX));
        assert!(p["messages"][0]["content"][2]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(MARK_PREFIX));
    }

    #[test]
    fn savings_below_the_threshold_are_skipped() {
        let body = "A".repeat(1000);
        let mut p = json!({ "messages": tool_msg(body) });
        // 留 75%：只省 25%，而说明文字又要占一截，不值得动
        let cfg = CompressConfig {
            enabled: true,
            max_tool_chars: 900,
            keep_head: 700,
            keep_tail: 50,
            smart_filters: false,
        };
        assert_eq!(apply(&mut p.clone(), &cfg).blocks, 0, "省 25% 不值得动它");

        // 留 60%：省 40%，压完连说明文字一起算仍然明显更短
        let cfg2 = CompressConfig {
            keep_head: 550,
            keep_tail: 50,
            ..cfg
        };
        assert_eq!(apply(&mut p, &cfg2).blocks, 1);
    }

    #[test]
    fn broken_limits_mean_no_compression() {
        let mut p = json!({ "messages": tool_msg("A".repeat(5000)) });
        let cfg = CompressConfig {
            enabled: true,
            max_tool_chars: 100,
            keep_head: 80,
            keep_tail: 80, // 留的比阈值还多：配置写反了
            smart_filters: false,
        };
        assert_eq!(apply(&mut p, &cfg).blocks, 0);
    }

    /// git diff：上下文行被丢掉，文件头与变更行保留。
    #[test]
    fn git_diff_drops_context_but_keeps_hunks() {
        let mut body = String::from("diff --git a/src/a.rs b/src/a.rs\nindex 111..222\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,20 +1,20 @@\n");
        for i in 0..40 {
            body.push_str(&format!(" context line {i} with some padding text here\n"));
        }
        body.push_str("+added one\n-removed one\n");
        let mut p = json!({ "messages": tool_msg(body.clone()) });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 1);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.contains("diff --git a/src/a.rs"));
        assert!(got.contains("+added one"));
        assert!(!got.contains("context line 7"), "上下文行应当被丢掉");
        assert!(got.contains("不要根据这段摘要臆测"));
    }

    /// grep：每个文件最多留 3 条命中，其余折成计数。
    #[test]
    fn grep_keeps_three_hits_per_file() {
        let mut body = String::new();
        for i in 0..20 {
            body.push_str(&format!("src/main.rs:{i}:fn thing_{i}() {{}}\n"));
        }
        let mut p = json!({ "messages": tool_msg(body) });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 1);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.contains("src/main.rs:0:"));
        assert!(got.contains("src/main.rs:2:"));
        assert!(!got.contains("src/main.rs:5:"), "第 4 条起应被折叠");
        assert!(got.contains("另有 17 处命中"));
    }

    /// 重复日志：连续 3 次以上折叠成 xN。
    #[test]
    fn repeated_log_lines_collapse() {
        let mut body = String::new();
        for _ in 0..30 {
            body.push_str("waiting for worker...\n");
        }
        let mut p = json!({ "messages": tool_msg(body) });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 1);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.contains("[x30]"), "应折叠成计数：{got}");
    }

    /// 认不出来的长文本仍走头尾截断，不能因为装了过滤器就改变行为。
    #[test]
    fn unknown_shape_falls_back_to_head_tail() {
        let body = "A".repeat(40) + &"B".repeat(200) + &"C".repeat(20);
        let mut p = json!({ "messages": tool_msg(body) });
        let out = apply(&mut p, &on());
        assert_eq!(out.blocks, 1);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.starts_with(&"A".repeat(40)));
        assert!(got.contains("省略中间"));
    }

    /// 关掉 smart_filters 时，git diff 也必须走通用截断（配置要能真的关掉）。
    #[test]
    fn smart_filters_can_be_turned_off() {
        let mut body = String::from("diff --git a/a b/a\n@@ -1 +1 @@\n");
        for i in 0..40 {
            body.push_str(&format!(" context {i}\n"));
        }
        let cfg = CompressConfig {
            smart_filters: false,
            ..on()
        };
        let mut p = json!({ "messages": tool_msg(body) });
        let out = apply(&mut p, &cfg);
        assert_eq!(out.blocks, 1);
        let got = p["messages"][0]["content"].as_str().unwrap();
        assert!(got.contains("省略中间"), "关掉后应退回通用截断");
    }
}
