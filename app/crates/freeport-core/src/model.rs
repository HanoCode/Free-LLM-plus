//! 持久化模型。字段名与 v0.1 的 `freeport.json` 完全一致，
//! 因此 Electron 版留下的配置可以直接被 Rust 版读到。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::quota::Ledger;

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
    /// 目录同步状态（只存摘要，目录本体另存 catalog.json）
    #[serde(default)]
    pub catalog: CatalogMeta,
    /// 模型级健康，key 为 `provider:model`
    #[serde(default, rename = "modelHealth")]
    pub model_health: BTreeMap<String, ModelHealth>,
    /// 巡检配置与当日预算账本
    #[serde(default)]
    pub patrol: PatrolConfig,
    /// 额度台账（四窗口）。只记账，Gate 只在配了限额时生效。
    #[serde(default)]
    pub ledger: Ledger,
    /// 虚拟 key。没有它，「中心」只是一台机器——多 agent 会互相抢额度且无法归因。
    #[serde(default, rename = "apiKeys")]
    pub api_keys: Vec<ApiKey>,
    /// 签名目录 feed：版本、策略、变更事件、墓碑。
    #[serde(default)]
    pub feed: FeedState,
    /// 本机安装码。许可证可以绑它，也可以不绑（离线激活默认不绑）。
    /// 只用于「这份 license 是不是本机的」，不参与任何路由判断。
    #[serde(default, rename = "installId")]
    pub install_id: String,
    /// Pro 许可证。token 本身是签名 JWT，不是秘密，所以与密钥库分开存。
    #[serde(default)]
    pub license: LicenseCfg,
    /// 墓碑：用户主动删掉的厂商/凭据。同步与预设回填看到它就跳过，
    /// 否则「删掉又被加回来」会无限循环。
    #[serde(default)]
    pub tombstones: Tombstones,
}

/// 删除记录。存时间是为了合并时能判断「谁删得更晚」——
/// 只存 id 的话，两台机器各删各的，合起来就成了「都还在」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tombstones {
    /// 厂商 id → 删除时间（RFC3339）
    #[serde(default)]
    pub providers: BTreeMap<String, String>,
    /// 凭据 id → 删除时间
    #[serde(default)]
    pub accounts: BTreeMap<String, String>,
}

impl Tombstones {
    pub fn bury_provider(&mut self, id: &str) {
        self.providers.insert(id.to_string(), now_rfc());
    }

    pub fn bury_account(&mut self, id: &str) {
        self.accounts.insert(id.to_string(), now_rfc());
    }

    /// 显式加回来时清掉墓碑。返回 true 表示确实删过。
    pub fn revive_provider(&mut self, id: &str) -> bool {
        self.providers.remove(id).is_some()
    }

    pub fn revive_account(&mut self, id: &str) -> bool {
        self.accounts.remove(id).is_some()
    }

    pub fn is_buried(&self, provider_id: &str) -> bool {
        self.providers.contains_key(provider_id)
    }
}

fn now_rfc() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 许可证配置。只存 token 原文——状态每次现算，不缓存。
///
/// 缓存状态会带来一个阴间 bug：用户把系统时间调回过去，缓存里那句
/// 「剩余 30 天」还能再撑一个月。现算成本是一次 Ed25519 验签（约 50µs），
/// 不值得为它冒这个险。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LicenseCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl LicenseCfg {
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }
}

/// 下发给客户端的 key。明文只在创建时出现一次，之后只留 sha256。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    /// sha256(明文) 的小写 hex
    #[serde(rename = "keyHash")]
    pub key_hash: String,
    /// 前 8 位明文，UI 用来辨认
    pub prefix: String,
    /// 每分钟请求上限，None = 不限
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "rpmLimit")]
    pub rpm_limit: Option<u32>,
    /// 每日请求上限，None = 不限
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "dailyLimit")]
    pub daily_req_limit: Option<u32>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub usage: KeyUsage,
}

