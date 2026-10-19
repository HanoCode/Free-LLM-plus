//! 候选链构建：Gate → Order → Pick 的第一、二层。
//!
//! M1.5 只落地 Gate（熔断/冷却过滤）与最朴素的 Order（显式 > 链 > tier），
//! Pick 层的 SWRR 与「最早刷新优先」在 M3 填实——但函数边界已经在这里，
//! 补算法不需要动调用方。

use crate::cooldown::{BreakerState, Cooldowns};
use crate::model::{AppData, Provider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 指向 `AppData.providers` 的下标
    pub provider: usize,
    pub model_id: String,
    /// 由半开试探放行；一轮里最多一个
    pub half_open: bool,
}

#[derive(Debug, Clone)]
pub struct Rejected {
    pub provider: String,
    pub model: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    pub rejected: Vec<Rejected>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// 组装本轮候选链。
///
/// `model` 支持三种写法：`auto` / `provider:model` / 裸 `model-id`。
pub fn plan(
    data: &AppData,
    model: &str,
    cooldowns: &Cooldowns,
    has_key: &dyn Fn(&Provider) -> bool,
) -> Plan {
    let mut out: Vec<Candidate> = Vec::new();
    let mut rejected: Vec<Rejected> = Vec::new();

    let usable: Vec<usize> = data
        .providers
        .iter()
        .enumerate()
        .filter(|(_, p)| p.enabled && has_key(p))
        .map(|(i, _)| i)
        .collect();

    let mut push = |idx: usize, model_id: &str| {
        if model_id.is_empty() {
            return;
        }
        if out
            .iter()
            .any(|c| c.provider == idx && c.model_id == model_id)
        {
            return;
        }
        out.push(Candidate {
            provider: idx,
            model_id: model_id.to_string(),
            half_open: false,
        });
    };

    // ① 显式点名
    if let Some((pid, mid)) = model.split_once(':') {
        if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
            push(idx, mid);
        }
    } else if !model.is_empty() && model != "auto" && model != "freeport/auto" {
        // 裸模型名：按 provider 顺序取第一个声明了它的
        if let Some(idx) = usable
            .iter()
            .copied()
            .find(|&i| data.providers[i].models.iter().any(|m| m.id == model))
        {
            push(idx, model);
        }
    }

    // ② 用户配置的链
    for entry in &data.routing.chain {
        if let Some((pid, mid)) = entry.split_once(':') {
            if let Some(idx) = usable.iter().copied().find(|&i| data.providers[i].id == pid) {
                push(idx, mid);
            }
        }
    }

    // ③ 兜底：tier A 优先，每个 provider 取前两个模型
    let mut by_tier = usable.clone();
    by_tier.sort_by(|&a, &b| data.providers[a].tier.cmp(&data.providers[b].tier));
    for idx in by_tier {
        for m in data.providers[idx].models.iter().take(2) {
            push(idx, &m.id);
        }
    }

    // ── Gate：熔断过滤 ──
    let mut closed = Vec::new();
    let mut half = Vec::new();
    for c in out {
        let p = &data.providers[c.provider];
        match cooldowns.state(&p.id, Some(&c.model_id)) {
            BreakerState::Closed => closed.push(c),
            BreakerState::HalfOpen => half.push(Candidate { half_open: true, ..c }),
            BreakerState::Open { until, reason } => rejected.push(Rejected {
                provider: p.id.clone(),
                model: c.model_id.clone(),
                reason: format!("{reason} until {until}"),
            }),
        }
    }

    // 半开只放一个试探，其余的等下一轮
    if let Some(probe) = half.into_iter().next() {
        closed.push(probe);
    }

    Plan {
        candidates: closed,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Health, ModelInfo};

    fn provider(id: &str, tier: &str, models: &[&str]) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: "https://example.com/v1".into(),
            tier: tier.into(),
            homepage: None,
            note: None,
            key_mask: "sk-••••1234".into(),
            enabled: true,
            added_at: String::new(),
            models: models
                .iter()
                .map(|m| ModelInfo {
                    id: (*m).into(),
                    owned_by: id.into(),
                    context: None,
                    created: None,
                    free: None,
                })
                .collect(),
            health: Health::default(),
        }
    }

    fn data() -> AppData {
        AppData {
            providers: vec![provider("groq", "A", &["llama-3", "mixtral"]), provider("cerebras", "A", &["qwen-3"])],
            routing: crate::model::RoutingConfig {
                strategy: "balanced".into(),
                chain: vec!["cerebras:qwen-3".into()],
            },
            ..Default::default()
        }
    }

    #[test]
    fn explicit_model_wins() {
        let d = data();
        let cd = Cooldowns::new();
        let p = plan(&d, "cerebras:qwen-3", &cd, &|_| true);
        assert_eq!(p.candidates[0].model_id, "qwen-3");
        assert_eq!(d.providers[p.candidates[0].provider].id, "cerebras");
    }

    #[test]
    fn chain_comes_before_fallback() {
        let d = data();
        let cd = Cooldowns::new();
        let p = plan(&d, "auto", &cd, &|_| true);
        assert_eq!(p.candidates[0].model_id, "qwen-3");
        assert!(p.candidates.len() >= 3);
    }

    #[test]
    fn open_breaker_removes_candidate() {
        let d = data();
        let cd = Cooldowns::new();
        cd.hit(crate::error::Scope::Key, "groq", None, 60_000, "rate_limit");
        let p = plan(&d, "auto", &cd, &|_| true);
        assert!(p.candidates.iter().all(|c| d.providers[c.provider].id != "groq"));
        assert!(p.rejected.iter().any(|r| r.provider == "groq"));
    }
}
