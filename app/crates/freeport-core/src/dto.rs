//! 面向前端的 DTO。字段名与 v0.1 的 TypeScript 契约逐字一致，
//! 前端从 `window.freeport.*` 换成 `invoke(...)` 时不需要改类型定义。

use serde::{Deserialize, Serialize};

use crate::cooldown::CooldownSnapshot;
use crate::model::{
    Account, AppData, CatalogMeta, FeedEvent, FeedPending, Health, ModelCaps, ModelHealth, ModelInfo,
    Provider, ProviderQuota,
};

#[derive(Debug, Clone, Serialize)]
pub struct GwStatus {
    pub running: bool,
    /// 暂停中：业务请求一律 503，控制面与 /health 正常
    pub paused: bool,
    pub port: Option<u16>,
    pub bind: String,
    pub since: Option<String>,
    #[serde(rename = "authToken")]
    pub auth_token: String,
    #[serde(rename = "requireAuth")]
    pub require_auth: bool,
    #[serde(rename = "requests24h")]
    pub requests24h: u64,
    /// 单次请求的总预算（毫秒），设置页据此显示当前值
    #[serde(rename = "requestBudgetMs")]
    pub request_budget_ms: u64,
    #[serde(rename = "tokensIn")]
    pub tokens_in: u64,
    #[serde(rename = "tokensOut")]
    pub tokens_out: u64,
    pub errors: u64,
    pub cooldowns: Vec<CooldownSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 许可证状态。UI 只读，不参与任何准入判断。
///
/// `tier` 是**生效**档位（过期即 `free`），`licensedTier` 是**买过**的档位。
/// 两个都下发，界面才能说清「你买的是 Pro，已到期 3 天，现在是免费版」。
#[derive(Debug, Clone, Serialize)]
pub struct GwLicense {
    /// missing / valid / grace / expired / invalid / wrong_machine
    pub status: String,
    /// 生效档位
    pub tier: String,
    #[serde(rename = "licensedTier")]
    pub licensed_tier: String,
    /// 便捷位：等价于 `tier == "pro"`
    pub pro: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// 能力清单。失效时是空数组而不是省略——前端少一处 undefined 判断。
    pub features: Vec<String>,
    /// 到期时刻（unix 秒）
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// 宽限期结束时刻（unix 秒）
    #[serde(rename = "graceUntil", skip_serializing_if = "Option::is_none")]
    pub grace_until: Option<i64>,
    /// 剩余天数，过期后为负
    #[serde(rename = "daysLeft", skip_serializing_if = "Option::is_none")]
    pub days_left: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats: Option<u32>,
    /// 本机安装码，激活绑机器时要用
    #[serde(rename = "machineId")]
    pub machine_id: String,
    /// 给用户看的一句话（失败原因 / 宽限期提醒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
    /// 第一条凭据的掩码，给还没升级到凭据视图的地方兜底
    #[serde(rename = "keyMask")]
    pub key_mask: String,
    #[serde(rename = "hasKey")]
    pub has_key: bool,
    /// 这个厂商下的所有凭据。同一厂商可以有多个 key，各算各的额度。
    pub accounts: Vec<GwAccount>,
    /// 数量，不是列表
    pub models: usize,
    pub health: Health,
    /// 厂商级「有免费额度」（来自目录）
    #[serde(rename = "freeTier")]
    pub free_tier: bool,
    #[serde(skip_serializing_if = "Option::is_none", rename = "freeNote")]
    pub free_note: Option<String>,
}

/// 下发给 UI 的凭据视图。**不带明文**：明文只在本机密文库里。
#[derive(Debug, Clone, Serialize)]
pub struct GwAccount {
    pub id: String,
    pub name: String,
    #[serde(rename = "keyMask")]
    pub key_mask: String,
    pub enabled: bool,
    #[serde(rename = "hasKey")]
    pub has_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<ProviderQuota>,
    /// 这条凭据最近一次校验的结果。没测过就不下发，界面上显示「待探测」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
    /// 凭据形态：`copilot` / `oauth`；`None` 表示是普通 API key。
    /// 界面据此显示「已登录为 xxx」而不是一把假 key。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// OAuth token 的到期时间（unix 秒），用来显示「还剩 xx 分钟」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl GwAccount {
    pub fn from(a: &Account, has_key: bool) -> Self {
        Self {
            id: a.id.clone(),
            name: a.name.clone(),
            key_mask: a.key_mask.clone(),
            enabled: a.enabled,
            has_key,
            quota: a.quota,
            health: a.health.clone(),
            auth: None,
            expires_at: None,
        }
    }