/// key 自己的计数器。与额度台账同思路：读时惰性归零，不建定时任务。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyUsage {
    /// 本地日期 `2026-10-19`
    #[serde(default)]
    pub day: String,
    #[serde(default, rename = "dayCount")]
    pub day_count: u32,
    /// unix 分钟
    #[serde(default)]
    pub minute: i64,
    #[serde(default, rename = "minuteCount")]
    pub minute_count: u32,
}

/// 配置结构版本。
///
/// 加字段**不**升版本（`serde(default)` 够用）；只有语义变化才升——
/// v2 引入墓碑表，旧文件里不存在这个结构，需要一个明确的迁移点。
pub const SCHEMA_VERSION: u32 = 2;

fn default_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            version: default_version(),
            gateway: GatewayConfig::default(),
            providers: Vec::new(),
            routing: RoutingConfig::default(),
            stats: Stats::default(),
            catalog: CatalogMeta::default(),
            model_health: BTreeMap::new(),
            patrol: PatrolConfig::default(),
            ledger: Ledger::default(),
            api_keys: Vec::new(),
            feed: FeedState::default(),
            install_id: String::new(),
            license: LicenseCfg::default(),
            tombstones: Tombstones::default(),
        }
    }
}

impl AppData {
    /// 加法式迁移：只补骨架与版本号，**从不覆盖用户值**。返回「是否需要写回」。
    ///
    /// 为什么不能「解析失败就重置」或「字段缺了就填默认」：这份文件里是用户全部的
    /// 厂商、凭据掩码与路由偏好。一次升级清空，等于让他把活重做一遍。
    /// 所以这里只做两件事：把没有意义的值修成默认，以及修掉墓碑与实体互相矛盾的状态。
    pub fn migrate(&mut self) -> bool {
        let mut changed = false;
        if self.version < SCHEMA_VERSION {
            // v1 → v2：墓碑是新表，`serde` 已经补成空；这里只修「有值但没意义」的旧字段
            if self.routing.fallback_horizon_ms == 0 {
                self.routing.fallback_horizon_ms = 30_000;
            }
            self.version = SCHEMA_VERSION;
            changed = true;
        }

        // 实体在、墓碑也在 = 「删了又加回来」但标记没清。以实体为准：
        // 留着墓碑会让下一次同步把用户刚加回来的东西再删一遍。
        let AppData { providers, tombstones, .. } = self;
        let live_p: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
        let live_a: Vec<&str> = providers
            .iter()
            .flat_map(|p| p.accounts.iter().map(|a| a.id.as_str()))
            .collect();
        let (np, na) = (tombstones.providers.len(), tombstones.accounts.len());
        tombstones.providers.retain(|id, _| !live_p.contains(&id.as_str()));
        tombstones.accounts.retain(|id, _| !live_a.contains(&id.as_str()));
        changed |= np != tombstones.providers.len() || na != tombstones.accounts.len();

        changed
    }

    /// 按凭据 id 反查厂商。凭据 id 全局唯一，所以路由/账本/日志都可以只存 id。
    pub fn find_account(&self, account_id: &str) -> Option<(usize, &Provider, &Account)> {
        self.providers
            .iter()
            .enumerate()
            .find_map(|(i, p)| p.account(account_id).map(|a| (i, p, a)))
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
    /// 首个有效语义块的等待上限。超时即判定这一跳失败，换下一家。
    #[serde(default = "default_hold_idle", rename = "holdIdleMs")]
    pub hold_idle_ms: u64,
    /// Hold 的总预算上限，兜底防止某些上游持续发空增量。
    #[serde(default = "default_budget", rename = "holdTotalMs")]
    pub hold_total_ms: u64,
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
fn default_hold_idle() -> u64 {
    8_000
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            auth_token: String::new(),
            require_auth: false,
            request_budget_ms: default_budget(),
            hold_idle_ms: default_hold_idle(),
            hold_total_ms: default_budget(),
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
    /// 免费额度上限。**v1.3 起限额挂在凭据上**，这里只作为旧配置的迁移来源：
    /// 加载时会被搬进第一个凭据，之后为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<ProviderQuota>,
    /// 这个厂商下的凭据。同一厂商可以有多个 key，各自算额度、各自冷却。
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub health: Health,
}

/// 一条凭据（一个 API Key）。
///
/// 为什么额度按凭据算而不是按厂商算：同一厂商的两个 key 在上游是两本独立的账，
/// 把它们的用量加在一起记，会得出「还有余量」的假象然后被 429 打回来。
///
/// 老配置迁移时 `id` 直接沿用 provider id，密钥库里那条记录不用搬家；
/// 新加的凭据用 `acct_xxx`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "keyMask")]
    pub key_mask: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, rename = "addedAt")]
    pub added_at: String,
    /// 这条凭据自己的四窗口上限。没填就只记账、不拦截。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<ProviderQuota>,
    /// 这条凭据最近一次校验的结果。
    ///
    /// 为什么健康度也要按凭据存：一条 key 失效，另一条还好着，是常态。
    /// 如果失败只写厂商级 `Provider::health`，界面会把整家刷红，
    /// 而且「校验第二把 key」这个动作会看起来像是把厂商搞挂了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
}

