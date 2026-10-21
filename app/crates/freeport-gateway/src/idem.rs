//! `Idempotency-Key` 响应缓存（§3.6.11）。
//!
//! agent 重试是常态：网络抖动一次，客户端会把同一份请求再发一遍。没有幂等缓存，
//! 上游会真切地再生成一遍——用户看到的是「同样的提示词被扣了两次额度」。
//!
//! 规则：
//! - 键 = `sha256(客户端 Idempotency-Key)`，**原地不存原文**（它可能被用户当成 secret）
//! - 指纹 = `model + messages + temperature + tools`。同键不同指纹 → 409，不静默复用
//! - 24h 内重放同一响应；流式不缓存（已经发出去的 token 没法重放），
//!   `finish_reason = length` 也不缓存（被截断的答案重放没有意义）
//!
//! 写在网关内存里，不落盘：进程重启后重放一次上游是可接受的代价，
//! 而把响应体（含用户 prompt 的产物）写到磁盘上不是。

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const TTL_MS: i64 = 24 * 3_600_000;
/// 内存上限。一份响应几 KB，512 条最多几 MB；超出按最早写入淘汰。
pub const MAX_ENTRIES: usize = 512;

struct Entry {
    fingerprint: String,
    body: String,
    provider: String,
    model: String,
    at: i64,
}

pub enum Lookup {
    Miss,
    /// 同键同指纹：直接重放
    Hit {
        body: String,
        provider: String,
        model: String,
    },
    /// 同键不同指纹：客户端拿错了 key，必须显式告诉它
    Conflict,
}

#[derive(Default)]
pub struct IdemStore {
    inner: Mutex<HashMap<String, Entry>>,
}

impl IdemStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, key: &str, fingerprint: &str, now_ms: i64) -> Lookup {
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, e| now_ms - e.at < TTL_MS);
        match map.get(key) {
            None => Lookup::Miss,
            Some(e) if e.fingerprint != fingerprint => Lookup::Conflict,
            Some(e) => Lookup::Hit {
                body: e.body.clone(),
                provider: e.provider.clone(),
                model: e.model.clone(),
            },
        }
    }

    pub fn put(
        &self,
        key: &str,
        fingerprint: &str,
        body: String,
        provider: String,
        model: String,
        now_ms: i64,
    ) {
        let mut map = self.inner.lock().unwrap();
        if map.len() >= MAX_ENTRIES && !map.contains_key(key) {
            // 淘汰最早写入的一条：这里不需要 LRU，重放窗口本来就是「刚刚」。
            if let Some(oldest) = map.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        map.insert(
            key.to_string(),
            Entry {
                fingerprint: fingerprint.to_string(),
                body,
                provider,
                model,
                at: now_ms,
            },
        );
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// 只取「同一次请求」的定义性字段。
///
/// 故意不含 `stream`（流式根本进不了缓存）、`max_tokens` 之外的采样参数：
/// 指纹太宽会让本来该重放的请求变成 409，太窄会把两次不同的请求认成同一次。
pub fn fingerprint(payload: &Value) -> String {
    let pick = |k: &str| payload.get(k).cloned().unwrap_or(Value::Null);
    let canon = serde_json::json!({
        "model": pick("model"),
        "messages": pick("messages"),
        "temperature": pick("temperature"),
        "tools": pick("tools"),
    });
    // serde_json 的 Map 默认按 key 排序，同一份语义结构序列化结果稳定
    sha256_hex(&canon.to_string())
}

/// 这条响应值不值得缓存。
pub fn cacheable(json: &Value) -> bool {
    let finish = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str());
    // 截断的答案重放没有意义；缺 finish_reason 的（部分厂商）按可缓存处理
    finish != Some("length")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_key_same_fingerprint_replays() {
        let s = IdemStore::new();
        let fp = fingerprint(&json!({"model":"m","messages":[{"role":"user","content":"hi"}]}));
        s.put("k1", &fp, "{\"ok\":1}".into(), "groq".into(), "m".into(), 1_000);
        match s.check("k1", &fp, 2_000) {
            Lookup::Hit { body, provider, .. } => {
                assert_eq!(body, "{\"ok\":1}");
                assert_eq!(provider, "groq");
            }
            _ => panic!("应当命中"),
        }
    }

    #[test]
    fn same_key_different_body_conflicts() {
        let s = IdemStore::new();
        let a = fingerprint(&json!({"model":"m","messages":[{"role":"user","content":"a"}]}));
        let b = fingerprint(&json!({"model":"m","messages":[{"role":"user","content":"b"}]}));
        s.put("k1", &a, "{}".into(), "p".into(), "m".into(), 1_000);
        assert!(matches!(s.check("k1", &b, 2_000), Lookup::Conflict));
    }

    #[test]
    fn fingerprint_ignores_stream_flag_but_not_temperature() {
        let a = fingerprint(&json!({"model":"m","stream":true,"temperature":0.2}));
        let b = fingerprint(&json!({"model":"m","stream":false,"temperature":0.2}));
        let c = fingerprint(&json!({"model":"m","temperature":0.9}));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn entries_expire_after_24h() {
        let s = IdemStore::new();
        let fp = fingerprint(&json!({"model":"m"}));
        s.put("k", &fp, "{}".into(), "p".into(), "m".into(), 0);
        assert!(matches!(s.check("k", &fp, TTL_MS - 1), Lookup::Hit { .. }));
        assert!(matches!(s.check("k", &fp, TTL_MS + 1), Lookup::Miss));
    }

    #[test]
    fn truncated_answers_are_not_cacheable() {
        assert!(!cacheable(&json!({"choices":[{"finish_reason":"length"}]})));
        assert!(cacheable(&json!({"choices":[{"finish_reason":"stop"}]})));
        assert!(cacheable(&json!({"choices":[{}]})));
    }
}
