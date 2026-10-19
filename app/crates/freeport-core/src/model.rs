//! 持久化模型。字段名与 v0.1 的 `freeport.json` 完全一致，
//! 因此 Electron 版留下的配置可以直接被 Rust 版读到。

use serde::{Deserialize, Serialize};

/// 整份配置。落盘为一个 JSON 文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppData {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub stats: Stats,
}

fn default_version() -> u32 {
    1
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            version: default_version(),
            gateway: GatewayConfig::default(),
            providers: Vec::new(),
            routing: RoutingConfig::default(),
            stats: Stats::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default, rename = "authToken")]
    pub auth_token: String,
    #[serde(default, rename = "requireAuth")]
    pub require_auth: bool,
    #[serde(default = "default_budget", rename = "requestBudgetMs")]
    pub request_budget_ms: u64,
}

fn default_port() -> u16 {
    8787
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_budget() -> u64 {
    45_000
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            auth_token: String::new(),
            require_auth: false,
            request_budget_ms: default_budget(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "baseUrl")]
    pub base_url: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, rename = "keyMask")]
    pub key_mask: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, rename = "addedAt")]
    pub added_at: String,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub health: Health,
}

fn default_tier() -> String {
    "C".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, rename = "ownedBy")]
    pub owned_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// 目录标出的免费判定（M2 接目录后填充）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "latencyMs")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "checkedAt")]
    pub checked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "modelCount")]
    pub model_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            status: HealthStatus::Unknown,
            latency_ms: None,
            checked_at: None,
            model_count: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    #[default]
    Unknown,
    Probing,
    Healthy,
    Degraded,
    Error,
}

impl HealthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Probing => "probing",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub chain: Vec<String>,
}

fn default_strategy() -> String {
    "balanced".to_string()
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            strategy: default_strategy(),
            chain: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    #[serde(default)]
    pub requests: u64,
    #[serde(default, rename = "tokensIn")]
    pub tokens_in: u64,
    #[serde(default, rename = "tokensOut")]
    pub tokens_out: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub since: String,
}

impl Stats {
    pub fn bump(&mut self, tokens_in: u64, tokens_out: u64, error: bool) {
        self.requests += 1;
        self.tokens_in += tokens_in;
        self.tokens_out += tokens_out;
        if error {
            self.errors += 1;
        }
    }
}

/// 一次请求的完整轨迹，日志页与 `X-Freeport-Trail` 都用它。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub at: String,
    pub path: String,
    #[serde(rename = "requestedModel")]
    pub requested_model: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub stream: bool,
    pub ok: bool,
    pub status: u16,
    pub ms: u64,
    pub usage: Option<Usage>,
    pub attempts: Vec<Attempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl Usage {
    pub fn tokens_in(&self) -> u64 {
        self.prompt_tokens.unwrap_or(0)
    }
    pub fn tokens_out(&self) -> u64 {
        self.completion_tokens.unwrap_or(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub provider: String,
    pub model: String,
    pub ms: u64,
    pub status: u16,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "cooledMs")]
    pub cooled_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "skipProvider")]
    pub skip_provider: Option<bool>,
}