impl Account {
    pub fn new(name: &str, key_mask: &str) -> Self {
        Self {
            id: format!("acct_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            name: name.to_string(),
            key_mask: key_mask.to_string(),
            enabled: true,
            added_at: chrono::Utc::now().to_rfc3339(),
            quota: None,
            health: None,
        }
    }
}

impl Provider {
    /// 参与路由的凭据。厂商关掉时整家不出现在链里，这里只管凭据自己。
    pub fn live_accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.iter().filter(|a| a.enabled)
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn account_mut(&mut self, id: &str) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.id == id)
    }

    /// 展示用：第一条凭据的掩码，给还没升级到凭据视图的老组件兜底。
    pub fn primary_mask(&self) -> String {
        self.accounts
            .first()
            .map(|a| a.key_mask.clone())
            .unwrap_or_else(|| self.key_mask.clone())
    }

    /// 把逐条凭据的健康度汇总成厂商级的一行字。
    ///
    /// 规则只有一条：**只要还有一条 key 是好的，这家就是好的**。校验第二把
    /// key 失败不该让第一把也变成红的。全部失败时才跟最晚那条的错误。
    /// 一条凭据都没校验过就保持原样——「没测过」和「测挂了」不是一回事。
    pub fn sync_health(&mut self) {
        // 先算成一个独立的值再写回去：`live_accounts()` 借着 `self`，
        // 中途写 `self.health` 会被借用检查挡住。
        let next = {
            let seen: Vec<&Health> = self
                .live_accounts()
                .filter_map(|a| a.health.as_ref())
                .collect();
            if seen.is_empty() {
                None
            } else if let Some(ok) = seen
                .iter()
                .filter(|h| matches!(h.status, HealthStatus::Healthy))
                .max_by_key(|h| h.checked_at.clone().unwrap_or_default())
            {
                Some(Health {
                    status: HealthStatus::Healthy,
                    latency_ms: ok.latency_ms,
                    checked_at: ok.checked_at.clone(),
                    model_count: ok.model_count,
                    error: None,
                })
            } else {
                seen.iter()
                    .max_by_key(|h| h.checked_at.clone().unwrap_or_default())
                    .map(|bad| Health {
                        status: HealthStatus::Error,
                        latency_ms: bad.latency_ms,
                        checked_at: bad.checked_at.clone(),
                        model_count: None,
                        error: bad.error.clone(),
                    })
            }
        };
        if let Some(h) = next {
            self.health = h;
        }
    }
}

/// 四窗口额度上限。免费厂商的约束分布差异很大，所以每一项都可缺省。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProviderQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpd: Option<u64>,
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
    /// 目录标出的免费判定（价格字段为 0，或目录显式标注）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
    /// 目录声明的能力。实测结果在 `ModelHealth.caps`，优先级高于这里。
    #[serde(default, skip_serializing_if = "ModelCaps::is_empty")]
    pub caps: ModelCaps,
    /// 由签名 feed 新加入、还没通过 canary 的模型：0.3 倍权重，连续
    /// [`CANARY_OKS`] 次成功才转正。
    #[serde(default, skip_serializing_if = "is_false")]
    pub candidate: bool,
}

