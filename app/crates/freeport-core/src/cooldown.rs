//! 冷却与三态熔断。
//!
//! 作用域有两级键：`p:provider` / `m:provider:model` / `k:provider`。
//! 到期不是直接恢复，而是进入半开：只允许一个试探请求通过，
//! 成功才真正闭合，失败则把冷却翻倍。这样避免「刚到期就再吃一次失败」。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Scope;

const MAX_COOLDOWN_MS: i64 = 24 * 3_600_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CooldownSnapshot {
    pub key: String,
    pub until: i64,
    pub reason: String,
    pub hits: u32,
    /// 到期后是否可以放一个试探请求
    #[serde(default)]
    pub half_open: bool,
    /// 是否已有试探在途
    #[serde(default)]
    pub probing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open { until: i64, reason: String },
    HalfOpen,
}

#[derive(Debug, Clone)]
struct Rest {
    until: i64,
    reason: String,
    hits: u32,
    probing: bool,
}

#[derive(Default)]
pub struct Cooldowns {
    map: Mutex<HashMap<String, Rest>>,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Cooldowns {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(scope: Scope, provider_id: &str, model_id: Option<&str>) -> String {
        match scope {
            Scope::Provider => format!("p:{provider_id}"),
            Scope::Model => format!("m:{provider_id}:{}", model_id.unwrap_or("*")),
            Scope::Key => format!("k:{provider_id}"),
        }
    }

    /// 记一次失败。同一目标反复失败时冷却翻倍，封顶 24h。
    /// 返回实际生效的冷却毫秒数。
    pub fn hit(&self, scope: Scope, provider_id: &str, model_id: Option<&str>, ms: u64, reason: &str) -> u64 {
        let key = Self::key(scope, provider_id, model_id);
        let mut map = self.map.lock().unwrap();
        let hits = map.get(&key).map(|r| r.hits + 1).unwrap_or(1);
        let factor = 2_i64.pow((hits - 1).min(4));
        let eff = ((ms as i64) * factor).min(MAX_COOLDOWN_MS).max(0);
        map.insert(
            key,
            Rest {
                until: now_ms() + eff,
                reason: reason.to_string(),
                hits,
                probing: false,
            },
        );
        eff as u64
    }

    /// 一次成功：清掉该 provider 的 key/provider 冷却，以及这个模型的冷却。
    pub fn note_success(&self, scope: Scope, provider_id: &str, model_id: Option<&str>) {
        let key = Self::key(scope, provider_id, model_id);
        let mut map = self.map.lock().unwrap();
        map.remove(&key);
        map.remove(&format!("k:{provider_id}"));
        map.remove(&format!("p:{provider_id}"));
        if let Some(m) = model_id {
            map.remove(&format!("m:{provider_id}:{m}"));
        }
    }

    /// 三态快照（任一作用域取最严的那个）。
    pub fn state(&self, provider_id: &str, model_id: Option<&str>) -> BreakerState {
        let map = self.map.lock().unwrap();
        let mut worst: Option<(&Rest, String)> = None;
        for key in [
            format!("p:{provider_id}"),
            format!("k:{provider_id}"),
            model_id.map(|m| format!("m:{provider_id}:{m}")).unwrap_or_default(),
        ] {
            if let Some(rest) = map.get(&key) {
                let is_worse = worst
                    .as_ref()
                    .map(|(w, _)| rest.until > w.until)
                    .unwrap_or(true);
                if is_worse {
                    worst = Some((rest, key));
                }
            }
        }
        match worst {
            None => BreakerState::Closed,
            Some((rest, _)) if now_ms() < rest.until => BreakerState::Open {
                until: rest.until,
                reason: rest.reason.clone(),
            },
            Some(_) => BreakerState::HalfOpen,
        }
    }

    /// 半开时抢占一次试探权；已经有人在探则返回 false。
    pub fn try_acquire_probe(&self, provider_id: &str, model_id: Option<&str>) -> bool {
        let mut map = self.map.lock().unwrap();
        let keys: Vec<String> = [
            format!("p:{provider_id}"),
            format!("k:{provider_id}"),
            model_id.map(|m| format!("m:{provider_id}:{m}")).unwrap_or_default(),
        ]
        .into_iter()
        .filter(|k| !k.is_empty())
        .collect();

        for k in keys {
            if let Some(rest) = map.get_mut(&k) {
                if now_ms() >= rest.until {
                    if rest.probing {
                        return false;
                    }
                    rest.probing = true;
                    return true;
                }
            }
        }
        false
    }

    /// 试探失败：把冷却翻倍，并释放试探权。
    pub fn probe_failed(&self, provider_id: &str, model_id: Option<&str>) {
        let mut map = self.map.lock().unwrap();
        let keys: Vec<String> = [
            format!("p:{provider_id}"),
            format!("k:{provider_id}"),
            model_id.map(|m| format!("m:{provider_id}:{m}")).unwrap_or_default(),
        ]
        .into_iter()
        .filter(|k| !k.is_empty())
        .collect();
        for k in keys {
            if let Some(rest) = map.get_mut(&k) {
                let span = (rest.until - (now_ms() - 60_000)).max(60_000);
                rest.until = now_ms() + (span * 2).min(MAX_COOLDOWN_MS);
                rest.hits += 1;
                rest.probing = false;
            }
        }
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    pub fn snapshot(&self) -> Vec<CooldownSnapshot> {
        let map = self.map.lock().unwrap();
        let now = now_ms();
        map.iter()
            .map(|(k, v)| CooldownSnapshot {
                key: k.clone(),
                until: v.until,
                reason: v.reason.clone(),
                hits: v.hits,
                half_open: now >= v.until,
                probing: v.probing,
            })
            .collect()
    }
}

/// `BreakerState::Open` 里带的是原因码，UI 会把它翻译成用户可读的一句话。
#[allow(dead_code)]
fn reason_label(reason: &str) -> &'static str {
    match reason {
        "invalid_key" => "密钥无效",
        "no_credit" => "额度耗尽",
        "quota" => "配额用尽",
        "rate_limit" => "限流",
        "not_found" => "模型不存在",
        "forbidden" => "无权限",
        "upstream" => "上游故障",
        "context" => "上下文超限",
        "no_tools" => "不支持工具调用",
        _ => "未知原因",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_on_repeat_failure() {
        let cd = Cooldowns::new();
        let a = cd.hit(Scope::Key, "groq", None, 1_000, "rate_limit");
        let b = cd.hit(Scope::Key, "groq", None, 1_000, "rate_limit");
        assert_eq!(a, 1_000);
        assert_eq!(b, 2_000);
    }

    #[test]
    fn success_clears_model_and_key() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Model, "groq", Some("llama"), 1_000, "not_found");
        cd.note_success(Scope::Model, "groq", Some("llama"));
        assert_eq!(cd.state("groq", Some("llama")), BreakerState::Closed);
    }

    #[test]
    fn expired_cooldown_becomes_half_open() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Key, "groq", None, 0, "rate_limit");
        assert!(matches!(cd.state("groq", None), BreakerState::HalfOpen));
        assert!(cd.try_acquire_probe("groq", None));
        assert!(!cd.try_acquire_probe("groq", None));
    }
}
