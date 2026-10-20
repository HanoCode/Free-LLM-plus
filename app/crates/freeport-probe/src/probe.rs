//! 探针：一次真实的上游调用，用最小代价回答「这条模型现在能不能用」。
//!
//! 两级：
//! - **liveness**：8 个 token，只验证「能回话」。高频、便宜，用于每日巡检。
//! - **capability**：带 tools / response_format 的请求，验证「会不会调工具」。
//!   贵 20 倍，只在入库、版本变更、手动触发时跑。

use std::time::{Duration, Instant};

use freeport_core::model::ModelCaps;
use serde_json::{json, Value};

pub const LIVENESS_TOKENS: u32 = 8;
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(20);
const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub status: u16,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub tokens: Option<u64>,
    /// 为了兼容厂商 quirk 重试了几次（正常 1 次）
    pub attempts: u8,
    /// 上游明说「这个模型没有免费档」。这不是临时故障，重试没有意义。
    pub no_free_tier: bool,
}

impl ProbeOutcome {
    fn fail(status: u16, latency_ms: u64, msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            status,
            latency_ms,
            error: Some(msg.into()),
            tokens: None,
            attempts: 1,
            no_free_tier: false,
        }
    }
}

fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// 发请求要带的认证头。探针不关心它是 `Bearer` 还是 `Cookie`——
/// 只认准「哪个头、什么值」，具体形态由 gateway 的凭据决定。
/// 这样探针不用为了新凭据形态再改一次签名。
pub type AuthHeader<'a> = (&'a str, &'a str);

fn trim(body: &str, max: usize) -> String {
    body.chars().take(max).collect::<String>().trim().to_string()
}

/// 一次最小的 chat 请求。
pub async fn liveness(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    auth: AuthHeader<'_>,
) -> ProbeOutcome {
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": LIVENESS_TOKENS,
        "temperature": 0,
        "stream": false,
    });

    let started = Instant::now();
    let mut attempt = 0u8;

    loop {
        attempt += 1;
        let res = client
            .post(join(base_url, "/chat/completions"))
            .header(auth.0, auth.1)
            .header("content-type", "application/json")
            .json(&body)
            .timeout(LIVENESS_TIMEOUT)
            .send()
            .await;

        let res = match res {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    "探针超时（20s）".to_string()
                } else {
                    e.to_string()
                };
                return ProbeOutcome {
                    attempts: attempt,
                    ..ProbeOutcome::fail(0, started.elapsed().as_millis() as u64, msg)
                };
            }
        };

        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();

        if (200..300).contains(&status) {
            let tokens = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("usage").and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64));
            return ProbeOutcome {
                ok: true,
                status,
                latency_ms: started.elapsed().as_millis() as u64,
                error: None,
                tokens,
                attempts: attempt,
                no_free_tier: false,
            };
        }

        // 厂商 quirk：新模型只认 max_completion_tokens，老网关不认 temperature
        if attempt == 1 && status == 400 {
            let lower = text.to_ascii_lowercase();
            if lower.contains("max_completion_tokens") {
                body["max_completion_tokens"] = body["max_tokens"].clone();
                body.as_object_mut().map(|o| o.remove("max_tokens"));
                continue;
            }
            if lower.contains("temperature") {
                body.as_object_mut().map(|o| o.remove("temperature"));
                continue;
            }
        }

        // 上游说「没有免费档」时，这一次失败就够判死了：
        // 后面 30 分钟一次复检，厂商真开了免费额度也能自己回来。
        let mut out = ProbeOutcome::fail(
            status,
            started.elapsed().as_millis() as u64,
            trim(&text, 240),
        );
        out.attempts = attempt;
        out.no_free_tier = freeport_core::error::parse_quota_signal(&text)
            .map(|s| s.no_free_tier)
            .unwrap_or(false);
        return out;
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapsOutcome {
    pub caps: ModelCaps,
    pub status: u16,
    pub notes: Vec<String>,
}

/// 能力探针：只测「请求被接受」而不是「模型一定照做」。
/// 模型不肯调用工具是行为，不是能力缺失；厂商直接 400 才是能力缺失。
pub async fn capability(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    auth: AuthHeader<'_>,
) -> CapsOutcome {
    let mut out = CapsOutcome::default();
    let url = join(base_url, "/chat/completions");

    // ① 工具调用
    let tools_body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "What time is it? Use the get_time tool." }],
        "max_tokens": 64,
        "temperature": 0,
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_time",
                "description": "Return the current time",
                "parameters": {
                    "type": "object",
                    "properties": { "timezone": { "type": "string" } },
                    "required": []
                }
            }
        }],
        "tool_choice": "auto"
    });

    match call(client, &url, auth, &tools_body, CAPABILITY_TIMEOUT).await {
        Ok((status, text)) if (200..300).contains(&status) => {
            out.caps.tools = Some(true);
            out.status = status;
            if !text.contains("\"tool_calls\"") && !text.contains("\"tool_use\"") {
                out.notes.push("接受 tools 参数，但本次未发起调用".into());
            }
        }
        Ok((status, text)) => {
            out.status = status;
            let lower = text.to_ascii_lowercase();
            if matches!(status, 400 | 404 | 422)
                && (lower.contains("tool") || lower.contains("function"))
            {
                out.caps.tools = Some(false);
                out.notes.push("上游拒绝 tools 参数".into());
            } else {
                out.notes.push(trim(&text, 160));
            }
        }
        Err(e) => out.notes.push(e),
    }

    // ② JSON 模式
    let json_body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "Reply with exactly {\"ok\":true} and nothing else." }],
        "max_tokens": 32,
        "temperature": 0,
        "response_format": { "type": "json_object" }
    });

    match call(client, &url, auth, &json_body, CAPABILITY_TIMEOUT).await {
        Ok((status, text)) if (200..300).contains(&status) => {
            let content = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let parses = serde_json::from_str::<Value>(content.trim()).is_ok();
            out.caps.json_mode = Some(parses);
            if !parses {
                out.notes.push("接受 response_format，但返回内容不是合法 JSON".into());
            }
        }
        Ok((status, text)) => {
            if matches!(status, 400 | 404 | 422) {
                out.caps.json_mode = Some(false);
                out.notes.push(format!("不支持 response_format（{status}）"));
            } else {
                out.notes.push(trim(&text, 160));
            }
        }
        Err(e) => out.notes.push(e),
    }

    out
}

async fn call(
    client: &reqwest::Client,
    url: &str,
    auth: AuthHeader<'_>,
    body: &Value,
    timeout: Duration,
) -> Result<(u16, String), String> {
    let res = client
        .post(url)
        .header(auth.0, auth.1)
        .header("content-type", "application/json")
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "探针超时".to_string()
            } else {
                e.to_string()
            }
        })?;
    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_body_is_cheap() {
        // 8 token 是硬约束：巡检要跑几百次，不能把用户额度吃光
        assert!(LIVENESS_TOKENS <= 10);
    }
}