/// 能力标记。`None` 表示「没测过/不知道」，与 `Some(false)`（测过且不支持）区分开。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "json")]
    pub json_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

impl ModelCaps {
    pub fn is_empty(&self) -> bool {
        self.tools.is_none()
            && self.vision.is_none()
            && self.json_mode.is_none()
            && self.reasoning.is_none()
    }

    /// 用实测值覆盖声明值里已知的部分。实测永远优先。
    pub fn merge_measured(self, measured: ModelCaps) -> ModelCaps {
        ModelCaps {
            tools: measured.tools.or(self.tools),
            vision: measured.vision.or(self.vision),
            json_mode: measured.json_mode.or(self.json_mode),
            reasoning: measured.reasoning.or(self.reasoning),
        }
    }

    pub fn any_true(&self) -> bool {
        self.tools == Some(true)
            || self.vision == Some(true)
            || self.json_mode == Some(true)
            || self.reasoning == Some(true)
    }
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
    Cooling,
    Unavailable,
    Error,
}

impl HealthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Probing => "probing",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Cooling => "cooling",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
        }
    }

    /// 路由是否还愿意尝试。`Unavailable` 与 `Cooling` 不放行；
    /// `Degraded` 放行（它只是慢，不是坏）。
    pub fn is_usable(&self) -> bool {
        matches!(
            self,
            Self::Unknown | Self::Probing | Self::Healthy | Self::Degraded
        )
    }
}

/// 单个模型的健康记录。与 provider 级 `Health` 分开：
/// 一个 provider 挂掉不代表它的每个模型都挂，反之亦然。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelHealth {
    #[serde(default)]
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "latencyMs")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "checkedAt")]
    pub checked_at: Option<String>,
    /// 连续失败次数。连续 2 次才判 unavailable，单次失败只降级。
    #[serde(default)]
    pub fails: u32,
    /// 连续成功次数
    #[serde(default)]
    pub oks: u32,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastOkAt")]
    pub last_ok_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 实测能力，优先于目录声明
    #[serde(default, skip_serializing_if = "ModelCaps::is_empty")]
    pub caps: ModelCaps,
}

impl ModelHealth {
    pub fn is_usable(&self) -> bool {
        self.status.is_usable()
    }

    /// 最近一次成功距今多少毫秒；从未成功返回 None。
    pub fn since_ok_ms(&self, now_ms: i64) -> Option<i64> {
        self.last_ok_at.as_deref().and_then(parse_rfc3339_ms).map(|t| now_ms - t)
    }
}

pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// 巡检调度配置 + 当日账本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatrolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 有真实流量的模型多久复检一次
    #[serde(default = "default_hot_hours", rename = "hotHours")]
    pub hot_hours: u32,
    /// 冷模型多久复检一次
    #[serde(default = "default_cold_hours", rename = "coldHours")]
    pub cold_hours: u32,
    /// 每日全量体检时间（本地时区，HH:MM）
    #[serde(default = "default_sweep_at", rename = "sweepAt")]
    pub sweep_at: String,
    /// 手动压低预算上限；None 表示按 日配额×1% 自动算
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "budgetCap")]
    pub budget_cap: Option<u32>,

    #[serde(default)]
    pub day: String,
    #[serde(default, rename = "usedToday")]
    pub used_today: u32,
    #[serde(default, rename = "budgetToday")]
    pub budget_today: u32,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastSweepAt")]
    pub last_sweep_at: Option<String>,
    /// 本地日期 `YYYY-MM-DD`，用来保证每日全量体检只跑一次
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastSweepDay")]
    pub last_sweep_day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastRunAt")]
    pub last_run_at: Option<String>,
    #[serde(default, rename = "probesTotal")]
    pub probes_total: u64,
    #[serde(default, rename = "probesOk")]
    pub probes_ok: u64,
}

fn default_hot_hours() -> u32 {
    6
}
fn default_cold_hours() -> u32 {
    24
}
fn default_sweep_at() -> String {
    "03:00".to_string()
}

