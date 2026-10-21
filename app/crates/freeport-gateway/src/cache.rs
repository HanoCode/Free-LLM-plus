//! 精确匹配响应缓存。
//!
//! 与 `idem` 的区别是**触发条件**：幂等缓存只认客户端带来的 `Idempotency-Key`
//! （重试语义），这里认「同样的请求体」（省钱语义）。两者不共用一个表：
//! 幂等要保证「重放的是同一次调用」，这里只关心「答案还能不能复用」。
//!
//! 四条自我约束：
//! 1. **默认关**。缓存会让「重新问一次」拿不到新答案，必须用户自己开。
//! 2. **只缓存非流式**。流式响应没法在返回前拿到完整体，硬拼一个等于伪造。
//! 3. **只缓存正常收尾的回答**。`finish_reason` 不是 `stop` 的（被截断、被内容
//!    过滤）一律不缓存——把半截答案复用一整轮，比不缓存更糟。
//! 4. **键里不含密钥**。只对请求体做 sha256，避免把用户 key 写进缓存键。

use std::collections::HashMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct Hit {
    pub body: String,
    pub provider: String,
    pub model: String,
    pub at: i64,
}

#[derive(Debug, Default)]
pub struct ResponseCache {
    map: HashMap<String, Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    hit: Hit,
    expires: i64,
}

impl ResponseCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&mut self, key: &str, now: i64) -> Option<Hit> {
        let e = self.map.get(key)?;
        if now >= e.expires {
            self.map.remove(key);
            return None;
        }
        Some(e.hit.clone())
    }

    pub fn put(&mut self, key: String, hit: Hit, now: i64, ttl_secs: u64, max: usize) {
        if max == 0 || ttl_secs == 0 {
            return;
        }
        if self.map.len() >= max && !self.map.contains_key(&key) {
            // 简单粗暴：过期项先清，仍然满就整体清空。
            // LRU 要再维护一张链表，而这里的收益只是「省一次上游调用」，
            // 不值得为它引入一套淘汰策略的状态。
            let now_ms = now;
            self.map.retain(|_, e| e.expires > now_ms);
            if self.map.len() >= max {
                self.map.clear();
            }
        }
        self.map.insert(
            key,
            Entry {
                hit,
                expires: now + (ttl_secs as i64) * 1000,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) -> usize {
        let n = self.map.len();
        self.map.clear();
        n
    }
}

/// 缓存键：请求体的 sha256。
///
/// 只去掉 `stream` 字段：同一个问题在流式/非流式之间切换应当命中同一条。
/// 其余字段（含采样参数、system、tools）全部参与——采样参数不同答案就该不同。
pub fn key(payload: &Value) -> String {
    let mut v = payload.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("stream");
        obj.remove("stream_options");
    }
    let text = serde_json::to_string(&v).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    hex::encode(h.finalize())
}

/// 这个回答值不值得缓存。
///
/// 没有 `choices`、或者 `finish_reason` 不是 `stop` 的都不缓存：
/// `length` 是截断，`content_filter` 是被拦，`tool_calls` 要重放工具就会错。
pub fn cacheable(json: &Value) -> bool {
    let Some(choices) = json.get("choices").and_then(|c| c.as_array()) else {
        return false;
    };
    if choices.is_empty() {
        return false;
    }
    choices.iter().all(|c| {
        c.get("finish_reason").and_then(|r| r.as_str()) == Some("stop")
            && c.get("message")
                .map(|m| m.get("tool_calls").is_none())
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit() -> Hit {
        Hit {
            body: "{}".into(),
            provider: "groq".into(),
            model: "llama".into(),
            at: 0,
        }
    }

    #[test]
    fn key_ignores_stream_flag_but_not_sampling() {
        let a = json!({"model":"auto","messages":[{"role":"user","content":"hi"}],"stream":true});
        let b = json!({"model":"auto","messages":[{"role":"user","content":"hi"}]});
        let c = json!({"model":"auto","messages":[{"role":"user","content":"hi"}],"temperature":0.2});
        assert_eq!(key(&a), key(&b));
        assert_ne!(key(&a), key(&c));
    }

    #[test]
    fn entry_expires_after_ttl() {
        let mut c = ResponseCache::new();
        c.put("k".into(), hit(), 1_000, 60, 8);
        assert!(c.get("k", 1_000).is_some());
        assert!(c.get("k", 1_000 + 60_000).is_none(), "过点即失效");
    }

    #[test]
    fn capacity_is_bounded() {
        let mut c = ResponseCache::new();
        for i in 0..5 {
            c.put(format!("k{i}"), hit(), 0, 60, 3);
        }
        assert!(c.len() <= 3);
    }

    #[test]
    fn truncated_or_tool_answers_are_not_cacheable() {
        assert!(cacheable(&json!({"choices":[{"finish_reason":"stop","message":{}}]})));
        assert!(!cacheable(&json!({"choices":[{"finish_reason":"length","message":{}}]})));
        assert!(!cacheable(&json!({"choices":[]})));
        assert!(!cacheable(&json!({"error":{"message":"x"}})));
        assert!(!cacheable(&json!({"choices":[{
            "finish_reason":"stop",
            "message":{"tool_calls":[{"id":"1"}]}
        }]})));
    }
}
