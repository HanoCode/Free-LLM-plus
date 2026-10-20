//! 巡检到期判定。
//!
//! 不做「全量定时扫」——那会在某一天把预算一次性烧光。
//! 而是按模型自己的节奏排队：有真实流量的 6 小时一检，冷的 24 小时一检，
//! 从没测过的排最前。每日 03:00 再做一次全量兜底体检。

use chrono::{Datelike, Local, Timelike};

use freeport_core::model::{parse_rfc3339_ms, AppData, HealthStatus, PatrolConfig};

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
        for m in &p.models {
            let k = key(&p.id, &m.id);
            let h = data.model_health.get(&k);
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

/// 今天该不该做全量体检。
pub fn sweep_due(state: &PatrolConfig, now: chrono::DateTime<Local>) -> bool {
    let (hh, mm) = parse_hhmm(&state.sweep_at).unwrap_or((3, 0));
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
            }],
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