impl Default for PatrolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hot_hours: default_hot_hours(),
            cold_hours: default_cold_hours(),
            sweep_at: default_sweep_at(),
            budget_cap: None,
            day: String::new(),
            used_today: 0,
            budget_today: 0,
            last_sweep_at: None,
            last_sweep_day: None,
            last_run_at: None,
            probes_total: 0,
            probes_ok: 0,
        }
    }
}

/// 目录同步摘要。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogMeta {
    /// `baseline` / `models.dev`
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "fetchedAt")]
    pub fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "builtAt")]
    pub built_at: Option<String>,
    #[serde(default)]
    pub providers: usize,
    #[serde(default)]
    pub models: usize,
    #[serde(default, rename = "freeModels")]
    pub free_models: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub chain: Vec<String>,
    /// 全部候选都被冷却挡掉时，是否把「马上就要到期」的那一条提前放出去试一次。
    ///
    /// 默认开：冷却只是暂时状态，直接回 503 等于把它升级成硬中断。
    #[serde(default = "yes")]
    pub fallback_when_all_cooling: bool,
    /// 回退窗口（毫秒）。**只回退快到期的**——日额度耗尽那种几小时后的
    /// reset 撞了也是白撞，如实告诉用户还要等多久更有用。
    #[serde(default = "default_fallback_horizon")]
    pub fallback_horizon_ms: i64,
}

fn yes() -> bool {
    true
}

fn default_fallback_horizon() -> i64 {
    30_000
}

fn default_strategy() -> String {
    "balanced".to_string()
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            strategy: default_strategy(),
            chain: Vec::new(),
            fallback_when_all_cooling: true,
            fallback_horizon_ms: default_fallback_horizon(),
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
    /// 最终答它的凭据 id
    #[serde(rename = "accountId", default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(rename = "accountName", default, skip_serializing_if = "Option::is_none")]
    pub account_name: Option<String>,
    pub stream: bool,
    pub ok: bool,
    pub status: u16,
    pub ms: u64,
    pub usage: Option<Usage>,
    pub attempts: Vec<Attempt>,
    /// 这一次是谁发起的：虚拟 key 的 id 与当次的名字快照。
    ///
    /// 名字随日志一起存，而不是展示时去查当前 key 列表——日志是历史事实，
    /// key 删掉之后这条记录仍然应该说得清「是哪个 agent 打的」。
    #[serde(rename = "keyId", default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(rename = "keyName", default, skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    /// 这一次是不是因为「这段对话上次就是它答的」而插了队。
    /// 只在粘性命中时为 true——用来解释「为什么明摆着更快的没用上」。
    #[serde(default, skip_serializing_if = "is_false")]
    pub sticky: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
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
    /// 这一跳用的是哪条凭据（多账号时才知道是哪个 key 挂的）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
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

/* ── 签名目录 feed ───────────────────────────────── */

/// canary 转正门槛：新模型连续成功这么多次，才从 0.3 倍权重恢复满权重。
///
/// 20 次的取法：免费档 RPM 常在 10–30 之间，20 次不到一分钟的真实流量就能拿到，
/// 但足够覆盖「第一次能回、第二次就 500」的那类假可用模型。
pub const CANARY_OKS: u32 = 20;

/// 目录 feed 的本地状态。
///
/// 只存**状态与事件**，不存目录本体——目录在 `catalog.json` 里，
/// 两边分开可以让一次 feed 失败不影响已有目录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedState {
    #[serde(default)]
    pub enabled: bool,
    /// latest.json 的地址。空 = 未配置，界面显示「未接 feed」而不是一直转圈。
    #[serde(default)]
    pub url: String,
    /// `stable` / `beta`
    #[serde(default = "default_channel")]
    pub channel: String,
    /// `auto` / `notify` / `pinned`
    #[serde(default = "default_policy")]
    pub policy: String,
    /// 自建 feed 的公钥（hex）。空则用编译期内置的那把。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    /// 轮询间隔（分钟）。Free 档默认 15，Pro 会下发更小的值。
    #[serde(default = "default_check_minutes", rename = "checkMinutes")]
    pub check_minutes: u32,
    /// 已应用的最新 seq。小于它的 manifest 一律丢弃。
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub version: String,
    /// 最近一次应用的快照被服务端标注的档位。只记录「服务端说了什么」，
    /// 不参与任何本地判断——客户端不因为自己是免费档就拒收某个快照。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "servedTier")]
    pub served_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastCheckAt")]
    pub last_check_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastOkAt")]
    pub last_ok_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastError")]
    pub last_error: Option<String>,
    /// 安全阀触发后写这里：说明**为什么**停下来了，不是静默失败。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halted: Option<String>,
    /// `notify` 策略下有新版本待用户确认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<FeedPending>,
    /// 最近变更事件，最新的在前，最多 50 条。
    #[serde(default)]
    pub events: Vec<FeedEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "appliedAt")]
    pub applied_at: Option<String>,
    /// 被验签/安全阀/回滚保护拦下的次数。
    #[serde(default)]
    pub rejected: u32,
    /// 已下架的 `provider:model`。路由在这里直接拒绝，不等目录重新同步。
    #[serde(default)]
    pub tombstones: BTreeSet<String>,
}