    /// 标注这是一条 OAuth 凭据（登录而来，不是手填的 key）。
    pub fn with_auth(mut self, kind: Option<String>, expires_at: Option<i64>) -> Self {
        self.auth = kind;
        self.expires_at = expires_at;
        self
    }
}

impl GwProvider {
    /// `has_secret` 按凭据 id 查密钥库——额度、冷却、密钥都挂在凭据上。
    pub fn from(p: &Provider, has_secret: &dyn Fn(&str) -> bool) -> Self {
        let accounts: Vec<GwAccount> = p
            .accounts
            .iter()
            .map(|a| GwAccount::from(a, has_secret(&a.id)))
            .collect();
        let has_key = accounts.iter().any(|a| a.has_key);
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            base_url: p.base_url.clone(),
            tier: p.tier.clone(),
            homepage: p.homepage.clone(),
            note: p.note.clone(),
            enabled: p.enabled,
            key_mask: p.primary_mask(),
            has_key,
            accounts,
            models: p.models.len(),
            health: p.health.clone(),
            free_tier: false,
            free_note: None,
        }
    }

    /// 带上目录里的免费额度信息（网关查目录后调用）。
    pub fn with_free_tier(mut self, free_tier: bool, free_note: Option<String>) -> Self {
        self.free_tier = free_tier;
        self.free_note = free_note;
        self
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
    /// 模型级零价（价格字段为 0 或目录显式标注）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
    /// 合并后的能力：实测优先，其次目录声明
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<ModelCaps>,
    /// caps 是否来自实测
    #[serde(skip_serializing_if = "Option::is_none", rename = "capsProbed")]
    pub caps_probed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "latencyMs")]
    pub latency_ms: Option<u64>,
    /// feed 新加入、还在 canary 里
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<bool>,
    /// canary 进度（连续成功次数 / 门槛）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canary: Option<u32>,
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
            free: None,
            caps: None,
            caps_probed: None,
            latency_ms: None,
            candidate: None,
            canary: None,
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
    /// 单次请求的总预算（毫秒）。落库后下一次请求立即生效。
    #[serde(default, rename = "requestBudgetMs")]
    pub request_budget_ms: Option<u64>,
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
            // 已被 feed 下架的模型不再对外宣告：Gate 会拒，列出来只会让 Agent
            // 挑一个必然失败的模型。厂商自己的 `/models` 还没删也要挡。
            if data.feed.tombstoned(&p.id, &m.id) {
                continue;
            }
            let health = data.model_health.get(&format!("{}:{}", p.id, m.id));
            out.push(model_row(p, m, true, health));
            // 同时暴露裸 id，方便直接填 gpt-4o-mini 这类名字
            if !out.iter().any(|x| x.id == m.id) {
                out.push(model_row(p, m, false, health));
            }
        }
    }
    out
}

fn model_row(p: &Provider, m: &ModelInfo, prefixed: bool, health: Option<&ModelHealth>) -> GwModel {
    let measured = health.map(|h| h.caps).unwrap_or_default();
    let caps = m.caps.merge_measured(measured);
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
            health: Some(
                health
                    .map(|h| h.status.as_str().to_string())
                    .unwrap_or_else(|| p.health.status.as_str().to_string()),
            ),
            virtual_model: None,
            note: None,
            free: m.free,
            caps: if caps.is_empty() { None } else { Some(caps) },
            caps_probed: Some(!measured.is_empty()),
            latency_ms: health.and_then(|h| h.latency_ms).or(p.health.latency_ms),
            candidate: if m.candidate { Some(true) } else { None },
            canary: if m.candidate {
                Some(health.map(|h| h.oks).unwrap_or(0))
            } else {
                None
            },
        }),
    }
}

/* ── 目录 ─────────────────────────────────────────── */

