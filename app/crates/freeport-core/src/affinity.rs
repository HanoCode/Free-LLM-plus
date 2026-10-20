//! 会话粘性：一段对话尽量回到上次答它的那家。
//!
//! 免费额度场景里，粘性的价值不是「负载均衡得更好看」，而是**保住 prompt cache**：
//! 同一段对话换一家，上游要重新读一遍完整 prompt；不换，第二次起读的是缓存，
//! 首字节更快、扣的额度更少。
//!
//! 三条刻意的取舍：
//! 1. **按会话算，不按 key 算**。一个 agent 可能同时跑多个会话，按 key 粘会让它们互相踩。
//! 2. **默认 30 分钟滑动过期**。上游的缓存普遍 5 分钟不读就凉，30 分钟是给「人还在想」留的余量；
//!    超过这个时间，粘性带来的好处已经小于「换一家可能更快」。
//! 3. **余量见底主动解粘**（`UNSTICK_RATIO`）。粘到最后一点额度上，比换家的代价大得多。
//!
//! 不做的事：不落盘。重启后第一轮重新学习，代价只有一次未命中。

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

/// 默认粘性窗口：30 分钟滑动。
pub const DEFAULT_TTL_MS: i64 = 30 * 60 * 1000;
/// 余量低于这个比例就主动解粘，把机会让给别家。
pub const UNSTICK_RATIO: f64 = 0.10;
/// 最多记多少个会话，超出按最旧淘汰。够覆盖一台机器上的并行 agent。
const MAX_SESSIONS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stick {
    pub provider: String,
    /// 上次答它的那条**凭据**。同一个厂商的两个 key 缓存互不相通，
    /// 所以粘性要精确到 key，而不是「回到这家随便一条」。
    pub account: String,
    pub model: String,
    /// 最后一次命中的时刻（ms），每次命中都会往后推
    pub at: i64,
}

#[derive(Debug, Clone)]
pub struct Affinity {
    ttl_ms: i64,
    map: BTreeMap<String, Stick>,
}

impl Default for Affinity {
    fn default() -> Self {
        Self::new(DEFAULT_TTL_MS)
    }
}

impl Affinity {
    pub fn new(ttl_ms: i64) -> Self {
        Self {
            ttl_ms,
            map: BTreeMap::new(),
        }
    }

    pub fn ttl_ms(&self) -> i64 {
        self.ttl_ms
    }

