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

    /// 这个窗口数的是 token 还是请求次数。
    ///
    /// 预演时判断「这单装不装得下」只能看 token 窗口：请求数窗口的「剩余」是
    /// 「还能发几次」，拿 3000 个 token 去和 2 次请求比，会得出荒谬的结论。
    pub fn is_tokens(&self) -> bool {
        matches!(self, Self::Tpm | Self::Tpd)
    }
}

/// 当前最紧的那个窗口。预演用来回答「这一单会不会因为某个窗口快满了而挂」。
#[derive(Debug, Clone, Copy)]
pub struct Tight {
    pub kind: WindowKind,
    /// 剩余绝对值：`kind.is_tokens()` 时是 token 数，否则是还能发几次。
    pub remaining: f64,
    pub reset_at: i64,
    /// 剩余比例 0..1。挑「最紧」按它比，不按绝对值——1000 个 token 对
    /// 10 万的上限很宽裕，对 1200 的上限就已经见底。
    pub ratio: f64,
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

    /// 模型级窗口的键：`account|model:kind`。
    ///
    /// 粒度是凭据 × 模型：上游的模型级限额跟着 key 走（两条 key 各有一本账），
    /// 换一个模型也不是同一本。`|` 是刻意的——账号 id（`acct_*`）与模型 id
    /// 都不会出现它，读侧用前缀 `account|` 就能把一个账号的模型窗口一起删掉。
    fn model_key(account: &str, model: &str, kind: WindowKind) -> String {
        format!("{account}|{model}:{}", kind.as_str())
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

    /// 学到**模型级**限额（上游 429 文案点名了模型时）。
    ///
    /// 这张表**没有配置入口**，这是刻意的：目录只有厂商级限额，先造一个
    /// 配置字段等于造一张没人填的表。数据只来自上游明说的事实——
    /// Groq / OpenAI 系的 `Rate limit reached for model \`x\` ... Limit N`。
    /// 语义与 [`learn`](Self::learn) 相同：只收紧，`limit <= 0` 忽略。
    pub fn learn_model(&mut self, account: &str, model: &str, kind: WindowKind, limit: f64, now: i64) {
        if limit <= 0.0 || !limit.is_finite() {
            return;
        }
        let key = Self::model_key(account, model, kind);
        let old = self.windows.get(&key).map(|w| w.limit).unwrap_or(0.0);
        let w = self.windows.entry(key).or_default();
        w.roll(kind, now);
        if old > 0.0 && old <= limit {
            return; // 已有更紧的：不动
        }
        w.limit = limit;
    }

    /// 模型级窗口。只有上游明说过才存在——没学过返回 `None`，不是「不限」。
    pub fn model_window(&self, account: &str, model: &str, kind: WindowKind) -> Option<&Window> {
        self.windows.get(&Self::model_key(account, model, kind))
    }

    /// 模型级门禁：这个模型在这个凭据上还有没有余量。
    ///
    /// 没学过任何模型级窗口时恒真——这条检查只在「上游明说了」的凭据上生效，
    /// 对其它凭据和模型零成本。
    pub fn model_has_room(&self, account: &str, model: &str, now: i64) -> bool {
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            if let Some(w) = self.model_window(account, model, kind) {
                if w.limit > 0.0 && now < w.reset_at && w.exhausted() {
                    return false;
                }
            }
        }
        true
    }

