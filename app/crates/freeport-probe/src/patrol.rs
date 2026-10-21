//! 巡检到期判定。
//!
//! 不做「全量定时扫」——那会在某一天把预算一次性烧光。
//! 而是按模型自己的节奏排队：有真实流量的 6 小时一检，冷的 24 小时一检，
//! 从没测过的排最前。每日 03:00 再做一次全量兜底体检。

use chrono::{Datelike, Local, Timelike};

use freeport_core::model::{parse_rfc3339_ms, AppData, HealthStatus, PatrolConfig, Purpose};

use crate::health;

/// 有真实流量后多久算「热模型」
const HOT_WINDOW_HOURS: i64 = 24;
/// 判定 unavailable 后的复检间隔：30 分钟。太慢会让模型「消失一整天」。
const UNAVAILABLE_RETRY_MIN: i64 = 30;

#[derive(Debug, Clone)]
pub struct DueItem {
    /// `provider:model`
    pub key: String,
    pub provider: String,
    pub model: String,
    pub hot: bool,
    /// 逾期多久（毫秒）。从没测过的给一个很大的值，排最前。
    pub overdue_ms: i64,
}

pub fn key(provider: &str, model: &str) -> String {
    format!("{provider}:{model}")
}

/// 按到期时间排序的候选队列。
pub fn due(
    data: &AppData,
    now_ms: i64,
    limit: usize,
    has_key: &dyn Fn(&str) -> bool,
) -> Vec<DueItem> {
    let mut items = collect(data, now_ms, has_key, false);
    items.truncate(limit);
    items
}

/// 每日全量体检：忽略间隔，所有可用的 provider×model 都排进来。
/// 仍然受预算约束（调用方逐条 `try_spend`）。
pub fn all(data: &AppData, has_key: &dyn Fn(&str) -> bool) -> Vec<DueItem> {
    collect(data, chrono::Utc::now().timestamp_millis(), has_key, true)
}

fn collect(
    data: &AppData,
    now_ms: i64,
    has_key: &dyn Fn(&str) -> bool,
    ignore_interval: bool,
) -> Vec<DueItem> {
    let hot_h = (data.patrol.hot_hours.max(1) as i64) * 3_600_000;
    let cold_h = (data.patrol.cold_hours.max(1) as i64) * 3_600_000;
    let hot_window = HOT_WINDOW_HOURS * 3_600_000;

    let mut out = Vec::new();
    for p in data.providers.iter().filter(|p| p.enabled) {
        if !has_key(&p.id) {
            continue;
        }
        // 巡检只碰「不限用途」或明确标了 `probe` 的账号。
        //
        // 整家都被标成别的用途时必须**在入队前**跳过，不能等到发请求时再 continue：
        // 那样每次巡检都会挑中它、发现没账号、跳过，而它的 overdue 只增不减，
        // 永远排在最前面，把真正该测的模型饿死。
        if !p.live_accounts().any(|a| a.admits(Purpose::Probe)) {
            continue;
        }
        for m in &p.models {
            let k = key(&p.id, &m.id);
            let h = data.model_health.get(&k);
            // retired 已移出路由，不再烧探针预算；唤醒靠手动重探或目录热更新
            if h.map(|h| h.status == HealthStatus::Retired).unwrap_or(false) {
                continue;
            }
            let hot = h
                .and_then(|h| h.last_used_at.as_deref())
                .and_then(parse_rfc3339_ms)
                .map(|t| now_ms - t < hot_window)
                .unwrap_or(false);
            let interval = if ignore_interval {
                0
            } else if h.map(|h| h.status == HealthStatus::Unavailable).unwrap_or(false) {
                // 判死之后 30 分钟必须复检一次，否则模型会「消失一整天」
                UNAVAILABLE_RETRY_MIN * 60_000
            } else if hot {
                hot_h
            } else {
                cold_h
            };

            let overdue = match h.and_then(|h| h.checked_at.as_deref()).and_then(parse_rfc3339_ms) {
                Some(checked) => now_ms - checked - interval,
                None => i64::MAX / 4, // 从没探过
            };
            if overdue < 0 {
                continue;
            }
            out.push(DueItem {
                key: k,
                provider: p.id.clone(),
                model: m.id.clone(),
                hot,
                overdue_ms: overdue,
            });
        }
    }

    out.sort_by(|a, b| b.overdue_ms.cmp(&a.overdue_ms));
    out
}

