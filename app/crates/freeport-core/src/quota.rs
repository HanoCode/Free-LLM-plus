//! 四窗口额度账本。
//!
//! 免费额度的特征是「按窗口过期、不可结转」。所以路由的最优解不是选最好的模型，
//! 而是优先花最先作废的额度 —— 这需要一本能回答「现在还剩多少、什么时候重置」的账。
//!
//! 两个刻意的设计：
//! 1. **用户流量与巡检分开计**。RPD 耗尽时不能再服务用户，但一条 3 token 的
//!    liveness 仍然值得跑 —— 混记就做不出这个区分，也解释不了「我只发了 3 次请求，
//!    额度怎么没了」。
//! 2. **窗口惰性归零**。读的时候比对 `reset_at`，不建定时任务，重启也不丢账。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::cooldown::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    /// 每分钟请求数
    Rpm,
    /// 每天请求数
    Rpd,
    /// 每分钟 token
    Tpm,
    /// 每天 token
    Tpd,
}

impl WindowKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rpm => "rpm",
            Self::Rpd => "rpd",
            Self::Tpm => "tpm",
            Self::Tpd => "tpd",
        }
    }

    pub fn window_ms(&self) -> i64 {
        match self {
            Self::Rpm | Self::Tpm => 60_000,
            Self::Rpd | Self::Tpd => 86_400_000,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Window {
    #[serde(default, rename = "usedUser")]
    pub used_user: f64,
    /// 巡检消耗，单列。上游看到的是两者之和，但 UI 要分开解释。
    #[serde(default, rename = "usedProbe")]
    pub used_probe: f64,
    /// 0 表示未知 / 不限
    #[serde(default)]
    pub limit: f64,
    /// 归零时刻（ms）。0 表示还没开始计时。
    #[serde(default, rename = "resetAt")]
    pub reset_at: i64,
}

impl Window {
    fn roll(&mut self, kind: WindowKind, now: i64) {
        if self.reset_at == 0 || now >= self.reset_at {
            self.used_user = 0.0;
            self.used_probe = 0.0;
            self.reset_at = now + kind.window_ms();
        }
    }

    pub fn used_total(&self) -> f64 {
        self.used_user + self.used_probe
    }

    pub fn exhausted(&self) -> bool {
        self.limit > 0.0 && self.used_total() >= self.limit
    }

    pub fn remaining_ratio(&self) -> f64 {
        if self.limit <= 0.0 {
            return 1.0;
        }
        ((self.limit - self.used_total()) / self.limit).clamp(0.0, 1.0)
    }
}

/// 台账。key 是 `provider:window`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    windows: BTreeMap<String, Window>,
}

impl Ledger {
    fn key(provider: &str, kind: WindowKind) -> String {
        format!("{provider}:{}", kind.as_str())
    }

    fn slot(&mut self, provider: &str, kind: WindowKind, limit: f64, now: i64) -> &mut Window {
        let w = self
            .windows
            .entry(Self::key(provider, kind))
            .or_default();
        // 上限以配置为准：用户改了配额，下一次请求就生效
        if limit > 0.0 {
            w.limit = limit;
        }
        w.roll(kind, now);
        w
    }

    /// 把配置里的上限写进台账（配置变更 / 首次加载时调用）。
    pub fn sync_limits(&mut self, provider: &str, limits: [f64; 4], now: i64) {
        for (kind, limit) in [
            (WindowKind::Rpm, limits[0]),
            (WindowKind::Rpd, limits[1]),
            (WindowKind::Tpm, limits[2]),
            (WindowKind::Tpd, limits[3]),
        ] {
            let w = self.slot(provider, kind, limit, now);
            if limit <= 0.0 {
                w.limit = 0.0;
            }
        }
    }

    /// 学到厂商自己说的上限（来自 429 body）。
    ///
    /// **只收紧**：用户已经配了更小的值时不放大，避免一次上游报数把用户的
    /// 保守配置冲掉。没配过则直接采纳，账本从此按真值算余量。
    pub fn learn(&mut self, account: &str, kind: WindowKind, limit: f64, now: i64) {
        if limit <= 0.0 || !limit.is_finite() {
            return;
        }
        let old = self.window(account, kind).map(|w| w.limit).unwrap_or(0.0);
        if old > 0.0 && old <= limit {
            // 已有更紧的上限：只让它按窗口惰性归零，不动限额
            self.slot(account, kind, 0.0, now);
            return;
        }
        self.slot(account, kind, limit, now);
    }

    /// 记账：请求数 +1，token 预扣。`tokens` 用 `input_est + max_tokens` 上限，
    /// 并发下不会超卖，代价是偏保守。
    pub fn reserve(&mut self, provider: &str, limits: [f64; 4], tokens: f64, probe: bool, now: i64) {
        for (kind, limit, amount) in [
            (WindowKind::Rpm, limits[0], 1.0),
            (WindowKind::Rpd, limits[1], 1.0),
            (WindowKind::Tpm, limits[2], tokens),
            (WindowKind::Tpd, limits[3], tokens),
        ] {
            let w = self.slot(provider, kind, limit, now);
            if probe {
                w.used_probe += amount;
            } else {
                w.used_user += amount;
            }
        }
    }

    /// 结算：把预扣的 token 换成实际值。请求数不动（上游确实收了这一次）。
    pub fn settle(
        &mut self,
        provider: &str,
        limits: [f64; 4],
        reserved_tokens: f64,
        actual_tokens: f64,
        probe: bool,
        now: i64,
    ) {
        let delta = actual_tokens - reserved_tokens;
        if delta == 0.0 {
            return;
        }
        for (kind, limit) in [
            (WindowKind::Tpm, limits[2]),
            (WindowKind::Tpd, limits[3]),
        ] {
            let w = self.slot(provider, kind, limit, now);
            let field = if probe {
                &mut w.used_probe
            } else {
                &mut w.used_user
            };
            *field = (*field + delta).max(0.0);
        }
    }

    /// 还能不能服务用户：任一窗口耗尽就出局。
    pub fn has_room(&self, provider: &str, now: i64) -> bool {
        let mut has_limit = false;
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            if let Some(w) = self.windows.get(&Self::key(provider, kind)) {
                if w.limit <= 0.0 {
                    continue;
                }
                has_limit = true;
                if now < w.reset_at && w.exhausted() {
                    return false;
                }
            }
        }
        // 一条限额都没配：只记账，不拦截
        let _ = has_limit;
        true
    }

    /// 配额余量比例，Pick 层用来做「快烧完了就让位」。
    pub fn remaining_ratio(&self, provider: &str, now: i64) -> f64 {
        let mut min = 1.0_f64;
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            if let Some(w) = self.windows.get(&Self::key(provider, kind)) {
                if w.limit <= 0.0 || now >= w.reset_at {
                    continue;
                }
                min = min.min(w.remaining_ratio());
            }
        }
        min
    }

    /// 最早恢复时刻；当前没有耗尽返回 None。渲染「预计 14:32 恢复」用。
    pub fn next_reset_at(&self, provider: &str, now: i64) -> Option<i64> {
        let mut earliest: Option<i64> = None;
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            if let Some(w) = self.windows.get(&Self::key(provider, kind)) {
                if w.limit > 0.0 && now < w.reset_at && w.exhausted() {
                    earliest = Some(earliest.map_or(w.reset_at, |e| e.min(w.reset_at)));
                }
            }
        }
        earliest
    }

    /// 凭据被删掉时清掉它的四个窗口，别让台账无限攒死账号。
    pub fn forget(&mut self, account: &str) {
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            self.windows.remove(&Self::key(account, kind));
        }
    }

    pub fn window(&self, provider: &str, kind: WindowKind) -> Option<&Window> {
        self.windows.get(&Self::key(provider, kind))
    }

    // ── 以下为免传 `now` 的便捷入口，调用方绝大多数场景用这几个 ──

    pub fn has_room_now(&self, provider: &str) -> bool {
        self.has_room(provider, now_ms())
    }

    pub fn reserve_now(&mut self, provider: &str, limits: [f64; 4], tokens: f64, probe: bool) {
        self.reserve(provider, limits, tokens, probe, now_ms());
    }

    pub fn settle_now(
        &mut self,
        provider: &str,
        limits: [f64; 4],
        reserved: f64,
        actual: f64,
        probe: bool,
    ) {
        self.settle(provider, limits, reserved, actual, probe, now_ms());
    }

    pub fn remaining_ratio_now(&self, provider: &str) -> f64 {
        self.remaining_ratio(provider, now_ms())
    }

    /// 当前耗尽时的恢复时刻；UI 渲染「预计 14:32 恢复」。
    pub fn next_reset_now(&self, provider: &str) -> Option<i64> {
        self.next_reset_at(provider, now_ms())
    }
}