#[derive(Debug, Clone, Serialize)]
pub struct GwCatalog {
    pub meta: CatalogMeta,
    pub providers: Vec<GwCatalogProvider>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GwCatalogProvider {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    pub tier: String,
    #[serde(rename = "freeTier")]
    pub free_tier: bool,
    #[serde(skip_serializing_if = "Option::is_none", rename = "freeNote")]
    pub free_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<CatalogQuota>,
    pub models: usize,
    #[serde(rename = "freeModels")]
    pub free_models: usize,
    /// 是否已经配置了这一家
    pub added: bool,
    /// `baseline` / `models.dev` / `both`
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CatalogQuota {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u32>,
}

/* ── 探针与巡检 ───────────────────────────────────── */

#[derive(Debug, Clone, Serialize)]
pub struct GwModelProbe {
    pub ok: bool,
    pub status: u16,
    #[serde(rename = "latencyMs")]
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 探完这一轮后的健康状态
    pub health: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caps: Option<ModelCaps>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GwPatrol {
    pub enabled: bool,
    #[serde(rename = "hotHours")]
    pub hot_hours: u32,
    #[serde(rename = "coldHours")]
    pub cold_hours: u32,
    #[serde(rename = "sweepAt")]
    pub sweep_at: String,
    #[serde(rename = "budgetToday")]
    pub budget_today: u32,
    #[serde(rename = "usedToday")]
    pub used_today: u32,
    pub remaining: u32,
    #[serde(rename = "dueNow")]
    pub due_now: u32,
    #[serde(skip_serializing_if = "Option::is_none", rename = "lastRunAt")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "lastSweepAt")]
    pub last_sweep_at: Option<String>,
    #[serde(rename = "probesTotal")]
    pub probes_total: u64,
    #[serde(rename = "probesOk")]
    pub probes_ok: u64,
    /// 当前在途的真实请求数。巡检遇到它就让路。
    pub inflight: u32,
}

/// 虚拟 key。永远不含 hash 与明文。
#[derive(Debug, Clone, Serialize)]
pub struct GwKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "rpmLimit")]
    pub rpm_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "dailyLimit")]
    pub daily_req_limit: Option<u32>,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    /// 今日已用请求数（跨天读作 0）
    #[serde(rename = "usedToday")]
    pub used_today: u32,
}

impl GwKey {
    pub fn from(k: &crate::model::ApiKey) -> Self {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let used = if k.usage.day == today {
            k.usage.day_count
        } else {
            0
        };
        Self {
            id: k.id.clone(),
            name: k.name.clone(),
            prefix: k.prefix.clone(),
            rpm_limit: k.rpm_limit,
            daily_req_limit: k.daily_req_limit,
            enabled: k.enabled,
            created_at: k.created_at.clone(),
            last_used_at: k.last_used_at.clone(),
            used_today: used,
        }
    }
}

/// 新建 key 的入参
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInput {
    pub name: String,
    #[serde(default)]
    pub rpm_limit: Option<u32>,
    #[serde(default)]
    pub daily_limit: Option<u32>,
}

/// 新建结果：明文只在这里出现一次。
#[derive(Debug, Clone, Serialize)]
pub struct KeyCreated {
    pub key: GwKey,
    pub secret: String,
}

/// 单个额度窗口。`limit == 0` 表示没配限额，只记账不拦截。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwWindow {
    pub kind: String,
    pub used_user: f64,
    pub used_probe: f64,
    pub limit: f64,
    /// 归零时刻（ms）
    pub reset_at: i64,
    /// 剩余比例 0~1
    pub remaining: f64,
    pub exhausted: bool,
}

/// 一个厂商的一个凭据的四个窗口 + 一句人话解释。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwQuota {
    pub provider: String,
    pub name: String,
    /// 这条窗口属于哪条凭据。额度按凭据算，所以一个厂商可能出现多行。
    pub account_id: String,
    pub account_name: String,
    pub windows: Vec<GwWindow>,
    /// 任一窗口耗尽时的恢复时刻（ms），用于渲染「预计 14:32 恢复」
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
    /// 剩余最紧的那个窗口的余量比例，Pick 层用它做「快烧完了让位」
    pub headroom: f64,
}