/// 巡检前跑一遍：把「连续 7 天 unavailable」的模型转 retired（§3.5.1 状态机的最后一站）。
/// 返回本次 retired 的 `provider:model` key，调用方据此记日志。
/// retired 之后不再进 `due()` 队列，探针预算让给活着的模型；
/// 厂商恢复免费额度后由手动「重新探测」唤醒（apply_ok 清掉 retired）。
pub fn retire_pass(data: &mut AppData, now_ms: i64) -> Vec<String> {
    let mut retired = Vec::new();
    for (k, h) in data.model_health.iter_mut() {
        if health::retire_if_dead(h, now_ms) {
            retired.push(k.clone());
        }
    }
    retired
}

/// 本机的全量体检抖动（分钟，-90..+90）。
///
/// 所有用户都在 03:00 整打上游，等于我们自己造了一个尖峰；免费额度本来就紧，
/// 尖峰里的 429 会被用户读成「这软件没用」。抖动由 `install_id` 决定，
/// 同一台机器每天同一时刻，重启也不变——不会出现「昨天 3 点今天 4 点」。
pub fn sweep_jitter_min(install_id: &str) -> i64 {
    if install_id.is_empty() {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for b in install_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    (h % 180) as i64 - 90
}

/// 今天该不该做全量体检（固定时刻，无抖动）。单测与不需要抖动的调用方用这个。
pub fn sweep_due(state: &PatrolConfig, now: chrono::DateTime<Local>) -> bool {
    sweep_due_at(state, now, 0)
}

/// 带抖动的版本：`jitter_min` 由 [`sweep_jitter_min`] 从 install_id 算出。
pub fn sweep_due_at(state: &PatrolConfig, now: chrono::DateTime<Local>, jitter_min: i64) -> bool {
    let (hh, mm) = parse_hhmm(&state.sweep_at).unwrap_or((3, 0));
    let target = (hh as i64 * 60 + mm as i64 + jitter_min).clamp(0, 24 * 60 - 1) as u32;
    let (hh, mm) = (target / 60, target % 60);
    let today = format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        now.month(),
        now.day()
    );
    if state.last_sweep_day.as_deref() == Some(today.as_str()) {
        return false;
    }
    now.hour() * 60 + now.minute() >= hh * 60 + mm
}