/// 免费额度的典型约束分布差异很大（RPD / TPM / credits），
/// 所以上限只认「配置里写了才算」，没写就不拦。
pub fn limits_of(q: Option<&crate::model::ProviderQuota>) -> [f64; 4] {
    let Some(q) = q else {
        return [0.0; 4];
    };
    [
        q.rpm.map(|v| v as f64).unwrap_or(0.0),
        q.rpd.map(|v| v as f64).unwrap_or(0.0),
        q.tpm.map(|v| v as f64).unwrap_or(0.0),
        q.tpd.map(|v| v as f64).unwrap_or(0.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_LIMITS: [f64; 4] = [0.0; 4];

    #[test]
    fn request_count_and_tokens_are_independent() {
        let mut l = Ledger::default();
        let limits = [60.0, 100.0, 10_000.0, 100_000.0];
        // 60 次请求把 RPM 打满，但 token 只用了 600
        for _ in 0..60 {
            l.reserve("p", limits, 10.0, false, 1_000);
        }
        assert!(!l.has_room("p", 1_000), "RPM 先耗尽");
        let tpm = l.window("p", WindowKind::Tpm).unwrap();
        assert_eq!(tpm.used_total(), 600.0);
        assert!(!tpm.exhausted(), "token 还富余");
    }

    #[test]
    fn probe_usage_is_counted_separately() {
        let mut l = Ledger::default();
        let limits = [60.0, 100.0, 0.0, 0.0];
        l.reserve("p", limits, 0.0, false, 1_000);
        l.reserve("p", limits, 0.0, true, 1_000);
        l.reserve("p", limits, 0.0, true, 1_000);
        let w = l.window("p", WindowKind::Rpd).unwrap();
        assert_eq!(w.used_user, 1.0);
        assert_eq!(w.used_probe, 2.0);
        assert_eq!(w.used_total(), 3.0, "上游看到的是总和");
    }

    #[test]
    fn window_resets_lazily() {
        let mut l = Ledger::default();
        let limits = [2.0, 0.0, 0.0, 0.0];
        l.reserve("p", limits, 0.0, false, 1_000);
        l.reserve("p", limits, 0.0, false, 1_000);
        assert!(!l.has_room("p", 1_000));
        // 61 秒后越过了 reset_at，读路径视为已归零，不需要定时任务
        assert!(l.has_room("p", 61_000));
        assert_eq!(l.remaining_ratio("p", 61_000), 1.0);
        // 真正的清零发生在下一次写入
        l.reserve("p", limits, 0.0, false, 61_000);
        assert_eq!(l.window("p", WindowKind::Rpm).unwrap().used_total(), 1.0);
    }

    #[test]
    fn settle_replaces_estimate_with_actual() {
        let mut l = Ledger::default();
        let limits = [0.0, 0.0, 1_000.0, 0.0];
        l.reserve("p", limits, 800.0, false, 1_000);
        assert_eq!(l.window("p", WindowKind::Tpm).unwrap().used_total(), 800.0);
        l.settle("p", limits, 800.0, 120.0, false, 1_000);
        assert_eq!(
            l.window("p", WindowKind::Tpm).unwrap().used_total(),
            120.0,
            "预扣 800 实际 120，应当回吐"
        );
    }

    #[test]
    fn no_limits_never_blocks() {
        let mut l = Ledger::default();
        for _ in 0..1000 {
            l.reserve("p", NO_LIMITS, 1_000.0, false, 1_000);
        }
        assert!(l.has_room("p", 1_000), "没配限额就只记账不拦截");
        assert_eq!(l.remaining_ratio("p", 1_000), 1.0);
    }

    #[test]
    fn learned_limit_fills_the_gap_but_never_loosens() {
        let mut l = Ledger::default();
        // 没配过：采纳厂商报的日上限
        l.learn("acc", WindowKind::Rpd, 250.0, 1_000);
        assert_eq!(l.window("acc", WindowKind::Rpd).unwrap().limit, 250.0);

        // 用户配了更小的 100：学习不能把它放大
        l.sync_limits("acc2", [0.0, 100.0, 0.0, 0.0], 1_000);
        l.learn("acc2", WindowKind::Rpd, 250.0, 1_000);
        assert_eq!(l.window("acc2", WindowKind::Rpd).unwrap().limit, 100.0);

        // 学到 0 或非法值：忽略，不把已有上限抹掉
        l.learn("acc", WindowKind::Rpd, 0.0, 1_000);
        assert_eq!(l.window("acc", WindowKind::Rpd).unwrap().limit, 250.0);
    }
}