    /// 从请求体推一个会话指纹。
    ///
    /// agent 不会主动带 session id，所以取「第一条 user 消息 + system」做指纹：
    /// 同一段对话的每一轮都会重发这段开头，指纹自然稳定；
    /// 用户改口重开一段对话，指纹就变了，也就不会再粘到旧的那家。
    pub fn fingerprint(model: &str, body: &serde_json::Value) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        model.hash(&mut h);
        if let Some(s) = body.get("system").and_then(|v| v.as_str()) {
            s.hash(&mut h);
        }
        if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
            if let Some(first) = msgs.iter().find(|m| {
                m.get("role").and_then(|r| r.as_str()) == Some("user")
            }) {
                content_text(first.get("content")).hash(&mut h);
            } else if let Some(first) = msgs.first() {
                content_text(first.get("content")).hash(&mut h);
            }
        }
        format!("{:016x}", h.finish())
    }

    /// 记下「这段对话是谁答的」。
    pub fn note(&mut self, session: &str, provider: &str, account: &str, model: &str, now: i64) {
        if session.is_empty() {
            return;
        }
        self.map.insert(
            session.to_string(),
            Stick {
                provider: provider.to_string(),
                account: account.to_string(),
                model: model.to_string(),
                at: now,
            },
        );
        if self.map.len() > MAX_SESSIONS {
            self.prune(now);
            // 还是超（大量新会话同时活跃）：按最旧淘汰，一次只淘汰一个
            if self.map.len() > MAX_SESSIONS {
                if let Some(oldest) = self
                    .map
                    .iter()
                    .min_by_key(|(_, s)| s.at)
                    .map(|(k, _)| k.clone())
                {
                    self.map.remove(&oldest);
                }
            }
        }
    }

    /// 读一次粘性。命中会**滑动**过期时间（人还在这个会话里说话，就该继续粘）。
    pub fn touch(&mut self, session: &str, now: i64) -> Option<Stick> {
        let s = self.map.get(session)?.clone();
        if now - s.at > self.ttl_ms {
            self.map.remove(session);
            return None;
        }
        if let Some(cur) = self.map.get_mut(session) {
            cur.at = now;
        }
        Some(s)
    }

    /// 只看不改，用于「要不要解粘」的判断。
    pub fn peek(&self, session: &str, now: i64) -> Option<&Stick> {
        let s = self.map.get(session)?;
        if now - s.at > self.ttl_ms {
            return None;
        }
        Some(s)
    }

    pub fn forget(&mut self, session: &str) {
        self.map.remove(session);
    }

    /// 清掉过期项。不建定时任务，写路径上顺手做。
    pub fn prune(&mut self, now: i64) {
        let ttl = self.ttl_ms;
        self.map.retain(|_, s| now - s.at <= ttl);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 把 messages 里的 content 摊平成文本：字符串直接取，数组只取 text 段。
fn content_text(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|part| {
                let ty = part.get("type").and_then(|t| t.as_str());
                if ty == Some("text") || ty.is_none() {
                    part.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(first: &str) -> serde_json::Value {
        json!({ "messages": [{ "role": "user", "content": first }] })
    }

    #[test]
    fn same_conversation_same_fingerprint() {
        let a = Affinity::fingerprint("auto", &body("帮我看看这个报错"));
        let b = Affinity::fingerprint(
            "auto",
            &json!({
                "messages": [
                    { "role": "user", "content": "帮我看看这个报错" },
                    { "role": "assistant", "content": "什么报错？" },
                    { "role": "user", "content": "E0382" }
                ]
            }),
        );
        assert_eq!(a, b, "多轮之后仍认得出是同一段对话");
    }

    #[test]
    fn different_conversation_different_fingerprint() {
        let a = Affinity::fingerprint("auto", &body("第一段对话"));
        let b = Affinity::fingerprint("auto", &body("另一段完全不同的话"));
        assert_ne!(a, b);
    }

    #[test]
    fn stick_slides_until_silence_too_long() {
        let mut a = Affinity::default();
        a.note("s", "groq", "acct_a", "llama-3", 1_000);
        assert_eq!(a.peek("s", 1_000).unwrap().provider, "groq");
        // 25 分钟后还在：命中会把时间往后推
        assert!(a.touch("s", 1_000 + 25 * 60_000).is_some());
        // 从最后一次命中再算 30 分钟才过期
        assert!(a.peek("s", 1_000 + 50 * 60_000).is_some());
        assert!(a.peek("s", 1_000 + 25 * 60_000 + DEFAULT_TTL_MS + 1).is_none());
    }

    #[test]
    fn expired_stick_is_dropped_on_read() {
        let mut a = Affinity::default();
        a.note("s", "groq", "acct_a", "llama-3", 0);
        assert!(a.peek("s", DEFAULT_TTL_MS + 1).is_none());
        assert!(a.touch("s", DEFAULT_TTL_MS + 1).is_none());
        assert_eq!(a.len(), 0, "读到过期就该清掉，不能无限攒");
    }

    #[test]
    fn note_overwrites_previous_answerer() {
        let mut a = Affinity::default();
        a.note("s", "groq", "acct_a", "llama-3", 0);
        a.note("s", "cerebras", "acct_b", "qwen-3", 10);
        let s = a.peek("s", 10).unwrap();
        assert_eq!((s.provider.as_str(), s.model.as_str()), ("cerebras", "qwen-3"));
        assert_eq!(a.len(), 1, "同一会话只留一条");
    }
}
