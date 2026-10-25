//! 中途接力（CONTINUE）：上游把流断在半截时的 prefill 续写。
//!
//! [`crate::hold`] 护住「首个语义块之前」——那之后 hedge 就收兵了，上游断流
//! 意味着客户端拿到半截回答。这里补上 FCC 分档里的 **CONTINUE**：已有正文、
//! 还没下发工具调用、客户端还连着，就把已收到的正文作为 assistant 消息
//! 接回上游（prefill），让模型从断点继续，客户端只看到一次连贯的回答。
//!
//! 边界（刻意收窄）：
//! - **工具调用一旦开始就 STOP**——tool_call 的参数是分帧拼出来的 JSON，
//!   半截参数续写等于让客户端执行一个没写完的工具；此时如实报错比续写安全。
//! - **纯 reasoning 断流不续**——思考内容进不了 prefill，没有正文可接。
//! - **一单只续一次**——续写本身也会断，再续就是在放大赌注。
//!
//! HANDOFF（换模型接管已声明的工具）需要重建工具调用语义，风险远超收益，
//! 不在这里做；断流时该失败还是失败，只是正文场景不再整轮作废。

use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 流式正文累积器：从（已归一成 OpenAI 形状的）SSE 块里攒正文。
///
/// chunk 会在任意字节处切开，所以按「行缓冲」处理：不完整的行留给下一片，
/// 完整的 `data:` 行才解析。解析失败静默跳过——SSE 里混注释行是常态，
/// 为它报错反而会把好流判坏。
pub struct ContentAcc {
    line: String,
    /// 已收到的正文（仅 `delta.content`；reasoning 不进来，prefill 用不了）
    pub content: String,
    /// 是否出现过工具调用。置位后不再续写（见模块注释）。
    pub tool_call: bool,
}

impl ContentAcc {
    pub fn new() -> Self {
        Self {
            line: String::new(),
            content: String::new(),
            tool_call: false,
        }
    }

    pub fn feed(&mut self, piece: &str) {
        self.line.push_str(piece);
        while let Some(i) = self.line.find('\n') {
            let line: String = self.line.drain(..=i).collect();
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let choices = v
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            for c in choices {
                if let Some(d) = c.get("delta") {
                    if let Some(s) = d.get("content").and_then(|s| s.as_str()) {
                        self.content.push_str(s);
                    }
                    let has_tools = d
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false)
                        || d.get("function_call").is_some();
                    if has_tools {
                        self.tool_call = true;
                    }
                }
                if c.get("message")
                    .and_then(|m| m.get("tool_calls"))
                    .is_some()
                {
                    self.tool_call = true;
                }
            }
        }
    }
}

/// 构造续写请求体：原样保留客户端发来的东西，把已收到的正文接成 assistant
/// prefill。模型从半截话继续，衔接处不需要任何标记。
pub fn prefill_body(payload: &Value, model: &str, partial: &str) -> Value {
    let mut body = payload.clone();
    body["model"] = json!(model);
    body["stream"] = json!(true);
    if body.get("stream_options").is_none() {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        msgs.push(json!({ "role": "assistant", "content": partial }));
    }
    body
}

/// 发起续写。成功返回上游流式响应（交给调用方归一转发）；任何一步不成
/// 都返回 `None`，调用方回落 STOP——续写失败不该再记一次健康惩罚，
/// 这一单在账本眼里本来就是成功的。
pub async fn resume(
    st: &std::sync::Arc<crate::server::State_>,
    provider_id: &str,
    account_id: &str,
    model_id: &str,
    payload: &Value,
    partial: &str,
    started: Instant,
    budget_ms: u64,
) -> Option<reqwest::Response> {
    // 45s 墙钟也管续写：剩 3s 以下就别再开了，开了也只会又被掐断
    let left_ms = budget_ms.saturating_sub(started.elapsed().as_millis() as u64);
    if left_ms < 3_000 {
        tracing::debug!(left_ms, "剩余预算不足以续写，放弃");
        return None;
    }
    // 续写是「补完」，不该吃满剩余预算：给 20s 封顶，留点给失败后的收尾
    let attempt_ms = Duration::from_millis(left_ms.min(20_000));

    let provider = {
        let d = st.data.read().await;
        d.providers
            .iter()
            .find(|p| p.id == provider_id)
            .cloned()?
    };
    // credential 内部可能发网络请求（OAuth 续期），必须在读锁外面调
    let cred = crate::server::credential(st, account_id).await?;
    let body = prefill_body(payload, model_id, partial);
    let client = crate::providers::client_of(&provider);

    match crate::providers::create_chat(&client, &provider, &cred, &body, attempt_ms).await {
        Ok(resp) => {
            let sse = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("event-stream"))
                .unwrap_or(false);
            if sse {
                tracing::info!(provider = %provider_id, account = %account_id, "断流续写已发起");
                Some(resp)
            } else {
                // 续写必须是流式：上游回了个 JSON 体说明它没按约定来，当失败
                None
            }
        }
        Err(e) => {
            tracing::debug!(provider = %provider_id, error = %e.body, "续写请求失败，回落 STOP");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acc_reassembles_chunks_split_mid_frame() {
        let mut acc = ContentAcc::new();
        // 一帧被切成三片
        acc.feed("data: {\"choices\":[{\"del");
        acc.feed("ta\":{\"content\":\"你\"}}]}\n\n");
        acc.feed("data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\ndata: [DONE]\n");
        assert_eq!(acc.content, "你好");
        assert!(!acc.tool_call);
    }

    #[test]
    fn acc_ignores_comments_and_partial_frames() {
        let mut acc = ContentAcc::new();
        // 注释行（keepalive）跳过；数据行被 TCP chunk 切在中间时不解析，
        // 攒到下一片凑齐整行才解析
        acc.feed(": keepalive\n\ndata: {\"choices\":[{\"del");
        acc.feed("ta\":{\"content\":\"x\"}}]}\n\n");
        assert_eq!(acc.content, "x");
    }

    #[test]
    fn acc_flags_tool_calls_and_stops_collecting() {
        let mut acc = ContentAcc::new();
        acc.feed("data: {\"choices\":[{\"delta\":{\"content\":\"前\"}}]}\n\n");
        acc.feed(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"ls\"}}]}}]}\n\n",
        );
        acc.feed("data: {\"choices\":[{\"delta\":{\"content\":\"后\"}}]}\n\n");
        assert!(acc.tool_call);
        // 工具之后的正文照样进 content（它已经发给客户端了），续写与否由调用方判
        assert_eq!(acc.content, "前后");
    }

    #[test]
    fn prefill_appends_assistant_and_keeps_the_rest() {
        let payload = json!({
            "model": "auto",
            "stream": true,
            "max_tokens": 512,
            "messages": [{"role": "user", "content": "讲个故事"}]
        });
        let body = prefill_body(&payload, "m-1", "从前有座山，");
        assert_eq!(body["model"], json!("m-1"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        assert_eq!(body["max_tokens"], json!(512));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["role"], json!("assistant"));
        assert_eq!(msgs[1]["content"], json!("从前有座山，"));
    }

    #[test]
    fn prefill_does_not_duplicate_stream_options() {
        let payload = json!({
            "stream": true,
            "stream_options": {"include_usage": false},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = prefill_body(&payload, "m", "x");
        assert_eq!(body["stream_options"]["include_usage"], json!(false));
    }
}