pub fn parse_hhmm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h < 24 && m < 60 {
        Some((h, m))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_core::model::{Health, HealthStatus, ModelHealth, ModelInfo, Provider};

    fn provider(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: "https://x/v1".into(),
            tier: "A".into(),
            homepage: None,
            note: None,
            key_mask: "k".into(),
            enabled: true,
            added_at: String::new(),
            quota: None,
            accounts: vec![freeport_core::model::Account {
                id: id.into(),
                name: "default".into(),
                key_mask: "k".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                health: None,
                purposes: Vec::new(),
            }],
            proxy: None,
            models: vec![ModelInfo {
                id: "m1".into(),
                owned_by: id.into(),
                context: None,
                created: None,
                free: None,
                caps: Default::default(),
                candidate: false,
            }],
            health: Health::default(),
        }
    }

    #[test]
    fn never_probed_is_due_first() {
        let mut d = AppData::default();
        d.providers = vec![provider("a")];
        let items = due(&d, 1_000_000, 10, &|_| true);
        assert_eq!(items.len(), 1);
        assert!(!items[0].hot);
        assert!(items[0].overdue_ms > 0);
    }

    #[test]
    fn fresh_probe_is_not_due() {
        let mut d = AppData::default();
        d.providers = vec![provider("a")];
        let now = chrono::Utc::now();
        d.model_health.insert(
            "a:m1".into(),
            ModelHealth {
                status: HealthStatus::Healthy,
                checked_at: Some(now.to_rfc3339()),
                ..Default::default()
            },
        );
        let items = due(&d, now.timestamp_millis(), 10, &|_| true);
        assert!(items.is_empty());
    }

    #[test]
    fn hot_models_are_due_sooner() {
        let mut d = AppData::default();
        d.providers = vec![provider("a")];
        let now = chrono::Utc::now();
        // 7 小时前测过，热模型（6h 周期）已到期，冷模型（24h）还没
        let checked = now - chrono::Duration::hours(7);
        d.model_health.insert(
            "a:m1".into(),
            ModelHealth {
                status: HealthStatus::Healthy,
                checked_at: Some(checked.to_rfc3339()),
                last_used_at: Some(now.to_rfc3339()),
                ..Default::default()
            },
        );
        assert_eq!(due(&d, now.timestamp_millis(), 10, &|_| true).len(), 1);

        d.model_health.get_mut("a:m1").unwrap().last_used_at = None;
        assert!(due(&d, now.timestamp_millis(), 10, &|_| true).is_empty());
    }

    #[test]
    fn providers_without_key_are_skipped() {
        let mut d = AppData::default();
        d.providers = vec![provider("a")];
        assert!(due(&d, 0, 10, &|_| false).is_empty());
    }

    #[test]
    fn retired_models_leave_the_patrol_queue() {
        let mut d = AppData::default();
        d.providers = vec![provider("a")];
        let now = chrono::Utc::now();
        // 旧检查时间，保证正常情况下必到期
        let old = (now - chrono::Duration::hours(48)).to_rfc3339();
        d.model_health.insert(
            "a:m1".into(),
            ModelHealth {
                status: HealthStatus::Unavailable,
                checked_at: Some(old.clone()),
                ..Default::default()
            },
        );
        d.model_health.insert(
            "a:m1-retired".into(),
            ModelHealth {
                status: HealthStatus::Retired,
                checked_at: Some(old.clone()),
                ..Default::default()
            },
        );
        // health 里挂了两个 key，但 provider 只有 m1 一条模型 —— due 只看目录里有的
        let items = due(&d, now.timestamp_millis(), 10, &|_| true);
        assert_eq!(items.len(), 1);

        // 把 m1 直接标 retired，队列立刻清空
        d.model_health.get_mut("a:m1").unwrap().status = HealthStatus::Retired;
        assert!(due(&d, now.timestamp_millis(), 10, &|_| true).is_empty());
    }

    #[test]
    fn retire_pass_flips_only_seven_day_dead_models() {
        use crate::health::RETIRE_AFTER_MS;
        let mut d = AppData::default();
        let t0 = parse_rfc3339_ms("2026-10-01T00:00:00Z").unwrap();
        let fresh_fail = "2026-10-19T08:00:00Z";
        d.model_health.insert(
            "a:dead".into(),
            ModelHealth {
                status: HealthStatus::Unavailable,
                unavailable_since: Some("2026-10-01T00:00:00Z".into()),
                checked_at: Some(fresh_fail.into()),
                ..Default::default()
            },
        );
        d.model_health.insert(
            "a:recent".into(),
            ModelHealth {
                status: HealthStatus::Unavailable,
                unavailable_since: Some("2026-10-18T00:00:00Z".into()),
                checked_at: Some(fresh_fail.into()),
                ..Default::default()
            },
        );
        let retired = retire_pass(&mut d, t0 + RETIRE_AFTER_MS);
        assert_eq!(retired, vec!["a:dead".to_string()]);
        assert_eq!(d.model_health["a:dead"].status, HealthStatus::Retired);
        assert_eq!(d.model_health["a:recent"].status, HealthStatus::Unavailable);
    }

    #[test]
    fn sweep_jitter_is_stable_per_install_and_within_90_minutes() {
        let a = sweep_jitter_min("install-abc");
        assert_eq!(a, sweep_jitter_min("install-abc"), "同一台机器必须天天同一时刻");
        assert!((-90..90).contains(&a));
        assert_eq!(sweep_jitter_min(""), 0, "没有 install_id 就不抖，别把时间抖到未知处");
        // 抖动真的会把时刻挪走（不同 install_id 至少有一个不同）
        let b = sweep_jitter_min("install-xyz");
        assert!(a != b || a == 0, "抖动应当按机器分散");
    }

    #[test]
    fn jitter_moves_the_due_minute() {
        use chrono::TimeZone;
        let mut s = PatrolConfig::default();
        s.sweep_at = "03:00".into();
        let at_three = Local.with_ymd_and_hms(2026, 10, 19, 3, 1, 0).unwrap();
        assert!(sweep_due_at(&s, at_three, 0));
        assert!(!sweep_due_at(&s, at_three, 60), "往后抖 1 小时，3:01 还没到");
        assert!(
            sweep_due_at(&s, at_three, -60),
            "往前抖 1 小时，3:01 已经过了"
        );
    }

    #[test]
    fn sweep_runs_once_after_the_hour() {
        use chrono::TimeZone;
        let mut s = PatrolConfig::default();
        s.sweep_at = "03:00".into();
        let at_two = Local.with_ymd_and_hms(2026, 10, 19, 2, 59, 0).unwrap();
        let at_three = Local.with_ymd_and_hms(2026, 10, 19, 3, 1, 0).unwrap();
        assert!(!sweep_due(&s, at_two));
        assert!(sweep_due(&s, at_three));
        s.last_sweep_day = Some("2026-10-19".into());
        assert!(!sweep_due(&s, at_three));
    }
}
