//! 面向前端的 DTO。字段名与 v0.1 的 TypeScript 契约逐字一致，
//! 前端从 `window.freeport.*` 换成 `invoke(...)` 时不需要改类型定义。

use serde::{Deserialize, Serialize};

use crate::cooldown::CooldownSnapshot;
use crate::model::{AppData, Health, ModelInfo, Provider};

#[derive(Debug, Clone, Serialize)]
pub struct GwStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub bind: String,
    pub since: Option<String>,
    #[serde(rename = "authToken")]
    pub auth_token: String,
    #[serde(rename = "requireAuth")]
    pub require_auth: bool,
    #[serde(rename = "requests24h")]
    pub requests24h: u64,
    #[serde(rename = "tokensIn")]
    pub tokens_in: u64,
    #[serde(rename = "tokensOut")]
    pub tokens_out: u64,
    pub errors: u64,
    pub cooldowns: Vec<CooldownSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GwProvider {
    pub id: String,
    pub name: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub enabled: bool,
    #[serde(rename = "keyMask")]
    pub key_mask: String,
    #[serde(rename = "hasKey")]
    pub has_key: bool,
    /// 数量，不是列表
    pub models: usize,
    pub health: Health,
}

impl GwProvider {
    pub fn from(p: &Provider, has_key: bool) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            base_url: p.base_url.clone(),
            tier: p.tier.clone(),
            homepage: p.homepage.clone(),
            note: p.note.clone(),
            enabled: p.enabled,
            key_mask: p.key_mask.clone(),
            has_key,
            models: p.models.len(),
            health: p.health.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GwModel {
    pub id: String,
    pub object: &'static str,
    pub owned_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freeport: Option<FreeportMeta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FreeportMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "upstreamModel")]
    pub upstream_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "virtual")]
    pub virtual_model: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `freeport/auto` 这类虚拟模型：不指向任何真实上游。
impl FreeportMeta {
    pub fn virtual_model(note: &str) -> Self {
        Self {
            provider: None,
            upstream_model: None,
            tier: None,
            health: None,
            virtual_model: Some(true),
            note: Some(note.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GwRouting {
    pub strategy: String,
    pub chain: Vec<String>,
    pub cooldowns: Vec<CooldownSnapshot>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddProviderResult {
    pub provider: GwProvider,
    #[serde(rename = "modelCount")]
    pub model_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingPatch {
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub chain: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPatch {
    #[serde(default)]
    pub require_auth: Option<bool>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "latencyMs")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 聚合 `/v1/models` 的输出。
pub fn aggregate_models(data: &AppData) -> Vec<GwModel> {
    let mut out = vec![GwModel {
        id: "freeport/auto".into(),
        object: "model",
        owned_by: "freeport".into(),
        context_length: None,
        created: None,
        freeport: Some(FreeportMeta::virtual_model("按当前路由链自动选择")),
    }];

    for p in data.providers.iter().filter(|p| p.enabled) {
        for m in &p.models {
            out.push(model_row(p, m, true));
            // 同时暴露裸 id，方便直接填 gpt-4o-mini 这类名字
            if !out.iter().any(|x| x.id == m.id) {
                out.push(model_row(p, m, false));
            }
        }
    }
    out
}

fn model_row(p: &Provider, m: &ModelInfo, prefixed: bool) -> GwModel {
    GwModel {
        id: if prefixed {
            format!("{}:{}", p.id, m.id)
        } else {
            m.id.clone()
        },
        object: "model",
        owned_by: p.id.clone(),
        context_length: m.context,
        created: m.created,
        freeport: Some(FreeportMeta {
            provider: Some(p.id.clone()),
            upstream_model: if prefixed { Some(m.id.clone()) } else { None },
            tier: Some(p.tier.clone()),
            health: Some(p.health.status.as_str().to_string()),
            virtual_model: None,
            note: None,
        }),
    }
}
