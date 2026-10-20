//! 冷却与三态熔断。
//!
//! 作用域三级键：`p:provider` / `m:provider:model` / `k:account`。
//! **Key 级冷却按凭据算**（`k:acct_x`），因为限流、欠费、key 失效都是那条 key 自己的事；
//! 厂商级（5xx）与模型级（上下文超限、不支持工具）按 provider 算，换条 key 也一样会挂。
//!
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

    fn key(scope: Scope, provider_id: &str, account_id: &str, model_id: Option<&str>) -> String {
        match scope {
            Scope::Provider => format!("p:{provider_id}"),
            Scope::Model => format!("m:{provider_id}:{}", model_id.unwrap_or("*")),
            Scope::Key => format!("k:{account_id}"),
        }
    }

    /// 一条候选要检查的三个键：厂商、凭据、模型。
    fn keys(provider_id: &str, account_id: &str, model_id: Option<&str>) -> [String; 3] {
        [
            format!("p:{provider_id}"),
            format!("k:{account_id}"),
            model_id.map(|m| format!("m:{provider_id}:{m}")).unwrap_or_default(),
        ]
    }

    /// 记一次失败。同一目标反复失败时冷却翻倍，封顶 24h。
    /// 返回实际生效的冷却毫秒数。
    pub fn hit(
        &self,
        scope: Scope,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
        ms: u64,
        reason: &str,
    ) -> u64 {
        let key = Self::key(scope, provider_id, account_id, model_id);
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

    /// 一次成功：清掉这条凭据的冷却、它所属厂商的冷却，以及这个模型的冷却。
    pub fn note_success(&self, provider_id: &str, account_id: &str, model_id: Option<&str>) {
        let mut map = self.map.lock().unwrap();
        map.remove(&format!("p:{provider_id}"));
        map.remove(&format!("k:{account_id}"));
        if let Some(m) = model_id {
            map.remove(&format!("m:{provider_id}:{m}"));
        }
    }

    /// 删掉一条凭据时清掉它的 key 级冷却。
    ///
    /// 只删 `k:` 这一把：`p:` 属于整家、`m:` 属于这个模型，都还可能有别的
    /// 凭据在用。留着不删虽然不影响路由（候选里已经没有这条凭据了），
    /// 但状态栏会挂一条指向不存在的凭据的冷却，看起来像 bug。
    pub fn forget_account(&self, account_id: &str) {
        let mut map = self.map.lock().unwrap();
        map.remove(&format!("k:{account_id}"));
    }

    /// 三态快照（任一作用域取最严的那个）。必须同时传厂商和凭据：
    /// 厂商级故障会连坐同一家的所有 key，凭据级故障只挡它自己。
    pub fn state_for(
        &self,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
    ) -> BreakerState {
        let map = self.map.lock().unwrap();
        let mut worst: Option<(&Rest, String)> = None;
        for key in Self::keys(provider_id, account_id, model_id) {
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
    pub fn try_acquire_probe(&self, provider_id: &str, account_id: &str, model_id: Option<&str>) -> bool {
        let mut map = self.map.lock().unwrap();
        let keys: Vec<String> = Self::keys(provider_id, account_id, model_id)
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
    pub fn probe_failed(&self, provider_id: &str, account_id: &str, model_id: Option<&str>) {
        let mut map = self.map.lock().unwrap();
        let keys: Vec<String> = Self::keys(provider_id, account_id, model_id)
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
        let a = cd.hit(Scope::Key, "groq", "acct_a", None, 1_000, "rate_limit");
        let b = cd.hit(Scope::Key, "groq", "acct_a", None, 1_000, "rate_limit");
        assert_eq!(a, 1_000);
        assert_eq!(b, 2_000);
    }

    #[test]
    fn success_clears_model_and_key() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Model, "groq", "acct_a", Some("llama"), 1_000, "not_found");
        cd.note_success("groq", "acct_a", Some("llama"));
        assert_eq!(
            cd.state_for("groq", "acct_a", Some("llama")),
            BreakerState::Closed
        );
    }

    #[test]
    fn expired_cooldown_becomes_half_open() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Key, "groq", "acct_a", None, 0, "rate_limit");
        assert!(matches!(
            cd.state_for("groq", "acct_a", None),
            BreakerState::HalfOpen
        ));
        assert!(cd.try_acquire_probe("groq", "acct_a", None));
        assert!(!cd.try_acquire_probe("groq", "acct_a", None));
    }

    /// 限流是那条 key 的事：冷却 A 不能让 B 也停摆，否则多账号白接。
    #[test]
    fn key_scope_is_isolated_between_accounts() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Key, "groq", "acct_a", Some("llama"), 60_000, "rate_limit");
        assert!(matches!(
            cd.state_for("groq", "acct_a", Some("llama")),
            BreakerState::Open { .. }
        ));
        assert_eq!(
            cd.state_for("groq", "acct_b", Some("llama")),
            BreakerState::Closed,
            "同一个厂商的另一条 key 不该被连坐"
        );
    }

    /// 上游 5xx 是厂商的事：换条 key 也一样挂，必须全挡。
    #[test]
    fn provider_scope_blocks_every_account() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Provider, "groq", "acct_a", None, 60_000, "upstream");
        assert!(matches!(
            cd.state_for("groq", "acct_b", Some("llama")),
            BreakerState::Open { .. }
        ));
    }

    /// 删凭据之后不该还挂着一把指向不存在 key 的冷却。
    #[test]
    fn forgetting_an_account_leaves_other_scopes_alone() {
        let cd = Cooldowns::new();
        cd.hit(Scope::Key, "groq", "acct_a", None, 60_000, "rate_limit");
        cd.hit(Scope::Provider, "groq", "acct_a", None, 60_000, "upstream");
        cd.forget_account("acct_a");

        // key 级清掉了，但厂商级还在（那是整家的事，不能因为删了一把 key 就解除）
        assert!(cd
            .snapshot()
            .iter()
            .all(|c| c.key != "k:acct_a"));
        assert!(cd.snapshot().iter().any(|c| c.key == "p:groq"));
    }
}
