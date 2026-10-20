//! 探针预算。
//!
//! `clamp(日配额 × 1%, 60, 800)`：白嫖也要有分寸。
//! 预算按天滚动，跨天自动重置；真实流量永远优先，巡检在流量高峰直接让路。

use freeport_core::model::PatrolConfig;

pub const MIN_BUDGET: u32 = 60;
pub const MAX_BUDGET: u32 = 800;

/// 单看总日配额算预算。
pub fn budget_for(total_daily_quota: u32) -> u32 {
    (total_daily_quota / 100).clamp(MIN_BUDGET, MAX_BUDGET)
}

/// 汇总所有已接入厂商的日配额。
///
/// `quotas` 只包含已配置 key 的厂商；未知额度按 0 计（不会凭空放大预算）。
pub fn total_budget(quotas: &[u32], cap: Option<u32>) -> u32 {
    let sum: u32 = quotas.iter().copied().sum();
    let auto = budget_for(sum);
    match cap {
        Some(c) if c > 0 => auto.min(c),
        _ => auto,
    }
}

/// 跨天重置。返回是否真的滚动了。
pub fn roll(state: &mut PatrolConfig, today: &str, budget: u32) -> bool {
    if state.day == today && state.budget_today == budget {
        return false;
    }
    if state.day != today {
        state.day = today.to_string();
        state.used_today = 0;
    }
    state.budget_today = budget;
    true
}

pub fn remaining(state: &PatrolConfig) -> u32 {
    state.budget_today.saturating_sub(state.used_today)
}

/// 扣一次探针。预算耗尽返回 false。
pub fn try_spend(state: &mut PatrolConfig, n: u32) -> bool {
    if remaining(state) < n {
        return false;
    }
    state.used_today += n;
    state.probes_total += 1;
    true
}

pub fn note_ok(state: &mut PatrolConfig) {
    state.probes_ok += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_follows_the_rule() {
        assert_eq!(budget_for(0), MIN_BUDGET);
        assert_eq!(budget_for(1_000), 60); // 1% = 10 → 抬到下限 60
        assert_eq!(budget_for(14_400), 144);
        assert_eq!(budget_for(1_000_000), MAX_BUDGET);
    }

    #[test]
    fn total_budget_sums_then_clamps() {
        assert_eq!(total_budget(&[14_400, 1_000], None), 154);
        assert_eq!(total_budget(&[14_400, 1_000], Some(100)), 100);
        assert_eq!(total_budget(&[], None), MIN_BUDGET);
    }

    #[test]
    fn day_rollover_resets_usage() {
        let mut s = PatrolConfig::default();
        assert!(roll(&mut s, "2026-10-19", 120));
        assert!(try_spend(&mut s, 5));
        assert_eq!(s.used_today, 5);
        assert!(roll(&mut s, "2026-10-20", 120));
        assert_eq!(s.used_today, 0);
    }

    #[test]
    fn budget_is_a_hard_stop() {
        let mut s = PatrolConfig::default();
        s.day = "2026-10-19".into();
        s.budget_today = 2;
        assert!(try_spend(&mut s, 2));
        assert!(!try_spend(&mut s, 1));
        assert_eq!(s.probes_total, 1);
    }
}
