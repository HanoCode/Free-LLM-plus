//! 跨协议的「思考签名」带内封装。
//!
//! 上游会把思考凭据放在 `tool_calls[].extra_content` 里（Gemini 的
//! `thought_signature` 就走这个字段），下一次请求必须原样带回，否则 400 ——
//! 而且这个 400 照样烧免费额度。
//!
//! Anthropic 协议里没有这个字段，只有一个 thinking 块的 `signature`。
//! 所以把整组 extra_content 封进 signature：客户端把它当不透明字符串回传，
//! 我们再解出来挂回 tool_calls。**无状态**，不存缓存，换机也不丢。
//!
//! 为什么不做「按目标厂商决定还原与否」：上游一律是 OpenAI 兼容协议，
//! `extra_content` 对非 Gemini 厂商只是一个被忽略的附加字段，带上不会坏事；
//! 反过来丢了就是 400。所以只在需要的时候带上，不做门禁。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

/// 带内签名的前缀。见到别的值一律当不可解析，不猜。
pub const PREFIX: &str = "fp1:";

#[derive(Debug, Clone, PartialEq)]
pub struct Sealed {
    /// 签名产生时在给谁服务（仅用于日志与排查）
    pub src: String,
    pub model: String,
    /// 每个 tool_call 的 extra_content，按 index 对齐；没有的填 null
    pub extras: Vec<Value>,
}

/// 封一个签名。`extras` 为空也封——至少能标出「这个 thinking 块是 Freeport 产的」。
pub fn seal(src: &str, model: &str, extras: &[Value]) -> String {
    let payload = json!({ "v": 1, "src": src, "m": model, "x": extras });
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(payload.to_string()))
}

/// 解签名。不是我们的格式、或内容坏了，都返回 `None`。
pub fn open(sig: &str) -> Option<Sealed> {
    let body = sig.strip_prefix(PREFIX)?;
    if body.is_empty() {
        return None;
    }
    let raw = URL_SAFE_NO_PAD.decode(body).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    if v.get("v").and_then(|x| x.as_u64()) != Some(1) {
        return None;
    }
    let src = v.get("src").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let model = v.get("m").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let extras = v
        .get("x")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Some(Sealed { src, model, extras })
}

/// 从 OpenAI 响应的 `message` 里取 extra_content 列表。
pub fn extras_of(msg: &Value) -> Vec<Value> {
    msg.get("tool_calls")
        .and_then(|x| x.as_array())
        .map(|cs| {
            cs.iter()
                .map(|c| c.get("extra_content").cloned().unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_extra_content() {
        let extras = vec![json!({ "google": { "thought_signature": "abc" } })];
        let sig = seal("gemini", "gemini-3-pro", &extras);
        assert!(sig.starts_with(PREFIX));
        let s = open(&sig).expect("should decode");
        assert_eq!(s.src, "gemini");
        assert_eq!(s.model, "gemini-3-pro");
        assert_eq!(s.extras, extras);
    }

    #[test]
    fn empty_extras_still_identify_the_source() {
        let s = open(&seal("groq", "llama-3", &[])).unwrap();
        assert_eq!(s.src, "groq");
        assert!(s.extras.is_empty());
    }

    #[test]
    fn foreign_or_broken_signature_is_not_guessed() {
        assert!(open("").is_none());
        assert!(open("EqQBCkgI...").is_none(), "Anthropic 原生签名不归我们管");
        assert!(open("fp1:").is_none());
        assert!(open("fp1:not-base64!!").is_none());
        // 版本不认识就放弃，不按 v1 猜
        let v2 = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(r#"{"v":2,"src":"x"}"#));
        assert!(open(&v2).is_none());
    }
}
