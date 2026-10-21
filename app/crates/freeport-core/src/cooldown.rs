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
/// 探针提前解除的水位：已经歇了一半以上，且还剩至少 60 秒。
///
/// 刚进去就解除等于没有冷却（上游还没缓过来，探针只会连着失败两次）；
/// 快到期了再解除没有收益（本来就快好了）。
const RECOVER_MIN_SERVED: f64 = 0.5;
const RECOVER_MIN_LEFT_MS: i64 = 60_000;
/// 已解除的记录保留多久，用来记住「这条目标翻过几次车」。
/// 不保留的话，一个不稳定的目标每次都从 2 分钟重新爬阶梯。
const HISTORY_KEEP_MS: i64 = 24 * 3_600_000;

/// 冷却的来源。
///
/// **只有 `Heuristic` 允许被探针提前解除**——我们猜的可以推翻，上游明说的不行：
/// - `Authoritative`：上游给了 `Retry-After` / 日额度重置时刻，是事实；
/// - `Credit`：402 欠费。key 有效不代表充值了，探针验不出任何东西；
/// - `Tier`：403 套餐不含该模型。key 有效但模型照样进不来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Heuristic,
    Authoritative,
    Credit,
    Tier,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heuristic => "heuristic",
            Self::Authoritative => "authoritative",
            Self::Credit => "credit",
            Self::Tier => "tier",
        }
    }

    /// 探针能不能推翻它
    pub fn probeable(self) -> bool {
        matches!(self, Self::Heuristic)
    }
}

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
    /// 来源：heuristic / authoritative / credit / tier
    #[serde(default)]
    pub source: String,
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
    source: Source,
    /// 这次冷却原本有多长，判「歇够一半没有」用
    bench_ms: i64,
    set_at: i64,
    /// 已被探针提前解除。记录留着，只为了记住翻车次数；
    /// 路由、UI、半开都必须当它不存在。
    recovered: bool,
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
    ///
    /// 默认来源是 `Heuristic`：调用方不知道上游到底说了什么，就按猜的算。
    /// 上游明确给了 `Retry-After` 的路径必须走 [`hit_with`]，否则探针会把
    /// 一个「厂商说 10 分钟」的事实当成猜测推翻掉。
    pub fn hit(
        &self,
        scope: Scope,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
        ms: u64,
        reason: &str,
    ) -> u64 {
        self.hit_with(Source::Heuristic, scope, provider_id, account_id, model_id, ms, reason)
    }

    pub fn hit_with(
        &self,
        source: Source,
        scope: Scope,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
        ms: u64,
        reason: &str,
    ) -> u64 {
        self.hit_at(source, scope, provider_id, account_id, model_id, ms, reason, now_ms())
    }

    /// 带注入时钟的版本：单测要能构造「已经歇了 400 秒」这种状态。
    #[allow(clippy::too_many_arguments)]
    fn hit_at(
        &self,
        source: Source,
        scope: Scope,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
        ms: u64,
        reason: &str,
        now: i64,
    ) -> u64 {
        let key = Self::key(scope, provider_id, account_id, model_id);
        let mut map = self.map.lock().unwrap();
        // 已解除的记录仍然算数：一个反复翻车的目标不该每次都从 2 分钟重新爬
        let hits = map.get(&key).map(|r| r.hits + 1).unwrap_or(1);
        let factor = 2_i64.pow((hits.saturating_sub(1)).min(4));
        let eff = ((ms as i64) * factor).min(MAX_COOLDOWN_MS).max(0);
        map.insert(
            key,
            Rest {
                until: now + eff,
                reason: reason.to_string(),
                hits,
                probing: false,
                source,
                bench_ms: eff,
                set_at: now,
                recovered: false,
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
                if rest.recovered {
                    continue;
                }
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
                if rest.recovered {
                    continue;
                }
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

    /// 一条冷却是否「歇够一半、并且还剩 60 秒以上」。
    ///
    /// 只剩几秒时解除没有意义（下一次真实请求本来就能进），
    /// 刚进冷却就解除更没有意义（上游大概率还没缓过来）。
    fn ripe(rest: &Rest, now: i64) -> bool {
        if rest.recovered || !rest.source.probeable() {
            return false;
        }
        let left = rest.until - now;
        let elapsed = now - rest.set_at;
        if left < RECOVER_MIN_LEFT_MS || rest.bench_ms <= 0 {
            return false;
        }
        (elapsed as f64) >= (rest.bench_ms as f64) * RECOVER_MIN_SERVED
    }

    /// 探针验过这条凭据 + 这个模型之后，提前解除**猜测性**冷却。
    ///
    /// 作用域跟着探针的结论走：一条 3 token 的 liveness 成功，
    /// 同时证明了三件事——这家厂商可达（`p:`）、这条 key 能用（`k:`）、
    /// 这个模型在这个账号上有响应（`m:`）。但只推翻我们猜的：
    /// 上游明说的重置时刻还在，402 的欠费也还在。
    ///
    /// 返回解除的条数，供日志与测试用。
    pub fn recover_after_probe(
        &self,
        provider_id: &str,
        account_id: &str,
        model_id: Option<&str>,
        now: i64,
    ) -> usize {
        let mut map = self.map.lock().unwrap();
        let mut n = 0;
        for key in Self::keys(provider_id, account_id, model_id) {
            if key.is_empty() {
                continue;
            }
            if let Some(rest) = map.get_mut(&key) {
                if Self::ripe(rest, now) {
                    // 保留 hits：解除的是这一次等待，不是它的前科
                    rest.recovered = true;
                    rest.probing = false;
                    n += 1;
                }
            }
        }
        // 顺手清掉一天前的「已解除」记录，别让历史无限长
        map.retain(|_, r| !r.recovered || now - r.set_at < HISTORY_KEEP_MS);
        n
    }

    pub fn snapshot(&self) -> Vec<CooldownSnapshot> {
        let map = self.map.lock().unwrap();
        let now = now_ms();
        map.iter()
            // 已解除的不出现在 UI 里：它不是「还在冷却」，只是前科记录
            .filter(|(_, v)| !v.recovered)
            .map(|(k, v)| CooldownSnapshot {
                key: k.clone(),
                until: v.until,
                reason: v.reason.clone(),
                hits: v.hits,
                half_open: now >= v.until,
                probing: v.probing,
                source: v.source.as_str().to_string(),
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
        assert!(cd.snapshot().iter().all(|c| c.key != "k:acct_a"));
        assert!(cd.snapshot().iter().any(|c| c.key == "p:groq"));
    }

    /// 探针只能推翻我们猜的；上游明说的重置时刻不能被提前解除。
    #[test]
    fn probe_only_recovers_heuristic_cooldowns() {
        let cd = Cooldowns::new();
        let t0 = 1_000_000;
        cd.hit_at(Source::Heuristic, Scope::Key, "groq", "acct_a", None, 600_000, "rate_limit", t0);
        cd.hit_at(Source::Authoritative, Scope::Model, "groq", "acct_a", Some("llama"), 600_000, "quota_exhausted", t0);

        // 歇了 400s（>50%）、还剩 200s（>60s）：heuristic 该解，authoritative 不动
        let n = cd.recover_after_probe("groq", "acct_a", Some("llama"), t0 + 400_000);
        assert_eq!(n, 1);
        // 注入时钟不能拿去和 state_for 的真钟比，只核对「还在不在快照里」
        let snap = cd.snapshot();
        assert!(snap.iter().all(|c| c.key != "k:acct_a"), "heuristic 已解除");
        let keep = snap.iter().find(|c| c.key == "m:groq:llama").expect("authoritative 应当还在");
        assert_eq!(keep.source, "authoritative");
    }

    /// 刚进冷却就探、或快到期才探，都不值得解除。
    #[test]
    fn probe_requires_half_served_and_a_minute_left() {
        let cd = Cooldowns::new();
        let t0 = 1_000_000;
        cd.hit_at(Source::Heuristic, Scope::Key, "groq", "acct_a", None, 600_000, "rate_limit", t0);
        assert_eq!(cd.recover_after_probe("groq", "acct_a", None, t0 + 60_000), 0, "才歇 10%");

        cd.hit_at(Source::Heuristic, Scope::Key, "groq", "acct_b", None, 600_000, "rate_limit", t0);
        assert_eq!(
            cd.recover_after_probe("groq", "acct_b", None, t0 + 580_000),
            0,
            "只剩 20 秒，本来就要到期了"
        );
    }

    /// 解除的是等待，不是前科：下次再挂仍然从上一档继续翻倍。
    #[test]
    fn recovery_keeps_the_escalation_history() {
        let cd = Cooldowns::new();
        let t0 = 1_000_000;
        cd.hit_at(Source::Heuristic, Scope::Key, "groq", "acct_a", None, 600_000, "rate_limit", t0);
        cd.recover_after_probe("groq", "acct_a", None, t0 + 400_000);
        let next = cd.hit_at(Source::Heuristic, Scope::Key, "groq", "acct_a", None, 600_000, "rate_limit", t0 + 400_001);
        assert_eq!(next, 1_200_000, "第二次失败应当翻倍，而不是回到 10 分钟");
    }
}