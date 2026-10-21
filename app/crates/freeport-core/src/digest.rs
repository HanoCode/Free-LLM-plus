//! 每日摘要（Digest）。
//!
//! 不是报告，是「一行结论 + 三条变更」（§3.5.1）。用户打开窗口只看一眼，
//! 所以要回答的是「今天有没有需要我动手的事」，不是把 24 小时数据铺开。
//!
//! 数据全部来自本地已有的状态，**不额外发一次请求**：
//! - 目录变更 → `feed.events`（每条带 `at`）
//! - 恢复 → `model_health[*].recovered_at`（`apply_ok` 从挂掉/退役爬回来时写）
//! - 探针消耗 → `patrol.used_today / budget_today`
//! - 需要处理 → 当前 health 分布 + feed 被拦下的原因
//!
//! 「恢复」必须靠时间戳：只看 status 的话，昨天挂了今天好了等于没发生过。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{parse_rfc3339_ms, AppData, HealthStatus, LogEntry, Purpose};

/// 统计窗口：24 小时。
pub const WINDOW_MS: i64 = 24 * 3_600_000;
/// 变更列表最多几条 —— 界面只放得下三条，多的折叠。
pub const MAX_CHANGES: usize = 3;

#[derive(Debug, Clone, Serialize)]
pub struct DigestChange {
    /// `added` / `removed` / `other`
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HealthCounts {
    pub healthy: usize,
    pub degraded: usize,
    pub cooling: usize,
    pub unavailable: usize,
    pub retired: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Digest {
    /// 生成时刻（RFC3339）
    pub at: String,
    /// 一句话结论
    pub headline: String,
    pub added: usize,
    pub removed: usize,
    pub recovered: usize,
    #[serde(rename = "probeUsed")]
    pub probe_used: u32,
    #[serde(rename = "probeBudget")]
    pub probe_budget: u32,
    pub health: HealthCounts,
    /// 按用途池分的请求统计。样本是**内存日志环**（最近 300 条），不是 24h 全量——
    /// `logScanned` 就是这次统计实际看了几条，别让读者把比例当成全天总量。
    pub pools: Vec<PoolStat>,
    #[serde(rename = "logScanned")]
    pub log_scanned: usize,
    /// 最多三条，按「影响决策」排序
    pub changes: Vec<DigestChange>,
    /// 需要用户动手的事（额度耗尽、feed 被拦、账号过期）
    pub attention: Vec<String>,
}

/// 一个用途池的近期战绩。
///
/// 解决的问题：用户问「我的 coding 号是不是挂了」，之前只能翻日志一条条数。
#[derive(Debug, Clone, Serialize)]
pub struct PoolStat {
    pub purpose: Purpose,
    pub requests: usize,
    pub errors: usize,
    /// 成功率 0..1。`None` 表示这个池**没有请求**——「没请求」和「全挂」是两件事，
    /// 用 0.0 表示前者会让界面显示成 0% 成功率。
    #[serde(rename = "okRate", skip_serializing_if = "Option::is_none")]
    pub ok_rate: Option<f64>,
}

impl Digest {
    /// 便于界面直接渲染，不必自己拼字符串。
    pub fn budget_line(&self) -> String {
        format!("{}/{}", self.probe_used, self.probe_budget)
    }
}

/// 按窗口聚合。`now_ms` 由调用方给，方便单测。
///
/// 不带日志的版本：只有目录/健康/预算，`pools` 为空。
/// 真实调用点用 [`build_with_logs`] —— 池战绩必须喂进日志，否则会显示成
/// 「今天没有任何请求」，而那是空数组最容易被读成的意思。
pub fn build(data: &AppData, now_ms: i64) -> Digest {
    build_with_logs(data, now_ms, &[])
}

pub fn build_with_logs(data: &AppData, now_ms: i64, logs: &[LogEntry]) -> Digest {
    let since = now_ms - WINDOW_MS;

    let mut added = 0usize;
    let mut removed = 0usize;
    let mut other: Vec<DigestChange> = Vec::new();
    for e in &data.feed.events {
        let at = parse_rfc3339_ms(&e.at);
        if at.map(|t| t < since).unwrap_or(false) {
            continue; // 窗口外
        }
        let title = if e.title.is_empty() {
            e.kind.clone()
        } else {
            e.title.clone()
        };
        match e.kind.as_str() {
            "model.added" | "provider.added" => added += 1,
            "model.removed" => removed += 1,
            "incident" => other.push(DigestChange {
                kind: "other".into(),
                title,
                detail: e.detail.clone(),
            }),
            _ => {}
        }
    }

    let mut health = HealthCounts::default();
    let mut recovered = 0usize;
    for h in data.model_health.values() {
        match h.status {
            HealthStatus::Healthy => health.healthy += 1,
            HealthStatus::Degraded => health.degraded += 1,
            HealthStatus::Cooling => health.cooling += 1,
            HealthStatus::Unavailable => health.unavailable += 1,
            HealthStatus::Retired => health.retired += 1,
            HealthStatus::Unknown | HealthStatus::Probing | HealthStatus::Error => {
                health.unknown += 1
            }
        }
        if h
            .recovered_at
            .as_deref()
            .and_then(parse_rfc3339_ms)
            .map(|t| t >= since)
            .unwrap_or(false)
        {
            recovered += 1;
        }
    }

    // 变更最多三条：新增/下线各占一条，事故各占一条，其余折叠在计数里
    let mut changes = Vec::new();
    if added > 0 {
        changes.push(DigestChange {
            kind: "added".into(),
            title: format!("+{added}"),
            detail: String::new(),
        });
    }
    if removed > 0 {
        changes.push(DigestChange {
            kind: "removed".into(),
            title: format!("-{removed}"),
            detail: String::new(),
        });
    }
    changes.extend(other.into_iter().take(MAX_CHANGES));
    changes.truncate(MAX_CHANGES);

    let mut attention = Vec::new();
    if let Some(halted) = data.feed.halted.as_deref() {
        attention.push(halted.to_string());
    } else if let Some(err) = data.feed.last_error.as_deref() {
        attention.push(format!("目录同步失败：{err}"));
    }
    if health.unavailable > 0 {
        attention.push(format!("{} 个模型不可用", health.unavailable));
    }
    if health.retired > 0 {
        attention.push(format!(
            "{} 个模型已退役（连续 7 天不可用）",
            health.retired
        ));
    }
    let dead_accounts = dead_accounts(data);
    if dead_accounts > 0 {
        attention.push(format!("{dead_accounts} 个凭据全部失效"));
    }

    let (pools, log_scanned) = pool_stats(logs, since);
    for p in &pools {
        // 只在样本够（≥3 单）且失败过半时才打扰：一单失败是常态，不是新闻。
        // 阈值刻意保守——attention 是「要你动手」的列表，狼来了几次就没人看了。
        if p.requests >= 3 && p.errors * 2 >= p.requests {
            attention.push(format!(
                "{}池最近 {} 单挂了 {} 单",
                purpose_zh(p.purpose),
                p.requests,
                p.errors
            ));
        }
    }

    Digest {
        at: chrono::Utc::now().to_rfc3339(),
        headline: headline(added, removed, recovered, &health, &data.patrol),
        added,
        removed,
        recovered,
        probe_used: data.patrol.used_today,
        probe_budget: data.patrol.budget_today,
        health,
        pools,
        log_scanned,
        changes,
        attention,
    }
}

/// 按用途池聚合内存日志。
///
/// 顺序固定为 `Purpose::ALL`（chat / coding / vision / probe），
/// 只输出**有请求**的池：没有请求的池写出来会让人以为「今天没走这条路」，
/// 而实际可能只是日志环已经被新的请求挤掉了。
fn pool_stats(logs: &[LogEntry], since: i64) -> (Vec<PoolStat>, usize) {
    let (req, err, scanned) = count(logs, since);
    let out = Purpose::ALL
        .iter()
        .enumerate()
        .filter(|(i, _)| req[*i] > 0)
        .map(|(i, p)| PoolStat {
            purpose: *p,
            requests: req[i],
            errors: err[i],
            ok_rate: Some(1.0 - err[i] as f64 / req[i] as f64),
        })
        .collect();
    (out, scanned)
}

/// 单个池的近期战绩。`None` = 24 小时内这个池没有请求。
///
/// 给路由预演用：`/v1/route?purpose=coding` 要能顺便回答「这个池最近是不是
/// 一直在挂」，而那句话必须和摘要里的数字是同一个来源——两个地方各算一遍
/// 迟早会出现「摘要说 78%、预演说 100%」这种没法解释的分歧。
pub fn pool_stat(logs: &[LogEntry], purpose: Purpose, now_ms: i64) -> Option<PoolStat> {
    let (req, err, _) = count(logs, now_ms - WINDOW_MS);
    let i = index(purpose);
    if req[i] == 0 {
        return None;
    }
    Some(PoolStat {
        purpose,
        requests: req[i],
        errors: err[i],
        ok_rate: Some(1.0 - err[i] as f64 / req[i] as f64),
    })
}

fn index(p: Purpose) -> usize {
    match p {
        Purpose::Chat => 0,
        Purpose::Coding => 1,
        Purpose::Vision => 2,
        Purpose::Probe => 3,
    }
}

fn count(logs: &[LogEntry], since: i64) -> ([usize; 4], [usize; 4], usize) {
    let mut req = [0usize; 4];
    let mut err = [0usize; 4];
    let mut scanned = 0usize;
    for l in logs {
        // 解析不出来就当作在窗口内：宁可多算一条，也不要静默丢掉真实请求
        if parse_rfc3339_ms(&l.at).map(|t| t < since).unwrap_or(false) {
            continue;
        }
        let i = index(l.purpose);
        req[i] += 1;
        if !l.ok {
            err[i] += 1;
        }
        scanned += 1;
    }
    (req, err, scanned)
}

fn purpose_zh(p: Purpose) -> &'static str {
    match p {
        Purpose::Chat => "对话",
        Purpose::Coding => "编码",
        Purpose::Vision => "视觉",
        Purpose::Probe => "巡检",
    }
}

fn headline(
    added: usize,
    removed: usize,
    recovered: usize,
    h: &HealthCounts,
    patrol: &crate::model::PatrolConfig,
) -> String {
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("新增 {added} 个模型"));
    }
    if removed > 0 {
        parts.push(format!("{removed} 个下线"));
    }
    if recovered > 0 {
        parts.push(format!("{recovered} 个恢复"));
    }
    let tail = if h.unavailable + h.retired == 0 {
        "无异常".to_string()
    } else {
        format!("{} 个不可用", h.unavailable + h.retired)
    };
    if parts.is_empty() {
        return format!(
            "今日无目录变更；探针 {}/{} 次预算，{tail}。",
            patrol.used_today, patrol.budget_today
        );
    }
    format!(
        "今日：{}；探针 {}/{} 次预算，{tail}。",
        parts.join(" · "),
        patrol.used_today,
        patrol.budget_today
    )
}

