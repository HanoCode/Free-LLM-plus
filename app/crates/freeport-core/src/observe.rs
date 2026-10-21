//! 观测：把内存日志环折成一组能回答问题的数字。
//!
//! 「免费额度」产品最终要回答的三件事：
//! 1. 现在快不快（p50/p95，而不是平均——平均值会被一条 40 秒的慢请求带偏）；
//! 2. 花了多少（token 与按目录价折算的美元）；
//! 3. 谁在烧、烧在哪（按厂商、按用途拆）。
//!
//! 这里只做纯计算，不碰文件与网络；采样来自 `LogEntry`（最多 300 条）。

use crate::model::{LogEntry, Usage};

/// 一条请求的耗时样本。失败的也收：它同样占用了用户的时间。
pub struct Summary {
    pub requests: usize,
    pub ok: usize,
    pub errors: usize,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// 按目录价折算的美元；没有价格信息的请求不计入
    pub cost_usd: f64,
    /// 没有价格信息、无法折算的请求数
    pub cost_unknown: usize,
}

/// 分位数用「最近秩」：样本少时不会编出一个不存在的值。
///
/// `p=0.95`、10 个样本 → 第 10 个（下标 9）。
pub fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 1.0);
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    Some(sorted[idx.saturating_sub(1).min(sorted.len() - 1)])
}

/// 按 `cost_in` / `cost_out`（美元 / 百万 token）折算一次请求。
pub fn cost_of(usage: Option<&Usage>, cost_in: Option<f64>, cost_out: Option<f64>) -> Option<f64> {
    let u = usage?;
    let (cin, cout) = (cost_in?, cost_out?);
    let in_tok = u.tokens_in() as f64;
    let out_tok = u.tokens_out() as f64;
    if in_tok == 0.0 && out_tok == 0.0 {
        return Some(0.0);
    }
    Some((in_tok * cin + out_tok * cout) / 1_000_000.0)
}

/// 只统计 `at >= since_ms` 的条目。时间戳解析不了的一律算「新鲜」——
/// 宁可多算一条，也不要在时钟格式变过一次之后整块统计消失。
pub fn summarize<F>(logs: &[LogEntry], since_ms: i64, price: F) -> Summary
where
    F: Fn(&str, &str) -> Option<(Option<f64>, Option<f64>)>,
{
    let mut ms: Vec<u64> = Vec::new();
    let mut s = Summary {
        requests: 0,
        ok: 0,
        errors: 0,
        p50_ms: None,
        p95_ms: None,
        max_ms: None,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
        cost_unknown: 0,
    };

    for e in logs {
        if crate::model::parse_rfc3339_ms(&e.at).map(|t| t < since_ms).unwrap_or(false) {
            continue;
        }
        s.requests += 1;
        if e.ok {
            s.ok += 1;
        } else {
            s.errors += 1;
        }
        // 失败请求的耗时没有可比性（常常是 90 秒超时），不进分位
        if e.ok && e.ms > 0 {
            ms.push(e.ms);
        }
        if let Some(u) = &e.usage {
            s.tokens_in += u.tokens_in();
            s.tokens_out += u.tokens_out();
        }
        match (e.provider.as_deref(), e.model.as_deref()) {
            (Some(p), Some(m)) => {
                let (ci, co) = price(p, m).map(|(a, b)| (a, b)).unwrap_or((None, None));
                match cost_of(e.usage.as_ref(), ci, co) {
                    Some(c) => s.cost_usd += c,
                    None => s.cost_unknown += 1,
                }
            }
            _ => {
                if e.usage.is_some() {
                    s.cost_unknown += 1;
                }
            }
        }
    }

    ms.sort_unstable();
    s.p50_ms = percentile(&ms, 0.50);
    s.p95_ms = percentile(&ms, 0.95);
    s.max_ms = ms.last().copied();
    s
}