impl Default for FeedState {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            channel: default_channel(),
            policy: default_policy(),
            pubkey: None,
            check_minutes: default_check_minutes(),
            seq: 0,
            version: String::new(),
            served_tier: None,
            last_check_at: None,
            last_ok_at: None,
            last_error: None,
            halted: None,
            pending: None,
            events: Vec::new(),
            applied_at: None,
            rejected: 0,
            tombstones: BTreeSet::new(),
        }
    }
}

impl FeedState {
    /// 配了地址才算「接了 feed」。
    pub fn configured(&self) -> bool {
        self.enabled && !self.url.trim().is_empty()
    }

    pub fn accepts_feed(&self) -> bool {
        self.configured() && self.policy != "pinned"
    }

    /// 到点该拉了吗。`now_ms` 用毫秒时间戳，`last_check_at` 是 RFC3339。
    pub fn due(&self, now_ms: i64) -> bool {
        if !self.accepts_feed() {
            return false;
        }
        let Some(last) = self.last_check_at.as_deref() else {
            return true;
        };
        match parse_rfc3339_ms(last) {
            Some(t) => now_ms - t >= (self.check_minutes.max(1) as i64) * 60_000,
            None => true,
        }
    }

    pub fn tombstoned(&self, provider: &str, model: &str) -> bool {
        !self.tombstones.is_empty() && self.tombstones.contains(&format!("{provider}:{model}"))
    }

    /// 幂等投递：同一 `eventId` 不重复，`seq` 倒退的旧事件直接丢。
    pub fn push_events(&mut self, incoming: Vec<FeedEvent>) {
        for e in incoming {
            if e.seq > 0 && self.seq > 0 && e.seq < self.seq {
                continue;
            }
            if self.events.iter().any(|x| x.id == e.id) {
                continue;
            }
            self.events.insert(0, e);
        }
        self.events.truncate(50);
    }
}

/// `notify` 策略下暂存的版本信息。**不存快照**：用户点「应用」时重新拉取，
/// 免得把一份可能已经过期的字节留在本地。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedPending {
    pub seq: u64,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "publishedAt")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// 一条变更事件。UI 渲染事件，不渲染版本号——用户关心「多了什么」，不关心 `2026.10.19.3`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub seq: u64,
    /// `provider.added` / `model.added` / `model.removed` / `quota.changed`
    /// / `capability.changed` / `incident` / `quirk.changed`
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

fn default_channel() -> String {
    "stable".into()
}

fn default_policy() -> String {
    "auto".into()
}

