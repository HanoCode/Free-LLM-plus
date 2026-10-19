//! 错误 → 作用域 → 动作。
//!
//! 这张表是整个网关的根：判错作用域比退避策略重要一个数量级。
//! 一个平台 5xx 如果被当成「模型失败」，会在它的每个模型上各烧一跳。
//!
//! v1.2 起，冷却到期不再直接恢复，而是进入半开试探（见 `Cooldowns::probe_ready`）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// 单个 key / 账号
    Key,
    /// 某个 provider 下的某个模型
    Model,
    /// 整个 provider（本项目一个 provider 目前只有一条凭据）
    Provider,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Model => "model",
            Self::Provider => "provider",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorClass {
    pub scope: Scope,
    pub cooldown_ms: u64,
    pub kind: String,
    /// 能力缺口，例如 "tools"；由 M2 的探针与门禁消费
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

impl ErrorClass {
    fn new(scope: Scope, cooldown_ms: u64, kind: &str) -> Self {
        Self {
            scope,
            cooldown_ms,
            kind: kind.to_string(),
            capability: None,
        }
    }

    fn with_capability(mut self, cap: &str) -> Self {
        self.capability = Some(cap.to_string());
        self
    }
}

const MIN: u64 = 60_000;
const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

/// 把上游的 HTTP 状态与响应体映射成作用域与冷却时长。
pub fn classify(status: u16, body: &str, retry_after_ms: u64) -> ErrorClass {
    let t = body.to_ascii_lowercase();

    match status {
        401 => ErrorClass::new(Scope::Key, 5 * MIN, "invalid_key"),
        402 => ErrorClass::new(Scope::Key, DAY, "no_credit"),
        403 => {
            if t.contains("quota")
                || t.contains("billing")
                || t.contains("exceeded")
                || t.contains("limit")
            {
                ErrorClass::new(Scope::Key, DAY, "quota")
            } else {
                ErrorClass::new(Scope::Model, DAY, "forbidden")
            }
        }
        429 => ErrorClass::new(Scope::Key, retry_after_ms.max(90_000), "rate_limit"),
        404 | 410 => ErrorClass::new(Scope::Model, DAY, "not_found"),
        413 => ErrorClass::new(Scope::Model, 0, "context"),
        400 => {
            if t.contains("context")
                || t.contains("too long")
                || t.contains("maximum")
                || t.contains("reduce")
            {
                ErrorClass::new(Scope::Model, 0, "context")
            } else if t.contains("tool") || t.contains("function") {
                ErrorClass::new(Scope::Model, 0, "no_tools").with_capability("tools")
            } else {
                ErrorClass::new(Scope::Model, 0, "bad_request")
            }
        }
        500..=599 => ErrorClass::new(Scope::Provider, 90_000, "upstream"),
        _ => ErrorClass::new(Scope::Model, 0, "unknown"),
    }
}

/// 由上游 `Retry-After` 头或 429 文案推出等待时间。
pub fn parse_retry_after(headers: Option<&str>, body: &str) -> u64 {
    if let Some(raw) = headers {
        let raw = raw.trim();
        if let Ok(sec) = raw.parse::<f64>() {
            return (sec.max(0.0) * 1000.0).round() as u64;
        }
    }
    // Gemini 的 retryDelay: "7s"；部分厂商写在 body 里
    let lower = body.to_ascii_lowercase();
    for needle in ["retry_after", "retrydelay", "retry in", "try again in"] {
        if let Some(pos) = lower.find(needle) {
            // 只看 needle 之后的一小段，避免抓到无关数字（如状态码 429）
            let tail: String = lower[pos + needle.len()..].chars().take(48).collect();
            if let Some(num) = first_number(&tail) {
                // 带 ms 的写法按毫秒算
                if tail.contains("ms") && num > 100.0 {
                    return num.round() as u64;
                }
                return (num * 1000.0).round() as u64;
            }
        }
    }
    0
}

fn first_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Ok(v) = s[start..i].parse::<f64>() {
                return Some(v);
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_cools_only_the_key() {
        let c = classify(401, "", 0);
        assert_eq!(c.scope, Scope::Key);
        assert_eq!(c.cooldown_ms, 300_000);
    }

    #[test]
    fn server_error_cools_the_whole_provider() {
        let c = classify(503, "upstream down", 0);
        assert_eq!(c.scope, Scope::Provider);
    }

    #[test]
    fn tool_rejection_is_scoped_to_the_model() {
        let c = classify(400, "this model does not support tools", 0);
        assert_eq!(c.scope, Scope::Model);
        assert_eq!(c.kind, "no_tools");
        assert_eq!(c.capability.as_deref(), Some("tools"));
    }

    #[test]
    fn rate_limit_honours_retry_after() {
        let c = classify(429, "", 12_000);
        assert_eq!(c.cooldown_ms, 90_000); // 低于下限时用 90s
        let c = classify(429, "", 300_000);
        assert_eq!(c.cooldown_ms, 300_000);
    }

    #[test]
    fn parses_gemini_retry_delay() {
        assert_eq!(parse_retry_after(None, r#"{"retryDelay":"7s"}"#), 7000);
    }
}