/// 按某维度拆开的计数与均值，用于「谁在烧」。
pub fn bucket<'a, F>(logs: &'a [LogEntry], since_ms: i64, key: F) -> Vec<(String, usize, u64)>
where
    F: Fn(&'a LogEntry) -> Option<&'a str>,
{
    let mut map: std::collections::BTreeMap<String, (usize, u64, usize)> =
        std::collections::BTreeMap::new();
    for e in logs {
        if crate::model::parse_rfc3339_ms(&e.at).map(|t| t < since_ms).unwrap_or(false) {
            continue;
        }
        let Some(k) = key(e) else { continue };
        let slot = map.entry(k.to_string()).or_insert((0, 0, 0));
        slot.0 += 1;
        if e.ok {
            slot.1 += e.ms;
            slot.2 += 1;
        }
    }
    let mut out: Vec<(String, usize, u64)> = map
        .into_iter()
        .map(|(k, (n, sum, okn))| (k, n, if okn > 0 { sum / okn as u64 } else { 0 }))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.truncate(12);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Purpose, Usage};

    fn entry(at: &str, ok: bool, ms: u64, tok: Option<(u64, u64)>) -> LogEntry {
        LogEntry {
            id: "x".into(),
            at: at.into(),
            path: "/v1/chat/completions".into(),
            requested_model: "auto".into(),
            model: Some("m1".into()),
            provider: Some("groq".into()),
            account_id: None,
            account_name: None,
            stream: false,
            ok,
            status: if ok { 200 } else { 429 },
            ms,
            usage: tok.map(|(i, o)| Usage {
                prompt_tokens: Some(i),
                completion_tokens: Some(o),
                total_tokens: Some(i + o),
            }),
            attempts: Vec::new(),
            key_id: None,
            key_name: None,
            sticky: false,
            purpose: Purpose::Chat,
            purpose_source: None,
            error: None,
        }
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<u64> = (1..=10).collect();
        assert_eq!(percentile(&v, 0.5), Some(5));
        assert_eq!(percentile(&v, 0.95), Some(10));
        assert_eq!(percentile(&[], 0.5), None);
        // 一个样本时 p50 与 p95 都是它，不能除以零
        assert_eq!(percentile(&[42], 0.95), Some(42));
    }

    #[test]
    fn failures_stay_out_of_latency_but_count_in_requests() {
        let now = crate::model::parse_rfc3339_ms("2026-10-21T03:00:00Z").unwrap();
        let logs = vec![
            entry("2026-10-21T03:01:00Z", true, 800, Some((100, 50))),
            entry("2026-10-21T03:02:00Z", true, 1200, Some((200, 100))),
            // 90 秒的超时不该把 p95 拉到 90s
            entry("2026-10-21T03:03:00Z", false, 90_000, None),
        ];
        let s = summarize(&logs, now - 3_600_000, |_, _| Some((Some(1.0), Some(2.0))));
        assert_eq!(s.requests, 3);
        assert_eq!(s.ok, 2);
        assert_eq!(s.errors, 1);
        assert_eq!(s.p95_ms, Some(1200));
        assert_eq!(s.tokens_in, 300);
        assert_eq!(s.tokens_out, 150);
        // (300*1 + 150*2) / 1e6
        assert!((s.cost_usd - 0.0006).abs() < 1e-9, "实际 {}", s.cost_usd);
    }

    #[test]
    fn entries_older_than_the_window_are_ignored() {
        let now = crate::model::parse_rfc3339_ms("2026-10-21T03:00:00Z").unwrap();
        let logs = vec![
            entry("2026-10-20T03:00:00Z", true, 500, None),
            entry("2026-10-21T02:59:00Z", true, 700, None),
        ];
        let s = summarize(&logs, now - 3_600_000, |_, _| None);
        assert_eq!(s.requests, 1);
        assert_eq!(s.p50_ms, Some(700));
        // 没有价格信息时要诚实地说「折不出来」，不能当成 0 美元
        assert_eq!(s.cost_unknown, 1);
    }

    #[test]
    fn buckets_rank_by_volume_and_skip_failures_in_the_average() {
        let now = crate::model::parse_rfc3339_ms("2026-10-21T03:00:00Z").unwrap();
        let mut a = entry("2026-10-21T03:01:00Z", true, 100, None);
        a.provider = Some("groq".into());
        let mut b = entry("2026-10-21T03:02:00Z", false, 90_000, None);
        b.provider = Some("groq".into());
        let mut c = entry("2026-10-21T03:03:00Z", true, 300, None);
        c.provider = Some("cerebras".into());
        let out = bucket(&[a, b, c], now - 3_600_000, |e| e.provider.as_deref());
        assert_eq!(out[0].0, "groq");
        assert_eq!(out[0].1, 2);
        assert_eq!(out[0].2, 100, "均值只算成功的那条");
    }
}
