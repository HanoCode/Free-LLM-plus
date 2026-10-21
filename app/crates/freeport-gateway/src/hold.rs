//! Hold：把「成功」的判定点从响应头后移到首个有效语义块。
//!
//! 免费厂商最典型的失败形态是 `200 OK` + 一段正常 SSE + 中途一条 error，
//! 或者干脆 200 + 空 content。若在拿到响应头时就记账成功，冷却永不触发，
//! 路由会持续把流量送进同一个黑洞。
//!
//! 这里的规则：在拿到 text / reasoning / tool_call 任一内容块之前，
//! 这次尝试都是可撤销的；超过空闲上限没等到，就换下一家。

use bytes::Bytes;
use futures_util::stream::{BoxStream, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

/// 单次 Hold 的最大缓冲。能输出这么多说明上游不是在报错。
const MAX_HEAD_BYTES: usize = 64 * 1024;

pub struct HoldCfg {
    /// 多久没收到任何字节就判失败
    pub idle_ms: u64,
    /// 总预算，防止上游持续发空增量把 Hold 拖满
    pub total_ms: u64,
}

pub enum HoldOutcome {
    /// 已提交：head 是已经读进来的字节，rest 是剩余流
    Committed {
        head: Vec<Bytes>,
        rest: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    },
    /// 未提交：可以安全地换下一跳
    Rejected { kind: &'static str, detail: String },
}

enum Scan {
    Wait,
    Commit,
    Error(&'static str, String),
}

pub async fn wait_commit(
    resp: reqwest::Response,
    cfg: &HoldCfg,
    wire: crate::upwire::Wire,
) -> HoldOutcome {
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("event-stream"))
        .unwrap_or(false);

    let mut stream = resp.bytes_stream().boxed();
    let mut head: Vec<Bytes> = Vec::new();
    let mut buf = String::new();
    let start = tokio::time::Instant::now();
    let mut seen = 0usize;

    loop {
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= cfg.total_ms {
            return HoldOutcome::Rejected {
                kind: "hold_total",
                detail: format!("{}ms 内没有可用内容", cfg.total_ms),
            };
        }
        let slice = cfg.idle_ms.min(cfg.total_ms - elapsed);

        match timeout(Duration::from_millis(slice), stream.next()).await {
            Err(_) => {
                return HoldOutcome::Rejected {
                    kind: "hold_idle",
                    detail: format!("{}ms 内没有任何字节", slice),
                }
            }
            Ok(None) => {
                // 流正常结束但从未提交
                return match scan(&buf, is_sse, wire) {
                    Scan::Error(kind, detail) => HoldOutcome::Rejected { kind, detail },
                    _ => HoldOutcome::Rejected {
                        kind: "empty_stream",
                        detail: format!("上游结束但没有内容：{}", head.len()),
                    },
                };
            }
            Ok(Some(Err(e))) => {
                return HoldOutcome::Rejected {
                    kind: "stream_broken",
                    detail: e.to_string(),
                }
            }
            Ok(Some(Ok(bytes))) => {
                seen += bytes.len();
                head.push(bytes.clone());
                buf.push_str(&String::from_utf8_lossy(&bytes));

                match scan(&buf, is_sse, wire) {
                    Scan::Commit => {
                        return HoldOutcome::Committed { head, rest: stream }
                    }
                    Scan::Error(kind, detail) => {
                        return HoldOutcome::Rejected { kind, detail }
                    }
                    Scan::Wait => {}
                }

                if seen > MAX_HEAD_BYTES {
                    // 已经在大量输出，不可能是错误响应
                    return HoldOutcome::Committed { head, rest: stream };
                }
            }
        }
    }
}

fn scan(buf: &str, is_sse: bool, wire: crate::upwire::Wire) -> Scan {
    // 上游讲原生协议时不能按 OpenAI 的字段去认内容，否则永远认不出来，
    // 每一单都要等到 hold 超时才提交——那是纯粹的延迟税。
    if !wire.is_openai() {
        return scan_native(buf, wire);
    }
    if !is_sse {
        let t = buf.trim();
        if !t.starts_with('{') {
            // 非 JSON、非 SSE：等读满再判，避免把纯文本错误当成内容
            return if buf.len() > 2048 {
                Scan::Commit
            } else {
                Scan::Wait
            };
        }
        let Ok(v) = serde_json::from_str::<Value>(t) else {
            return Scan::Wait;
        };
        if v.get("error").is_some() {
            return Scan::Error("upstream_body", first_message(&v));
        }
        return if has_content(&v) {
            Scan::Commit
        } else {
            Scan::Error("empty_completion", "响应里没有内容".into())
        };
    }

    for line in buf.split('\n') {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue; // 行还没收全，下一轮再看
        };
        if v.get("error").is_some() {
            return Scan::Error("upstream_body", first_message(&v));
        }
        if has_content(&v) {
            return Scan::Commit;
        }
    }
    Scan::Wait
}

/// 原生协议只认两件事：出错了、出内容了。
///
/// 刻意不在这里做完整解析：那是 [`crate::upwire`] 的活，而且多解析一遍
/// 就等于把「什么时候算提交」这个判断分散到两处，迟早对不上。
fn scan_native(buf: &str, wire: crate::upwire::Wire) -> Scan {
    for line in buf.split('\n') {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty.contains("error") || ty.ends_with(".failed") {
            return Scan::Error("upstream_body", first_message(&v));
        }
        let hit = match wire {
            crate::upwire::Wire::Anthropic => match ty {
                // message_start 只是开场白，不含内容，不能当提交
                "content_block_delta" => v
                    .pointer("/delta/text")
                    .or_else(|| v.pointer("/delta/thinking"))
                    .or_else(|| v.pointer("/delta/partial_json"))
                    .and_then(|x| x.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
                "content_block_start" => {
                    v.pointer("/content_block/type").and_then(|x| x.as_str())
                        == Some("tool_use")
                }
                _ => false,
            },
            crate::upwire::Wire::Responses => match ty {
                "response.output_text.delta"
                | "response.reasoning_summary_text.delta"
                | "response.function_call_arguments.delta" => v
                    .get("delta")
                    .and_then(|x| x.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false),
                "response.output_item.added" => {
                    v.pointer("/item/type").and_then(|x| x.as_str()) == Some("function_call")
                }
                _ => false,
            },
            crate::upwire::Wire::Openai => false,
        };
        if hit {
            return Scan::Commit;
        }
    }
    Scan::Wait
}

fn first_message(v: &Value) -> String {
    v.pointer("/error/message")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
        .unwrap_or("上游返回错误")
        .chars()
        .take(240)
        .collect()
}

/// 是否出现了「已提交」的内容：正文、思考、工具调用任一即可。
fn has_content(v: &Value) -> bool {
    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
        return false;
    };
    for c in choices {
        if let Some(d) = c.get("delta") {
            if non_empty(d.get("content"))
                || non_empty(d.get("reasoning_content"))
                || non_empty(d.get("reasoning"))
            {
                return true;
            }
            if d.get("tool_calls")
                .and_then(|t| t.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
            {
                return true;
            }
            if d.get("function_call").is_some() {
                return true;
            }
        }
        if let Some(m) = c.get("message") {
            if non_empty(m.get("content")) || m.get("tool_calls").is_some() {
                return true;
            }
        }
    }
    false
}

fn non_empty(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_only_delta_is_not_a_commit() {
        let s = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Wait));
    }

    #[test]
    fn text_delta_commits() {
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}";
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Commit));
    }

    #[test]
    fn reasoning_delta_commits() {
        let s = r#"data: {"choices":[{"delta":{"reasoning_content":"think"}}]}"#;
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Commit));
    }

    #[test]
    fn tool_call_delta_commits() {
        let s = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1"}]}}]}"#;
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Commit));
    }

    #[test]
    fn error_event_is_rejected() {
        let s = r#"data: {"error":{"message":"free tier exhausted"}}"#;
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Error("upstream_body", _)));
    }

    #[test]
    fn partial_line_waits_for_more() {
        let s = r#"data: {"choices":[{"delta":{"cont"#;
        assert!(matches!(scan(s, true, crate::upwire::Wire::Openai), Scan::Wait));
    }
}
