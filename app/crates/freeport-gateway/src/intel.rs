//! 本机额度情报（Pro）：把「最近请求 + 账本 + 健康度」折成每个
//! `provider×model` 一行能回答问题的数字。
//!
//! v1.12 起情报全部本地计算——不再上报、不再从服务器拉别人的样本。
//! 数据源就是本机已有的三样东西：内存日志环（≤300 条）、四窗口账本、
//! 模型健康状态机。样本量如实下发（`scanned`），不假装它是全量历史。

use std::collections::BTreeMap;

use freeport_core::dto::{GwIntel, GwModelIntel};
use freeport_core::model::{AppData, LogEntry};

/// 每个模型最近的错误只留一句，太长的截断——情报是给人扫一眼的，
/// 不是给 grep 用的。
const MAX_ERR_CHARS: usize = 120;

struct Slot {
    requests: usize,
    errors: usize,
    ms: Vec<u64>,
    last_error: Option<String>,
    last_used_at: Option<String>,
}

impl Slot {
    fn new() -> Self {
        Self {
            requests: 0,
            errors: 0,
            ms: Vec::new(),
            last_error: None,
            last_used_at: None,
        }
    }
}

/// 纯聚合，不碰任何 IO。测试可以直接喂日志。
pub fn collect(logs: &[LogEntry], data: &AppData) -> Vec<GwModelIntel> {
    let mut slots: BTreeMap<(String, String), Slot> = BTreeMap::new();
    for e in logs {
        let (Some(provider), Some(model)) = (e.provider.as_deref(), e.model.as_deref()) else {
            continue;
        };
        let slot = slots
            .entry((provider.to_string(), model.to_string()))
            .or_insert_with(Slot::new);
        slot.requests += 1;
        if !e.ok {
            slot.errors += 1;
        }
        if e.ok && e.ms > 0 {
            slot.ms.push(e.ms);
        }
        if let Some(err) = &e.error {
            let mut s: String = err.chars().take(MAX_ERR_CHARS).collect();
            if err.chars().count() > MAX_ERR_CHARS {
                s.push('…');
            }
            slot.last_error = Some(s);
        }
        slot.last_used_at = Some(e.at.clone());
    }

    let now = freeport_core::cooldown::now_ms();
    slots
        .into_iter()
        .map(|((provider, model), s)| {
            let ms = {
                let mut v = s.ms.clone();
                v.sort_unstable();
                v
            };
            GwModelIntel {
                remaining: data.ledger.tightest(&provider, now).map(|t| t.ratio),
                health: data
                    .model_health
                    .get(&format!("{provider}:{model}"))
                    .map(|h| h.status.as_str().to_string()),
                provider,
                model,
                requests: s.requests,
                errors: s.errors,
                ok_rate: if s.requests > 0 {
                    (s.requests - s.errors) as f64 / s.requests as f64
                } else {
                    0.0
                },
                p50_ms: freeport_core::observe::percentile(&ms, 0.50),
                p95_ms: freeport_core::observe::percentile(&ms, 0.95),
                last_error: s.last_error,
                last_used_at: s.last_used_at,
            }
        })
        .collect()
}

impl crate::server::Gateway {
    /// 情报面板数据。非 Pro 返回空列表 + `pro: false`，
    /// UI 拿它进 paywall——绝不渲染一份「看起来有」的假数据。
    pub async fn intel(&self) -> GwIntel {
        let pro = {
            let d = self.st.data.read().await;
            crate::pro::allow(&d, crate::pro::Feature::Intel)
        };
        if !pro {
            return GwIntel { pro: false, models: Vec::new(), scanned: 0 };
        }
        let logs = self.logs().await;
        let scanned = logs.len();
        let d = self.st.data.read().await;
        GwIntel {
            pro: true,
            models: collect(&logs, &d),
            scanned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_core::model::{LogEntry, ModelHealth, Purpose};

    fn entry(provider: &str, model: &str, ok: bool, ms: u64) -> LogEntry {
        LogEntry {
            id: format!("e{}", ms),
            at: chrono::Utc::now().to_rfc3339(),
            path: "/v1/chat/completions".into(),
            requested_model: model.into(),
            model: Some(model.into()),
            provider: Some(provider.into()),
            account_id: None,
            account_name: None,
            stream: false,
            ok,
            status: if ok { 200 } else { 502 },
            ms,
            usage: None,
            attempts: Vec::new(),
            key_id: None,
            key_name: None,
            sticky: false,
            purpose: Purpose::Chat,
            purpose_source: None,
            error: (!ok).then(|| "boom".to_string()),
        }
    }

    #[test]
    fn groups_by_provider_and_model() {
        let data = AppData::default();
        let logs = vec![
            entry("groq", "llama-3", true, 200),
            entry("groq", "llama-3", true, 400),
            entry("groq", "llama-3", false, 0),
            entry("cerebras", "llama-3", true, 100),
        ];
        let out = collect(&logs, &data);
        assert_eq!(out.len(), 2);
        let groq = out.iter().find(|m| m.provider == "groq").unwrap();
        assert_eq!(groq.requests, 3);
        assert_eq!(groq.errors, 1);
        assert!((groq.ok_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(groq.p50_ms, Some(200)); // 最近秩：2 个样本的 p50 取第 1 个
        assert_eq!(groq.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn health_and_ledger_join_in() {
        let mut data = AppData::default();
        data.model_health.insert(
            "groq:llama-3".into(),
            ModelHealth {
                status: freeport_core::model::HealthStatus::Healthy,
                ..Default::default()
            },
        );
        let logs = vec![entry("groq", "llama-3", true, 100)];
        let out = collect(&logs, &data);
        assert_eq!(out[0].health.as_deref(), Some("healthy"));
        // 没记过账：remaining 是 None，不是 0 —— 0 会伪装成「额度用完」
        assert_eq!(out[0].remaining, None);
    }
}