    /// 把一笔用量记进**已学到**的模型级窗口。没学过的窗口不动——
    /// 只在 Gate 需要拦的时候才需要记，不存在「记了账却没人看」的窗口。
    pub fn reserve_model(&mut self, account: &str, model: &str, tokens: f64, probe: bool, now: i64) {
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            let exists = self
                .model_window(account, model, kind)
                .map(|w| w.limit > 0.0)
                .unwrap_or(false);
            if !exists {
                continue;
            }
            let amount = if kind.is_tokens() { tokens } else { 1.0 };
            let w = self.windows.get_mut(&Self::model_key(account, model, kind)).unwrap();
            w.roll(kind, now);
            if probe {
                w.used_probe += amount;
            } else {
                w.used_user += amount;
            }
        }
    }

    /// 模型级窗口的 token 结算（预扣换实际值），与 [`settle`](Self::settle) 同语义。
    pub fn settle_model(
        &mut self,
        account: &str,
        model: &str,
        reserved: f64,
        actual: f64,
        probe: bool,
        now: i64,
    ) {
        let delta = actual - reserved;
        if delta == 0.0 {
            return;
        }
        for kind in [WindowKind::Tpm, WindowKind::Tpd] {
            let exists = self
                .model_window(account, model, kind)
                .map(|w| w.limit > 0.0)
                .unwrap_or(false);
            if !exists {
                continue;
            }
            let w = self.windows.get_mut(&Self::model_key(account, model, kind)).unwrap();
            w.roll(kind, now);
            let field = if probe { &mut w.used_probe } else { &mut w.used_user };
            *field = (*field + delta).max(0.0);
        }
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
    /// 学到的模型级窗口一并清（键都以 `account|` 开头）。
    pub fn forget(&mut self, account: &str) {
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            self.windows.remove(&Self::key(account, kind));
        }
        let prefix = format!("{account}|");
        self.windows.retain(|k, _| !k.starts_with(&prefix));
    }

    pub fn window(&self, provider: &str, kind: WindowKind) -> Option<&Window> {
        self.windows.get(&Self::key(provider, kind))
    }

    /// 余量最紧的窗口。没配上限、或已经过了重置时刻的窗口不算——
    /// 它们不拦截任何请求，报出来只会让人去查一个不存在的问题。
    pub fn tightest(&self, provider: &str, now: i64) -> Option<Tight> {
        let mut out: Option<Tight> = None;
        for kind in [
            WindowKind::Rpm,
            WindowKind::Rpd,
            WindowKind::Tpm,
            WindowKind::Tpd,
        ] {
            let Some(w) = self.windows.get(&Self::key(provider, kind)) else {
                continue;
            };
            if w.limit <= 0.0 || now >= w.reset_at {
                continue;
            }
            let ratio = w.remaining_ratio();
            let t = Tight {
                kind,
                remaining: (w.limit - w.used_total()).max(0.0),
                reset_at: w.reset_at,
                ratio,
            };
            // 严格小于：并列时保留先出现的（rpm 先于 tpd），顺序稳定才适合断言
            if out.map(|p| ratio < p.ratio).unwrap_or(true) {
                out = Some(t);
            }
        }
        out
    }

    /// 这一单的 token 量装不装得下。`None` = 装得下；否则是第一个装不下的
    /// token 窗口及它还剩多少。
    ///
    /// 存在的意义：`has_room` 只判「窗口有没有耗尽」。日窗口还剩 300 token 时
    /// 一条 8k 的请求照样会被放行，然后在上面撞 429 —— 这个函数就是用来
    /// 在预演里把这种「账面上有、实际不够」的跳指出来。
    pub fn shortfall(&self, provider: &str, tokens: f64, now: i64) -> Option<(WindowKind, f64)> {
        for kind in [WindowKind::Tpm, WindowKind::Tpd] {
            let Some(w) = self.windows.get(&Self::key(provider, kind)) else {
                continue;
            };
            if w.limit <= 0.0 || now >= w.reset_at {
                continue;
            }
            let left = (w.limit - w.used_total()).max(0.0);
            if tokens > left {
                return Some((kind, left));
            }
        }
        None
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

    pub fn reserve_model_now(&mut self, account: &str, model: &str, tokens: f64, probe: bool) {
        self.reserve_model(account, model, tokens, probe, now_ms());
    }

    pub fn settle_model_now(
        &mut self,
        account: &str,
        model: &str,
        reserved: f64,
        actual: f64,
        probe: bool,
    ) {
        self.settle_model(account, model, reserved, actual, probe, now_ms());
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

    #[test]
    fn tightest_picks_the_window_that_will_block_first() {
        // 只配了日 token 上限：请求数那几个槽位 limit=0 也会被 reserve 建出来，
        // 但它们不拦任何请求，不能当选「最紧」，否则每跳都在报一个假问题
        let mut l = Ledger::default();
        l.reserve("p", [0.0, 0.0, 0.0, 1_000.0], 300.0, false, 1_000);
        let t = l.tightest("p", 1_000).expect("配了上限就该报");
        assert_eq!(t.kind, WindowKind::Tpd);
        assert_eq!(t.remaining, 700.0);
        assert!((t.ratio - 0.7).abs() < 1e-9);

        // 按比例挑，不按绝对值：tpm 只剩 100 才是真瓶颈，尽管 rpm 还剩 9 次
        let mut m = Ledger::default();
        m.reserve("p", [10.0, 0.0, 1_000.0, 0.0], 900.0, false, 1_000);
        let t = m.tightest("p", 1_000).unwrap();
        assert_eq!(t.kind, WindowKind::Tpm);
        assert_eq!(t.remaining, 100.0);
        assert!((t.ratio - 0.1).abs() < 1e-9);
    }

    #[test]
    fn a_window_past_its_reset_is_not_tight_any_more() {
        let mut l = Ledger::default();
        l.reserve("p", [1.0, 0.0, 0.0, 0.0], 0.0, false, 1_000);
        assert_eq!(l.tightest("p", 1_000).unwrap().remaining, 0.0);
        // 越过 reset_at：读路径视为已归零，没有「紧」可言
        assert!(l.tightest("p", 61_000).is_none());
    }

    #[test]
    fn shortfall_only_measures_tokens_against_token_windows() {
        let mut l = Ledger::default();
        l.reserve("p", [0.0, 0.0, 0.0, 1_000.0], 900.0, false, 1_000);
        let (kind, left) = l.shortfall("p", 500.0, 1_000).expect("只剩 100 token");
        assert_eq!(kind, WindowKind::Tpd);
        assert_eq!(left, 100.0);
        assert!(l.shortfall("p", 100.0, 1_000).is_none(), "刚好装得下");

        // 次数窗口剩 9 次，不该因为「9 < 50」就误判成装不下
        let mut l2 = Ledger::default();
        l2.reserve("p", [0.0, 10.0, 0.0, 0.0], 50.0, false, 1_000);
        assert!(l2.shortfall("p", 50.0, 1_000).is_none());
    }

    // ── 模型级窗口（只来自上游 429 明说，没有配置入口）──

    #[test]
    fn model_window_only_exists_once_upstream_states_it() {
        let mut l = Ledger::default();
        assert!(
            l.model_window("acc", "llama-3", WindowKind::Tpd).is_none(),
            "没学过的模型没有窗口，连 limit=0 的槽位都不该有"
        );
        assert!(l.model_has_room("acc", "llama-3", 1_000), "没学过就不拦");

        l.learn_model("acc", "llama-3", WindowKind::Tpd, 1_000.0, 1_000);
        assert_eq!(
            l.model_window("acc", "llama-3", WindowKind::Tpd).unwrap().limit,
            1_000.0
        );
        // 别的模型不受影响
        assert!(l.model_has_room("acc", "mixtral", 1_000));
        // 同一条凭据学两个模型：互不串账
        l.learn_model("acc", "mixtral", WindowKind::Rpd, 5.0, 1_000);
        assert!(l.model_window("acc", "mixtral", WindowKind::Rpd).is_some());
        assert!(l.model_window("acc", "llama-3", WindowKind::Rpd).is_none());
    }

    #[test]
    fn learned_model_limit_blocks_only_that_model() {
        let mut l = Ledger::default();
        l.learn_model("acc", "llama-3", WindowKind::Rpd, 2.0, 1_000);
        l.reserve_model("acc", "llama-3", 0.0, false, 1_000);
        l.reserve_model("acc", "llama-3", 0.0, false, 1_000);
        assert!(
            !l.model_has_room("acc", "llama-3", 1_000),
            "模型级日额度耗尽要拦"
        );
        assert!(l.model_has_room("acc", "mixtral", 1_000), "别的模型照常");
        // 账号级窗口没建立：账号整体仍有余量，其它模型可用
        assert!(l.has_room("acc", 1_000));
        // 越过重置时刻：读路径视为归零
        assert!(l.model_has_room("acc", "llama-3", 86_401_000));
    }

    #[test]
    fn model_learn_never_loosens_and_ignores_garbage() {
        let mut l = Ledger::default();
        l.learn_model("acc", "m", WindowKind::Tpm, 6_000.0, 1_000);
        l.learn_model("acc", "m", WindowKind::Tpm, 9_000.0, 1_000);
        assert_eq!(l.model_window("acc", "m", WindowKind::Tpm).unwrap().limit, 6_000.0);
        l.learn_model("acc", "m", WindowKind::Tpm, 0.0, 1_000);
        assert_eq!(l.model_window("acc", "m", WindowKind::Tpm).unwrap().limit, 6_000.0);
        // 同窗口学得更紧：采纳
        l.learn_model("acc", "m", WindowKind::Tpm, 3_000.0, 1_000);
        assert_eq!(l.model_window("acc", "m", WindowKind::Tpm).unwrap().limit, 3_000.0);
    }

    #[test]
    fn model_settle_replaces_estimate_and_forget_clears_the_scope() {
        let mut l = Ledger::default();
        l.learn_model("acc", "m", WindowKind::Tpd, 1_000.0, 1_000);
        l.reserve_model("acc", "m", 800.0, false, 1_000);
        l.settle_model("acc", "m", 800.0, 120.0, false, 1_000);
        assert_eq!(
            l.model_window("acc", "m", WindowKind::Tpd).unwrap().used_total(),
            120.0
        );

        l.forget("acc");
        assert!(
            l.model_window("acc", "m", WindowKind::Tpd).is_none(),
            "凭据删除要连学到的模型窗口一起清，否则台账攒死账号"
        );
    }
}
