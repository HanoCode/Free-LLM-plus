//! 健康状态机。
//!
//! 规则来自 §3.4.1：
//! - **连续 2 次**失败才判 `unavailable`，单次失败只降到 `degraded`
//! - 成功即回到 `healthy`；慢但成功（>10s）算 `degraded`
//! - 真实流量的失败只喂给冷却器，不直接改模型健康 —— 单次 502 不该让一条模型
//!   从目录里消失

use freeport_core::model::{HealthStatus, ModelCaps, ModelHealth};

pub const FAILS_BEFORE_UNAVAILABLE: u32 = 2;
/// 超过这个延迟就算「慢」，标 degraded 但仍然可路由
pub const SLOW_MS: u64 = 10_000;

pub fn apply_ok(h: &mut ModelHealth, latency_ms: u64, now: &str) {
    h.fails = 0;
    h.oks = h.oks.saturating_add(1);
    h.latency_ms = Some(latency_ms);
    h.checked_at = Some(now.to_string());
    h.last_ok_at = Some(now.to_string());
    h.error = None;
    h.status = if latency_ms > SLOW_MS {
        HealthStatus::Degraded
    } else {
        HealthStatus::Healthy
    };
}

pub fn apply_fail(h: &mut ModelHealth, error: impl Into<String>, now: &str) {
    h.oks = 0;
    h.fails = h.fails.saturating_add(1);
    h.latency_ms = None;
    h.checked_at = Some(now.to_string());
    h.error = Some(error.into());
    h.status = if h.fails >= FAILS_BEFORE_UNAVAILABLE {
        HealthStatus::Unavailable
    } else {
        HealthStatus::Degraded
    };
}

/// 「这个模型没有免费档」：一次就判死。
///
/// 普通的失败要连续两次才 unavailable，因为单次 5xx 可能只是抖动；
/// 但 `limit: 0` 是稳定事实，再试一次纯属浪费额度。判死后
/// `UNAVAILABLE_RETRY_MIN` 会安排一次复检，厂商真开放了免费额度能自己恢复。
pub fn apply_no_free_tier(h: &mut ModelHealth, error: impl Into<String>, now: &str) {
    h.fails = FAILS_BEFORE_UNAVAILABLE;
    apply_fail(h, error, now);
}

/// 实测能力覆盖目录声明。
pub fn apply_caps(h: &mut ModelHealth, caps: ModelCaps) {
    if caps.is_empty() {
        return;
    }
    h.caps = h.caps.merge_measured(caps);
}

/// 记下一个「实测不支持」的能力。
///
/// 和 [`apply_caps`] 的区别：那条是探针**主动测出来的肯定结论**，覆盖全部字段；
/// 这条是真实请求被上游拒了，只否定被测的那一项，其余保持原样。
/// 未知（`None`）不能被这里当成 false —— 它只是信息缺口。
pub fn deny_cap(h: &mut ModelHealth, cap: &str) -> bool {
    match cap {
        "tools" => h.caps.tools = Some(false),
        "vision" => h.caps.vision = Some(false),
        "json" => h.caps.json_mode = Some(false),
        "reasoning" => h.caps.reasoning = Some(false),
        _ => return false,
    }
    true
}

pub fn note_used(h: &mut ModelHealth, now: &str) {
    h.last_used_at = Some(now.to_string());
}

/// 冷却器触发的临时不可用：不改连续失败计数，只看时间。
pub fn mark_cooling(h: &mut ModelHealth) {
    h.status = HealthStatus::Cooling;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> ModelHealth {
        ModelHealth::default()
    }

    #[test]
    fn single_failure_does_not_mark_unavailable() {
        let mut h = fresh();
        apply_ok(&mut h, 120, "2026-10-19T00:00:00Z");
        assert_eq!(h.status, HealthStatus::Healthy);

        apply_fail(&mut h, "502", "2026-10-19T00:05:00Z");
        assert_eq!(h.status, HealthStatus::Degraded);
        assert_eq!(h.fails, 1);
    }

    #[test]
    fn second_consecutive_failure_marks_unavailable() {
        let mut h = fresh();
        apply_fail(&mut h, "502", "2026-10-19T00:00:00Z");
        apply_fail(&mut h, "timeout", "2026-10-19T00:05:00Z");
        assert_eq!(h.status, HealthStatus::Unavailable);
    }

    #[test]
    fn success_recovers_and_resets_counter() {
        let mut h = fresh();
        apply_fail(&mut h, "e1", "2026-10-19T00:00:00Z");
        apply_fail(&mut h, "e2", "2026-10-19T00:05:00Z");
        apply_ok(&mut h, 300, "2026-10-19T00:10:00Z");
        assert_eq!(h.status, HealthStatus::Healthy);
        assert_eq!(h.fails, 0);
        assert_eq!(h.oks, 1);
        assert!(h.error.is_none());
    }

    #[test]
    fn slow_success_is_degraded_but_usable() {
        let mut h = fresh();
        apply_ok(&mut h, SLOW_MS + 1, "2026-10-19T00:00:00Z");
        assert_eq!(h.status, HealthStatus::Degraded);
        assert!(h.status.is_usable());
    }

    #[test]
    fn measured_caps_override_declared() {
        let mut h = fresh();
        apply_caps(
            &mut h,
            ModelCaps {
                tools: Some(false),
                ..Default::default()
            },
        );
        apply_caps(
            &mut h,
            ModelCaps {
                tools: Some(true),
                json_mode: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(h.caps.tools, Some(true));
        assert_eq!(h.caps.json_mode, Some(true));
    }

    #[test]
    fn no_free_tier_dies_on_the_first_probe() {
        let mut h = fresh();
        apply_no_free_tier(&mut h, "limit: 0", "2026-10-19T00:00:00Z");
        assert_eq!(h.status, HealthStatus::Unavailable, "稳定事实不必试两次");
        assert!(h.error.is_some());
    }

    #[test]
    fn unavailable_is_not_usable() {
        assert!(!HealthStatus::Unavailable.is_usable());
        assert!(!HealthStatus::Cooling.is_usable());
        assert!(HealthStatus::Degraded.is_usable());
        assert!(HealthStatus::Unknown.is_usable());
    }

    #[test]
    fn deny_cap_only_touches_the_rejected_one() {
        let mut h = fresh();
        h.caps.tools = Some(true);
        assert!(deny_cap(&mut h, "vision"));
        assert_eq!(h.caps.vision, Some(false));
        assert_eq!(h.caps.tools, Some(true), "没被测到的能力不能被顺手清掉");
        assert_eq!(h.caps.json_mode, None, "未知还是未知");
    }

    #[test]
    fn deny_cap_ignores_unknown_capability_names() {
        let mut h = fresh();
        assert!(!deny_cap(&mut h, "telepathy"));
        assert!(h.caps.is_empty());
    }
}