impl GwQuota {
    pub fn from(
        provider: &Provider,
        account: &Account,
        ledger: &crate::quota::Ledger,
        now: i64,
    ) -> Self {
        use crate::quota::WindowKind;
        let mut windows = Vec::new();
        for kind in [WindowKind::Rpm, WindowKind::Rpd, WindowKind::Tpm, WindowKind::Tpd] {
            let w = ledger.window(&account.id, kind);
            windows.push(GwWindow {
                kind: kind.as_str().to_string(),
                used_user: w.map(|w| w.used_user).unwrap_or(0.0),
                used_probe: w.map(|w| w.used_probe).unwrap_or(0.0),
                limit: w.map(|w| w.limit).unwrap_or(0.0),
                reset_at: w.map(|w| w.reset_at).unwrap_or(0),
                remaining: w.map(|w| w.remaining_ratio()).unwrap_or(1.0),
                exhausted: w.map(|w| w.limit > 0.0 && now < w.reset_at && w.exhausted()).unwrap_or(false),
            });
        }
        Self {
            provider: provider.id.clone(),
            name: provider.name.clone(),
            account_id: account.id.clone(),
            account_name: if account.name.is_empty() {
                "default".into()
            } else {
                account.name.clone()
            },
            headroom: ledger.remaining_ratio(&account.id, now),
            reset_at: ledger.next_reset_at(&account.id, now),
            windows,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PatrolReport {
    pub ran: u32,
    pub ok: u32,
    pub failed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatrolPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub hot_hours: Option<u32>,
    #[serde(default)]
    pub cold_hours: Option<u32>,
    #[serde(default)]
    pub sweep_at: Option<String>,
    #[serde(default)]
    pub budget_cap: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

/* ── 凭据（多账号） ─────────────────────────────── */

/// 给某个已有厂商再加一条 key。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInput {
    #[serde(rename = "providerId")]
    pub provider_id: String,
    pub api_key: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub quota: Option<ProviderQuota>,
}

/// 改一条凭据的名字 / 开关 / 限额。`None` 表示不动。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 传 `Some(None)` 没法用 JSON 表达，所以用 `clearQuota` 显式清空
    #[serde(default)]
    pub quota: Option<ProviderQuota>,
    #[serde(default, rename = "clearQuota")]
    pub clear_quota: bool,
}

/* ── 目录 feed ────────────────────────────────────── */

/// feed 的完整状态。界面上只需要这几项就能回答
/// 「接没接」「上次什么时候查的」「为什么没应用」「有什么新东西」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwFeed {
    pub enabled: bool,
    pub url: String,
    pub channel: String,
    /// auto | notify | pinned
    pub policy: String,
    pub configured: bool,
    /// 内置公钥或自填公钥可用
    pub has_pubkey: bool,
    pub check_minutes: u32,
    pub seq: u64,
    pub version: String,
    /// 最近一次应用成功的快照被服务端标注的档位（`free` / `pro`）。
    /// 纯展示用：说明这份目录是服务端按哪个档下发的，客户端不据此拦功能。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 安全阀触发原因（与 lastError 分开：一个要人工确认，一个只是失败）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub halted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<FeedPending>,
    pub events: Vec<FeedEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
    pub rejected: u32,
    pub tombstones: Vec<String>,
}

/// 改 feed 配置。`None` 表示不动。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPatch {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub pubkey: Option<String>,
    #[serde(default)]
    pub check_minutes: Option<u32>,
}

/* ── 登录（设备码） ─────────────────────────────── */

/// 发起登录后给界面的东西：把用户要做的事说清楚，别让人自己拼 URL。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwLoginStart {
    pub login_id: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub interval: u64,
    pub expires_in: u64,
    /// 「在浏览器打开 X 并输入 Y」——界面直接显示这句
    pub instruction: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GwLoginState {
    /// `pending` / `ready` / `denied` / `expired` / `error`
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 登录成功后新建的凭据 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
}

impl GwLoginState {
    pub fn pending() -> Self {
        Self {
            status: "pending".into(),
            message: None,
            account_id: None,
            login: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: "error".into(),
            message: Some(message.into()),
            account_id: None,
            login: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, ModelInfo, Provider};    fn provider_with_models(ids: &[&str]) -> Provider {
        Provider {
            id: "acme".into(),
            name: "Acme".into(),
            base_url: "http://127.0.0.1".into(),
            tier: "A".into(),
            homepage: None,
            note: None,
            key_mask: "fp-****".into(),
            enabled: true,
            added_at: String::new(),
            models: ids
                .iter()
                .map(|id| ModelInfo {
                    id: (*id).to_string(),
                    owned_by: "acme".into(),
                    context: None,
                    created: None,
                    free: None,
                    caps: Default::default(),
                    candidate: false,
                })
                .collect(),
            quota: None,
            accounts: vec![Account {
                id: "acc1".into(),
                name: "main".into(),
                key_mask: "fp-****".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                health: None,
            }],
            health: Default::default(),
        }
    }

    #[test]
    fn tombstoned_model_is_not_advertised() {
        let mut data = AppData::default();
        data.providers = vec![provider_with_models(&["keep", "gone"])];
        data.feed.tombstones.insert("acme:gone".into());

        let ids: Vec<String> = aggregate_models(&data).into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&"acme:keep".to_string()));
        // 带前缀与裸 id 都不应该出现
        assert!(!ids.iter().any(|id| id.ends_with("gone")), "下架模型仍在列表里：{ids:?}");
    }

    #[test]
    fn tombstone_is_scoped_to_provider() {
        let mut data = AppData::default();
        data.providers = vec![provider_with_models(&["gone"])];
        // 同名模型在别家还活着，不能被误伤
        data.feed.tombstones.insert("other:gone".into());

        let ids: Vec<String> = aggregate_models(&data).into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&"acme:gone".to_string()));
    }
}