fn dead_accounts(data: &AppData) -> usize {
    data.providers
        .iter()
        .filter(|p| p.enabled)
        .flat_map(|p| p.live_accounts())
        .filter(|a| {
            a.health
                .as_ref()
                .map(|h| !h.status.is_usable() && h.status != HealthStatus::Cooling)
                .unwrap_or(false)
        })
        .count()
}

/// 按 `provider:model` 分组统计，留给将来「按厂商折叠」的界面用。
pub fn by_provider(data: &AppData) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for (k, h) in &data.model_health {
        if !h.status.is_usable() {
            let provider = k.split(':').next().unwrap_or("").to_string();
            *out.entry(provider).or_insert(0) += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FeedEvent, FeedState, ModelHealth, PatrolConfig};

    fn at(ms_ago: i64, now: i64) -> String {
        chrono::DateTime::from_timestamp_millis(now - ms_ago)
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn counts_only_events_inside_the_window() {
        let now = 1_770_000_000_000i64;
        let mut d = AppData::default();
        d.feed = FeedState {
            events: vec![
                FeedEvent {
                    kind: "model.added".into(),
                    title: "cerebras/qwen3".into(),
                    at: at(3_600_000, now),
                    ..Default::default()
                },
                FeedEvent {
                    kind: "model.removed".into(),
                    title: "openrouter/deepseek-r1:free".into(),
                    at: at(2 * 3_600_000, now),
                    ..Default::default()
                },
                FeedEvent {
                    kind: "model.added".into(),
                    title: "old".into(),
                    at: at(WINDOW_MS + 60_000, now),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let dg = build(&d, now);
        assert_eq!(dg.added, 1, "窗口外的不算");
        assert_eq!(dg.removed, 1);
        assert!(dg.headline.contains("新增 1"));
    }

    #[test]
    fn recovery_needs_a_timestamp_not_just_a_green_status() {
        let now = 1_770_000_000_000i64;
        let mut d = AppData::default();
        d.model_health.insert(
            "a:m".into(),
            ModelHealth {
                status: HealthStatus::Healthy,
                recovered_at: Some(at(60_000, now)),
                ..Default::default()
            },
        );
        // 一直在绿，但没挂过 → 不算恢复
        d.model_health.insert(
            "a:n".into(),
            ModelHealth {
                status: HealthStatus::Healthy,
                recovered_at: None,
                ..Default::default()
            },
        );
        let dg = build(&d, now);
        assert_eq!(dg.recovered, 1);
        assert!(!dg.headline.contains("2 个恢复"));
    }

    #[test]
    fn retired_models_show_up_in_attention() {
        let now = 1_770_000_000_000i64;
        let mut d = AppData::default();
        d.model_health.insert(
            "a:dead".into(),
            ModelHealth {
                status: HealthStatus::Retired,
                ..Default::default()
            },
        );
        d.patrol = PatrolConfig {
            used_today: 137,
            budget_today: 300,
            ..Default::default()
        };
        let dg = build(&d, now);
        assert_eq!(dg.health.retired, 1);
        assert!(dg.attention.iter().any(|s| s.contains("退役")));
        assert_eq!(dg.budget_line(), "137/300");
        assert!(dg.headline.contains("137/300"));
    }

    #[test]
    fn changes_are_capped_at_three() {
        let now = 1_770_000_000_000i64;
        let mut d = AppData::default();
        d.feed.events = (0..5)
            .map(|i| FeedEvent {
                kind: "incident".into(),
                title: format!("incident-{i}"),
                at: at(60_000, now),
                ..Default::default()
            })
            .collect();
        let dg = build(&d, now);
        assert_eq!(dg.changes.len(), MAX_CHANGES);
    }

    fn entry(purpose: Purpose, ok: bool, at: String) -> LogEntry {
        LogEntry {
            id: "r".into(),
            at,
            path: "/v1/chat/completions".into(),
            requested_model: "auto".into(),
            model: None,
            provider: None,
            account_id: None,
            account_name: None,
            stream: false,
            ok,
            status: if ok { 200 } else { 503 },
            ms: 1,
            usage: None,
            attempts: Vec::new(),
            key_id: None,
            key_name: None,
            sticky: false,
            purpose,
            purpose_source: None,
            error: None,
        }
    }

    #[test]
    fn pool_stats_are_per_purpose_and_only_inside_the_window() {
        let now = 1_770_000_000_000i64;
        let d = AppData::default();
        let logs = vec![
            entry(Purpose::Coding, true, at(60_000, now)),
            entry(Purpose::Coding, true, at(120_000, now)),
            entry(Purpose::Coding, false, at(180_000, now)),
            entry(Purpose::Chat, true, at(240_000, now)),
            // 窗口外的旧日志不该计入：它会把「今天的成功率」拉低
            entry(Purpose::Vision, false, at(WINDOW_MS + 60_000, now)),
        ];
        let dg = build_with_logs(&d, now, &logs);

        assert_eq!(dg.log_scanned, 4, "窗口外的条目不算进样本");
        assert_eq!(dg.pools.len(), 2, "没有请求的池不输出，避免被读成 0%");
        // 顺序固定为 ALL：chat 在 coding 前面
        assert_eq!(dg.pools[0].purpose, Purpose::Chat);
        assert_eq!(dg.pools[0].ok_rate, Some(1.0));
        assert_eq!(dg.pools[1].purpose, Purpose::Coding);
        assert_eq!(dg.pools[1].requests, 3);
        assert_eq!(dg.pools[1].errors, 1);
        let rate = dg.pools[1].ok_rate.unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 1e-9, "{rate}");
    }

    #[test]
    fn a_mostly_failing_pool_is_worth_attention_but_two_requests_are_not() {
        let now = 1_770_000_000_000i64;
        let d = AppData::default();
        let logs = vec![
            entry(Purpose::Coding, false, at(60_000, now)),
            entry(Purpose::Coding, false, at(120_000, now)),
            entry(Purpose::Coding, true, at(180_000, now)),
            entry(Purpose::Coding, false, at(240_000, now)),
            // 视觉池只挂了 2 单：样本不够，不该进 attention
            entry(Purpose::Vision, false, at(300_000, now)),
            entry(Purpose::Vision, false, at(360_000, now)),
        ];
        let dg = build_with_logs(&d, now, &logs);
        assert!(
            dg.attention.iter().any(|s| s.contains("编码池最近 4 单挂了 3 单")),
            "{:?}",
            dg.attention
        );
        assert!(
            !dg.attention.iter().any(|s| s.contains("视觉")),
            "两单全挂不该惊动用户：{:?}",
            dg.attention
        );
    }

    #[test]
    fn single_pool_stat_matches_the_digest_numbers() {
        let now = 1_770_000_000_000i64;
        let logs = vec![
            entry(Purpose::Coding, true, at(60_000, now)),
            entry(Purpose::Coding, false, at(120_000, now)),
            entry(Purpose::Chat, true, at(180_000, now)),
            // 窗口外：不该把今天的成功率拉低
            entry(Purpose::Coding, false, at(WINDOW_MS + 60_000, now)),
        ];
        let coding = pool_stat(&logs, Purpose::Coding, now).expect("有请求就该有战绩");
        assert_eq!(coding.requests, 2);
        assert_eq!(coding.errors, 1);

        // 预演和摘要必须是同一个数：两处各算一遍，迟早出现
        // 「摘要说 50%、预演说 100%」这种没法跟用户解释的分歧
        let dg = build_with_logs(&AppData::default(), now, &logs);
        let from_digest = dg
            .pools
            .iter()
            .find(|p| p.purpose == Purpose::Coding)
            .unwrap();
        assert_eq!(from_digest.requests, coding.requests);
        assert_eq!(from_digest.ok_rate, coding.ok_rate);

        // 没有请求的池不编一个 0% 出来
        assert!(pool_stat(&logs, Purpose::Probe, now).is_none());
    }

    #[test]
    fn no_logs_means_no_pools_not_zero_percent() {
        let now = 1_770_000_000_000i64;
        let dg = build_with_logs(&AppData::default(), now, &[]);
        assert!(dg.pools.is_empty());
        assert_eq!(dg.log_scanned, 0);
        // 不带日志的 build() 也一样：宁可空着，也不编一个「各池 0%」出来
        assert!(build(&AppData::default(), now).pools.is_empty());
    }
}