fn default_check_minutes() -> u32 {
    15
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_with(healths: &[Option<Health>]) -> Provider {
        let accounts: Vec<Account> = healths
            .iter()
            .enumerate()
            .map(|(i, h)| Account {
                id: format!("acct_{i}"),
                name: format!("key-{i}"),
                key_mask: "sk-••••1234".into(),
                enabled: true,
                added_at: String::new(),
                quota: None,
                health: h.clone(),
            })
            .collect();
        Provider {
            id: "groq".into(),
            name: "Groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            tier: "A".into(),
            homepage: None,
            note: None,
            key_mask: String::new(),
            enabled: true,
            added_at: String::new(),
            models: Vec::new(),
            quota: None,
            accounts,
            health: Health::default(),
        }
    }

    fn ok(ms: u64) -> Health {
        Health {
            status: HealthStatus::Healthy,
            latency_ms: Some(ms),
            checked_at: Some("2026-10-19T12:00:00Z".into()),
            model_count: Some(7),
            error: None,
        }
    }

    fn bad(msg: &str) -> Health {
        Health {
            status: HealthStatus::Error,
            latency_ms: Some(9),
            checked_at: Some("2026-10-19T12:00:00Z".into()),
            model_count: None,
            error: Some(msg.into()),
        }
    }

    /// 一条 key 挂了，另一条还好着 —— 这家仍然是好的。
    /// 之前的实现只写厂商级字段，界面会把整个厂商刷红。
    #[test]
    fn one_dead_key_does_not_darken_the_provider() {
        let mut p = provider_with(&[Some(bad("401 invalid api key")), Some(ok(120))]);
        p.sync_health();
        assert!(matches!(p.health.status, HealthStatus::Healthy));
        assert_eq!(p.health.latency_ms, Some(120));
        assert_eq!(p.health.model_count, Some(7));
    }

    /// 全部失败才报错，并且用最近那条的原文。
    #[test]
    fn all_keys_dead_surfaces_the_error() {
        let mut p = provider_with(&[Some(bad("401 invalid api key"))]);
        p.sync_health();
        assert!(matches!(p.health.status, HealthStatus::Error));
        assert_eq!(p.health.error.as_deref(), Some("401 invalid api key"));
    }

    /// 一条都没校验过时不要凭空写个 healthy/error —— 「没测过」得保持原样。
    #[test]
    fn no_probe_yet_leaves_health_untouched() {
        let mut p = provider_with(&[None, None]);
        p.health = Health {
            status: HealthStatus::Unknown,
            ..Default::default()
        };
        p.sync_health();
        assert!(matches!(p.health.status, HealthStatus::Unknown));
    }

    #[test]
    fn migrate_bumps_version_and_keeps_user_values() {
        let mut d = AppData::default();
        d.version = 1; // 老文件：没有 tombstones，也没有 fallback 窗口
        d.routing.fallback_horizon_ms = 0;
        d.routing.chain = vec!["groq:llama".into()];

        assert!(d.migrate(), "v1 文件必须报告需要写回");
        assert_eq!(d.version, SCHEMA_VERSION);
        assert_eq!(d.routing.fallback_horizon_ms, 30_000);
        assert_eq!(d.routing.chain, vec!["groq:llama"], "用户的路由偏好一个字都不许动");
        assert!(d.tombstones.providers.is_empty());
        // 已经是新版本 → 无事可做，不该反复写盘
        assert!(!d.migrate());
    }

    #[test]
    fn migrate_drops_tombstone_when_entity_exists() {
        let mut d = AppData::default();
        d.providers.push(provider_with(&[None]));
        d.tombstones.bury_provider("groq");
        d.tombstones.bury_account("acct_0");
        d.tombstones.bury_provider("gone");

        assert!(d.migrate(), "墓碑与实体矛盾时要清掉");
        assert!(!d.tombstones.providers.contains_key("groq"), "实体为准");
        assert!(!d.tombstones.accounts.contains_key("acct_0"));
        assert!(d.tombstones.providers.contains_key("gone"), "真删掉的要留着");
    }

    #[test]
    fn tombstones_bury_and_revive() {
        let mut t = Tombstones::default();
        t.bury_provider("groq");
        assert!(t.is_buried("groq"));
        assert!(t.providers["groq"].starts_with("20"), "要记时间，合并时才能比先后");
        assert!(t.revive_provider("groq"), "显式加回来要清墓碑");
        assert!(!t.is_buried("groq"));
        assert!(!t.revive_provider("groq"), "没删过就不该报删过");
    }
}
