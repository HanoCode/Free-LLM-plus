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
    /// 扩展配置（ASR / 图片生成）。只存开关与上游地址；凭据进密钥库，
    /// 槽位 `ext:<id>`，这里只留掩码。
    #[serde(default, rename = "extensions")]
    pub extensions: BTreeMap<String, ExtState>,
    /// Pro 本机功能配置（v1.12）。云端只做授权，功能全在本地——
    /// 这里只存「用户想不想用」，能不能用由 gateway 的 `pro::allow()` 统一判。
    #[serde(default)]
    pub pro: ProCfg,
}

/// Pro 功能开关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProCfg {
    /// 会话预热：粘住的会话余量见底前，提前体检下一个候选
    #[serde(default = "default_true")]
    pub prewarm: bool,
    /// 自定义解粘阈值（余量比例）。`None` = 用内置默认 0.10。
    /// 只在 Pro 下生效——免费版永远用默认值。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "unstickRatio")]
    pub unstick_ratio: Option<f64>,
    /// 粘性窗口（分钟）。`None` = 内置默认 30 分钟。只在 Pro 下生效，
    /// 可用范围由 gateway 的 `pro::stick_ttl_ms` 夹取（5..=1440）。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "stickTtlMin")]
    pub stick_ttl_min: Option<u32>,
    /// 自动备份
    #[serde(default)]
    pub backup: BackupCfg,
}

impl Default for ProCfg {
    fn default() -> Self {
        Self {
            prewarm: true,
            unstick_ratio: None,
            stick_ttl_min: None,
            backup: BackupCfg::default(),
        }
    }
}

/// 自动备份配置。快照 = transfer 信封（含明文 key 的密文），口令由本机
/// 主密钥派生——所以快照只在**本机**可解；跨机器迁移仍走手动迁移码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupCfg {
    #[serde(default)]
    pub enabled: bool,
    /// 快照输出目录（绝对路径）。空 = 未配置，备份不会跑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// 间隔（小时）
    #[serde(default = "default_backup_interval_hours", rename = "intervalHours")]
    pub interval_hours: u32,
    /// 保留份数，超出删最旧
    #[serde(default = "default_backup_keep")]
    pub keep: u32,
    /// 上次成功备份时间（epoch 秒）
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastAt")]
    pub last_at: Option<i64>,
}

impl Default for BackupCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: None,
            interval_hours: default_backup_interval_hours(),
            keep: default_backup_keep(),
            last_at: None,
        }
    }
}

fn default_backup_interval_hours() -> u32 {
    24
}
fn default_backup_keep() -> u32 {
    7
}

/// 单个扩展的用户配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtState {
    #[serde(default)]
    pub enabled: bool,
    /// 上游 OpenAI 兼容端点（含版本前缀，如 https://api.groq.com/openai/v1）
    #[serde(default, rename = "baseUrl")]
    pub base_url: String,
    /// 默认模型（转写模型 / 画图模型）。请求体里带了 model 时以请求为准。
    #[serde(default)]
    pub model: String,
    #[serde(default, rename = "keyMask")]
    pub key_mask: String,
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
    /// 每日 token 上限（输入 + 输出）。None = 不限。
    ///
    /// 与请求数上限互补：一个 agent 反复读大文件时，请求数很少但 token 天文数字，
    /// 只按次数限流等于没限。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "dailyTokenLimit"
    )]
    pub daily_token_limit: Option<u64>,
    /// 模型白名单。空 = 不限；支持 `provider:model` 与 `前缀*`。
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        rename = "allowedModels"
    )]
    pub allowed_models: Vec<String>,
    /// 过期时刻（RFC3339）。到点后直接拒，不用等计数器。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "expiresAt")]
    pub expires_at: Option<String>,
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
    /// 今日 token 用量（输入 + 输出），跨天惰性归零
    #[serde(default, rename = "dayTokens")]
    pub day_tokens: u64,
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
            extensions: BTreeMap::new(),
            pro: ProCfg::default(),
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
    /// 工具输出压缩。默认关——它会丢信息，不能让用户不知情地开着。
    #[serde(default)]
    pub compress: CompressConfig,
    /// 按调用方标识（`x-freeport-identifier`）的每分钟请求上限。0 = 关闭。
    ///
    /// 与虚拟 key 的区别：key 是「谁在调用」，identifier 是「同一条 key 下的哪个终端用户」。
    /// 把一个 key 嵌进前端页面时必须靠它，否则一个用户打满就把所有人都挡了。
    #[serde(default, rename = "identifierLimit")]
    pub identifier_limit: u32,
    /// 精确匹配响应缓存。默认关：开了之后「再问一次」可能拿到旧答案。
    #[serde(default)]
    pub cache: CacheConfig,
    /// 每条凭据同时在途的请求上限。0 = 不限（默认）。
    ///
    /// 免费额度是按条限流的，同一秒发十个请求只会换来九个 429 和一段
    /// 被浪费的等待；限成一个队列反而更快拿全。默认关是因为单机场景下
    /// 让人「排一下队」比自己决定打几个更容易被理解成卡住。
    #[serde(default, rename = "maxInflightPerAccount")]
    pub max_inflight_per_account: u32,
    /// OTLP 导出（traces，OTLP/HTTP + JSON）。默认关：会把请求元数据
    /// （不含 prompt 内容）发往用户指定的 collector。
    #[serde(default)]
    pub otlp: OtlpConfig,
}

/// OTLP 导出配置。见 `freeport-gateway/src/otlp.rs`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpConfig {
    #[serde(default)]
    pub enabled: bool,
    /// collector 基址（如 `http://127.0.0.1:4318`）；写到 `/v1/traces` 的按原样用。
    #[serde(default)]
    pub endpoint: String,
    /// 批量上报的间隔（秒），1–300。
    #[serde(default = "default_otlp_interval", rename = "intervalSecs")]
    pub interval_secs: u64,
    /// 单次最多上报多少个 span。
    #[serde(default = "default_otlp_batch", rename = "batchSize")]
    pub batch_size: usize,
    /// 自定义请求头，一行一个 `Key: Value`（collector 需要 Bearer 时用）。
    #[serde(default)]
    pub headers: Vec<String>,
}

fn default_otlp_interval() -> u64 {
    5
}
fn default_otlp_batch() -> usize {
    64
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            interval_secs: default_otlp_interval(),
            batch_size: default_otlp_batch(),
            headers: Vec::new(),
        }
    }
}

/// 响应缓存配置。见 `freeport-gateway/src/cache.rs` 的四条约束。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 生存期（秒）。默认 10 分钟：够覆盖 agent 的重试窗口，又不至于陈旧。
    #[serde(default = "default_cache_ttl", rename = "ttlSecs")]
    pub ttl_secs: u64,
    /// 最多存多少条。满了先清过期项，仍然满就整体清空。
    #[serde(default = "default_cache_max", rename = "maxEntries")]
    pub max_entries: usize,
}

fn default_cache_ttl() -> u64 {
    600
}
fn default_cache_max() -> usize {
    512
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_cache_ttl(),
            max_entries: default_cache_max(),
        }
    }
}

/// 工具输出压缩（§「prompt / 工具输出压缩」）。
///
/// 只动 `role: tool` / `tool_result` 这类**机器产出**的正文，不动用户说的话、
/// 也不动模型的回答：前者丢了没法恢复，后者丢了呢模型自己都不知道它在丢。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 超过这个字符数才压。
    #[serde(default = "default_tool_max", rename = "maxToolChars")]
    pub max_tool_chars: usize,
    /// 保留开头多少字符。
    #[serde(default = "default_keep_head", rename = "keepHead")]
    pub keep_head: usize,
    /// 保留结尾多少字符（报错栈、退出码通常在尾巴上）。
    #[serde(default = "default_keep_tail", rename = "keepTail")]
    pub keep_tail: usize,
    /// 按内容识别再压（git diff / grep / 目录树 / 重复日志）。
    ///
    /// 只影响「压得更聪明」，不影响「压不压」：`enabled = false` 时一律不动。
    /// 认不出来就退回保留头尾的通用截断，不会因为没认出而把内容弄丢。
    #[serde(default = "yes_default", rename = "smartFilters")]
    pub smart_filters: bool,
}

fn yes_default() -> bool {
    true
}

fn default_tool_max() -> usize {
    8_000
}
fn default_keep_head() -> usize {
    2_400
}
fn default_keep_tail() -> usize {
    1_200
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tool_chars: default_tool_max(),
            keep_head: default_keep_head(),
            keep_tail: default_keep_tail(),
            smart_filters: true,
        }
    }
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
            compress: CompressConfig::default(),
            identifier_limit: 0,
            cache: CacheConfig::default(),
            max_inflight_per_account: 0,
            otlp: OtlpConfig::default(),
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
    /// 厂商级代理（`http://127.0.0.1:7890` 这类）。空 = 跟随环境变量。
    /// 优先级：账号级（未实现）> 厂商级 > 环境变量；本机回环永远直连。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// 上游线格式：`openai` / `anthropic` / `responses` / `gemini`。
    ///
    /// 空 = 按 id 推断（`freeport-gateway::upwire::wire_of_provider`），
    /// 这是存量配置与订阅预设的老行为，只为兼容保留：新配置一律显式写。
    /// 需要它的场景很少——只有「额度只能从原生端点用掉」的厂商才值得配，
    /// 讲 OpenAI 兼容线的厂商填了反而是给自己找 400。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<String>,
    #[serde(default)]
    pub health: Health,
}

/// 账号池的用途维度。
///
/// 分池的目的只有一个：**别让某一类流量烧掉不该烧的额度**。典型的三种诉求是
/// 「留给 Claude Code 的那把 key 不许被随手 curl 用掉」「带图的请求只走测过视觉
/// 的那家」「巡检别碰我的主力号」。
///
/// 语义刻意做成「标签 = 准入限制」而不是「标签 = 分配」：
/// 账号的 `purposes` **为空表示不限**，什么活都接。反过来（空 = 什么都不能干）
/// 会让所有存量用户升级后一条路都没有——那是最坏的兼容性事故。
///
/// 因此它是**收窄**而不是**切分**：一块额度没有因为打标签而变小，
/// 只是少了几类请求来抢它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    /// 普通对话。未标注用途的请求默认落这里。
    Chat,
    /// 来自已注入 Agent 的流量（每个 Agent 一把 `agent:<id>` key，天然可识别）。
    Coding,
    /// 带图片的请求。判定优先于 coding：图片请求要的是视觉能力，不是身份。
    Vision,
    /// 巡检探针。单独成池，是为了让「探针别烧主力号」可配置。
    Probe,
}

impl Default for Purpose {
    fn default() -> Self {
        Self::Chat
    }
}

impl Purpose {
    pub const ALL: [Purpose; 4] = [
        Purpose::Chat,
        Purpose::Coding,
        Purpose::Vision,
        Purpose::Probe,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Chat => "chat",
            Purpose::Coding => "coding",
            Purpose::Vision => "vision",
            Purpose::Probe => "probe",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Purpose::Chat),
            "coding" | "code" => Some(Purpose::Coding),
            "vision" | "image" => Some(Purpose::Vision),
            "probe" | "patrol" => Some(Purpose::Probe),
            _ => None,
        }
    }

    /// 请求侧判定。三条规则的顺序是有理由的：
    ///
    /// 1. **显式头最优先**：调用方说「这次是 vision」就照办，不猜；
    /// 2. **含图 ⇒ vision**：这是请求的客观属性，比「谁发的」更贴近它需要什么；
    /// 3. **已注入 Agent ⇒ coding**：身份来自我们自己写进去的 key，不需要用户配置
    ///    任何东西就能区分「IDE 里的 Agent」和「随手 curl」。
    ///
    /// 剩下的一律 chat —— 因为绝大多数未标注的请求确实是普通对话。
    pub fn infer(declared: Option<Purpose>, has_image: bool, from_agent: bool) -> Self {
        Self::infer_with_source(declared, has_image, from_agent).0
    }

    /// 同 [`Purpose::infer`]，另外告诉调用方「是哪条规则命中的」。
    ///
    /// 分开返回而不是让调用方自己再判一遍顺序：两处实现迟早会漂，
    /// 然后日志上写的依据就和实际过滤用的池不一致了——那比不记还坏。
    pub fn infer_with_source(
        declared: Option<Purpose>,
        has_image: bool,
        from_agent: bool,
    ) -> (Self, PurposeSource) {
        Self::infer_deep_with_source(declared, has_image, from_agent, None)
    }

    /// 在 [`Purpose::infer_with_source`] 的三信号之上追加启发式正文判定。
    ///
    /// 带 `payload` 的调用方（网关真实请求）多拿一档：显式头、带图、agent key
    /// 都没命中时，看一眼请求长得像不像编码——tools 定义、代码围栏、编码类
    /// system。这些是**零成本**信号，不用小模型、不烧额度、不加延迟；
    /// 判不出就回落 chat，与旧行为完全一致。
    pub fn infer_deep_with_source(
        declared: Option<Purpose>,
        has_image: bool,
        from_agent: bool,
        payload: Option<&serde_json::Value>,
    ) -> (Self, PurposeSource) {
        if let Some(p) = declared {
            return (p, PurposeSource::Header);
        }
        if has_image {
            return (Self::Vision, PurposeSource::Image);
        }
        if from_agent {
            return (Self::Coding, PurposeSource::AgentKey);
        }
        if let Some(body) = payload {
            if matches!(Self::heuristic(body), Some(Self::Coding)) {
                return (Self::Coding, PurposeSource::Heuristic);
            }
        }
        (Self::Chat, PurposeSource::Default)
    }

    /// 零成本意图判定：只回答「这单是不是编码」。
    ///
    /// vision / probe 有更硬的信号（图片是客观事实、探针是自己人），不在这里猜。
    /// 只会返回 `Some(Coding)` 或 `None` —— 打分不足时保持沉默，让上层回落 chat。
    ///
    /// 打分口径（≥3 判编码）：
    /// - 请求带 `tools` 定义：+2（agent/IDE 客户端才会带工具表，权重最高）；
    /// - 正文出现代码围栏 ``` ：+2（贴代码问问题，哪怕不是 agent 也在写代码）；
    /// - 消息里有 `role:"tool"`：+1（工具回传结果，多半在工具循环里）；
    /// - system 里命中编码关键词：每类 +1，封顶 +2（中英文各算一类）。
    ///
    /// 刻意保守：漏判只是少享受一次分池，误判却会把聊天流量挡在 chat 池外——
    /// 阈值取 3 而不是 2，就是为了把「贴了一段报错问怎么回事」这种边界情形
    /// 留在 chat 里。
    pub fn heuristic(payload: &serde_json::Value) -> Option<Self> {
        let mut score = 0i32;

        if payload
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            score += 2;
        }

        let mut fenced = false;
        let mut tool_role = false;
        let mut sys_kw_en = false;
        let mut sys_kw_zh = false;
        for msg in payload
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role == "tool" {
                tool_role = true;
            }
            let texts: Vec<&str> = match msg.get("content") {
                Some(serde_json::Value::String(s)) => vec![s.as_str()],
                // 多模态内容里只看文本段
                Some(serde_json::Value::Array(a)) => a
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect(),
                _ => Vec::new(),
            };
            for text in texts {
                if !fenced && text.contains("```") {
                    fenced = true;
                }
                if role == "system" {
                    let low = text.to_ascii_lowercase();
                    sys_kw_en |= ["coding agent", "code assistant", "software engineer", "programmer", "you are claude code", "codex cli", "ide assistant"]
                        .iter()
                        .any(|k| low.contains(k));
                    sys_kw_zh |= ["编程", "代码助手", "程序员", "开发者", "写代码"]
                        .iter()
                        .any(|k| text.contains(k));
                }
            }
        }
        if fenced {
            score += 2;
        }
        if tool_role {
            score += 1;
        }
        if sys_kw_en {
            score += 1;
        }
        if sys_kw_zh {
            score += 1;
        }

        (score >= 3).then_some(Self::Coding)
    }
}

/// 用途判定的依据，随日志一起存。
///
/// 为什么要记这个：池过滤是**静默的**——一条请求被挡在 coding 池外时，
/// 用户看到的只是「为什么没走那把更快的 key」。没有依据就只能猜是
/// header 写错了、还是图片被识别成 vision 了、还是 agent key 生效了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PurposeSource {
    /// 调用方用 `x-freeport-purpose` 显式指定
    Header,
    /// 请求体里带图
    Image,
    /// 用的是注入时发的 `agent:<id>` key
    AgentKey,
    /// 三信号都没命中，但请求特征像编码（tools / 代码围栏 / 编码类 system）
    Heuristic,
    /// 都没命中，按普通对话处理
    Default,
}

impl PurposeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PurposeSource::Header => "header",
            PurposeSource::Image => "image",
            PurposeSource::AgentKey => "agent-key",
            PurposeSource::Heuristic => "heuristic",
            PurposeSource::Default => "default",
        }
    }
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
    /// 这条凭据只接哪些用途的活。**空 = 不限用途**（见 [`Purpose`]）。
    ///
    /// 不下发空数组：老配置里没有这个字段，`skip_serializing_if` 让它们继续以
    /// 「无此字段」的形态存在，而不是被写成一串空数组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub purposes: Vec<Purpose>,
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
            purposes: Vec::new(),
        }
    }

    /// 这条凭据是否接某一类活。
    ///
    /// 空列表 = 不限。这是分池语义的全部实现，路由与巡检共用它，
    /// 免得两边各写一份判断然后慢慢漂移。
    pub fn admits(&self, purpose: Purpose) -> bool {
        self.purposes.is_empty() || self.purposes.contains(&purpose)
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
    /// 连续 7 天 unavailable，或目录标记下线。移出路由与巡检队列，保留历史；
    /// 厂商恢复免费额度后由手动「重新探测」或目录热更新唤醒。
    Retired,
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
            Self::Retired => "retired",
            Self::Error => "error",
        }
    }

    /// 路由是否还愿意尝试。`Unavailable` / `Retired` / `Cooling` 不放行；
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
    /// 首次进入 unavailable 的时刻。连续 7 天没爬出来就 retired。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "unavailableSince")]
    pub unavailable_since: Option<String>,
    /// 最近一次从 unavailable / retired 爬回来的时刻。每日 Digest 用它统计「恢复」，
    /// 不然「昨天挂了今天好了」这件事在状态里留不下痕迹。
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "recoveredAt")]
    pub recovered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastOkAt")]
    pub last_ok_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 实测能力，优先于目录声明
    #[serde(default, skip_serializing_if = "ModelCaps::is_empty")]
    pub caps: ModelCaps,
    /// 衰减加权的成功/失败样本（见 `score`）。状态机看「现在」，这里看「长期」。
    #[serde(default, rename = "okWeight", skip_serializing_if = "is_zero")]
    pub ok_weight: f64,
    #[serde(default, rename = "failWeight", skip_serializing_if = "is_zero")]
    pub fail_weight: f64,
    /// 上一次把这些权重衰减到哪一刻（毫秒）。0 = 还没有样本。
    #[serde(default, rename = "decayAt", skip_serializing_if = "is_zero_i64")]
    pub decay_at: i64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

impl ModelHealth {
    pub fn is_usable(&self) -> bool {
        self.status.is_usable()
    }

    /// 最近一次成功距今多少毫秒；从未成功返回 None。
    pub fn since_ok_ms(&self, now_ms: i64) -> Option<i64> {
        self.last_ok_at.as_deref().and_then(parse_rfc3339_ms).map(|t| now_ms - t)
    }

    /// 记一次成功/失败样本（衰减加权）。
    pub fn note_ok(&mut self, now_ms: i64) {
        crate::score::note(true, now_ms, &mut self.ok_weight, &mut self.fail_weight, &mut self.decay_at);
    }

    pub fn note_fail(&mut self, now_ms: i64) {
        crate::score::note(false, now_ms, &mut self.ok_weight, &mut self.fail_weight, &mut self.decay_at);
    }

    /// 长期可靠性后验期望。样本不足返回 `None`（没意见，交给状态机）。
    pub fn reliability(&self, now_ms: i64) -> Option<f64> {
        crate::score::expected(self.ok_weight, self.fail_weight, self.decay_at, now_ms)
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
    /// 命名链。键是 profile 名，值是 `provider:model` 列表。
    ///
    /// 与 `chain` 的区别是「谁在用」：`chain` 是全局兜底，命名链由
    /// `auto:<名字>` 或用途映射（`purposeChain`）点中。分开存是为了让
    /// 「coding 走强模型」不影响「随手问一句」的默认链。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub chains: BTreeMap<String, Vec<String>>,
    /// 用途 → 命名链。键是 `chat` / `coding` / `vision` / `probe`。
    ///
    /// 只影响「链从哪来」，不改准入：用途分池仍然是 Gate 的事。
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        rename = "purposeChain"
    )]
    pub purpose_chain: BTreeMap<String, String>,
    /// 全部候选都被冷却挡掉时，是否把「马上就要到期」的那一条提前放出去试一次。
    ///
    /// 默认开：冷却只是暂时状态，直接回 503 等于把它升级成硬中断。
    #[serde(default = "yes")]
    pub fallback_when_all_cooling: bool,
    /// 回退窗口（毫秒）。**只回退快到期的**——日额度耗尽那种几小时后的
    /// reset 撞了也是白撞，如实告诉用户还要等多久更有用。
    #[serde(default = "default_fallback_horizon")]
    pub fallback_horizon_ms: i64,
    /// 三维评分权重。`enabled = false` 时维持旧公式（档位 × 余量 × 健康），
    /// 打开后第三因子换成「可靠性 / 速度 / 智能」的加权合成。
    ///
    /// 只被 `balanced` 策略使用：极值策略的定义就是「只看一个维度」，
    /// 再叠一层权重会把「最快」变成「没那么快」。
    #[serde(default, skip_serializing_if = "RouteWeights::is_unset")]
    pub weights: RouteWeights,
    /// 策略档（Pro）：把「策略 + 三维权重」绑给某类调用方。
    /// 匹配顺序 = 数组顺序，第一条命中的生效。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<StrategyProfile>,
}

/// 策略档。两个字段都是 `None` 的档永远命中，不合法——写入口要拦。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyProfile {
    pub id: String,
    pub name: String,
    /// 匹配的虚拟 key id 前缀（如 `agent:claude-code`）。`None` = 不按 key 匹配
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "keyPrefix")]
    pub key_prefix: Option<String>,
    /// 匹配的用途（chat/coding/vision/probe）。`None` = 不按用途匹配
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// 覆盖的策略名（八档之一；parse 失败的档在入口被拒）
    pub strategy: String,
    /// 覆盖的三维权重。`None` = 沿用全局
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<RouteWeights>,
}

impl StrategyProfile {
    /// 是否合法：至少绑一个匹配维度、名字非空、策略认识。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("策略档名字不能为空".into());
        }
        if self.key_prefix.is_none() && self.purpose.is_none() {
            return Err("策略档至少要绑定 key 前缀或用途之一".into());
        }
        if crate::route::Strategy::parse(&self.strategy).as_str() != self.strategy.trim() {
            return Err(format!("不认识的路由策略：{}", self.strategy));
        }
        Ok(())
    }

    /// 这条请求（key id + 用途）是否命中本档。
    pub fn matches(&self, key_id: &str, purpose: Purpose) -> bool {
        let key_ok = match &self.key_prefix {
            None => true,
            Some(p) => key_id.starts_with(p.as_str()),
        };
        let purpose_ok = match &self.purpose {
            None => true,
            Some(p) => Purpose::parse(p) == Some(purpose),
        };
        key_ok && purpose_ok
    }
}

/// 三维评分权重（百分比，不必凑够 100，按总和归一）。
///
/// 默认 70 / 15 / 15：先保证能跑通，再谈快慢与聪明。这是**相对**权重，
/// 三个都填 10 与都填 100 等价。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteWeights {
    /// 关掉即回落到「只看健康」，老用户升级后行为不变。
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_w_reliability")]
    pub reliability: u8,
    #[serde(default = "default_w_speed")]
    pub speed: u8,
    #[serde(default = "default_w_intelligence")]
    pub intelligence: u8,
}

fn default_w_reliability() -> u8 {
    70
}
fn default_w_speed() -> u8 {
    15
}
fn default_w_intelligence() -> u8 {
    15
}

impl Default for RouteWeights {
    fn default() -> Self {
        Self {
            enabled: false,
            reliability: default_w_reliability(),
            speed: default_w_speed(),
            intelligence: default_w_intelligence(),
        }
    }
}

impl RouteWeights {
    /// 配置落盘时省掉「没开且全是默认值」的那一段，别把默认值写进用户文件。
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    /// 启用了才返回自身，避免调用点到处写 `if weights.enabled`。
    pub fn active(&self) -> Option<RouteWeights> {
        if self.enabled {
            Some(*self)
        } else {
            None
        }
    }
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
            chains: BTreeMap::new(),
            purpose_chain: BTreeMap::new(),
            fallback_when_all_cooling: true,
            fallback_horizon_ms: default_fallback_horizon(),
            weights: RouteWeights::default(),
            profiles: Vec::new(),
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
    /// 这次请求进了哪个账号池。
    ///
    /// 必须记：池过滤是静默的，不写下来的话，事后完全无法解释
    /// 「为什么另一把更宽裕的 key 没被选中」。
    #[serde(default)]
    pub purpose: Purpose,
    /// 用途是怎么判出来的（header / image / agent-key / heuristic / default）
    #[serde(
        rename = "purposeSource",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub purpose_source: Option<PurposeSource>,
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
    /// 最近一次同步实际应用了哪些字段包（服务端按许可证 `feat` 下发的）。
    ///
    /// 同样是「服务端给了什么」的实录。许可证到期后**不主动清**：客户端不判档，
    /// 服务端下次不给就是不再更新，已到手的知识留在目录里。
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "servedFeats")]
    pub served_feats: Vec<String>,
    /// 服务端列了但没能应用的字段包，形如 `model.scores（本版本不支持）`。
    /// 静默丢弃会让「Pro 面板为什么是空的」变成一个查不出来的现象。
    #[serde(default, skip_serializing_if = "Vec::is_empty", rename = "featsSkipped")]
    pub feats_skipped: Vec<String>,
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
            served_feats: Vec::new(),
            feats_skipped: Vec::new(),
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
        self.due_for(now_ms, "free")
    }

    /// 按档位决定轮询间隔。
    ///
    /// 这只是**礼貌**，不是权限：免费档拿到的是月度快照，一小时问一次和一分钟
    /// 问一次结果一样，只是后者白耗自己的带宽和服务端的日志。真正的时效差
    /// 由服务端下发哪份快照决定（见 `crates/freeport-catalogd`）。
    pub fn due_for(&self, now_ms: i64, tier: &str) -> bool {
        if !self.accepts_feed() {
            return false;
        }
        let Some(last) = self.last_check_at.as_deref() else {
            return true;
        };
        let floor_min = if tier.eq_ignore_ascii_case("pro") { 1 } else { 30 };
        let interval = self.check_minutes.max(floor_min);
        match parse_rfc3339_ms(last) {
            Some(t) => now_ms - t >= (interval as i64) * 60_000,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

    #[test]
    fn provider_proxy_survives_a_config_roundtrip_and_stays_absent_when_unset() {
        let raw = r#"{"id":"p","name":"P","baseUrl":"https://x/v1","proxy":"http://127.0.0.1:7890"}"#;
        let p: Provider = serde_json::from_str(raw).unwrap();
        assert_eq!(p.proxy.as_deref(), Some("http://127.0.0.1:7890"));
        let back: Provider = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.proxy.as_deref(), Some("http://127.0.0.1:7890"));

        let plain: Provider = serde_json::from_str(r#"{"id":"p","name":"P"}"#).unwrap();
        assert!(plain.proxy.is_none());
        assert!(
            !serde_json::to_string(&plain).unwrap().contains("proxy"),
            "没设代理就不该在配置里留一个空字段，否则用户会以为它被改过"
        );
    }

    /// 免费档问得再勤也只会拿到月度快照，所以客户端自觉把节奏放到 30 分钟起；
    /// Pro 才允许 1 分钟级轮询。这不是权限，是礼貌——真正的时效差在服务端。
    #[test]
    fn feed_cadence_follows_the_tier() {
        let mut f = FeedState {
            enabled: true,
            url: "https://example.com/v1/latest".into(),
            check_minutes: 1,
            last_check_at: Some("2026-10-21T03:00:00Z".into()),
            ..Default::default()
        };
        let now = parse_rfc3339_ms("2026-10-21T03:05:00Z").unwrap();
        assert!(!f.due_for(now, "free"), "免费档 5 分钟内不该再问");
        assert!(f.due_for(now, "pro"), "Pro 档 1 分钟就该问");

        f.check_minutes = 60;
        assert!(!f.due_for(now, "pro"), "用户自己配的更大间隔仍然生效");
        let later = parse_rfc3339_ms("2026-10-21T04:01:00Z").unwrap();
        assert!(f.due_for(later, "pro"));
    }

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
                purposes: Vec::new(),
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
            proxy: None,
            wire: None,
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

    // ── 用途分池 ──────────────────────────────────────────────

    /// 判定顺序不是随意的：显式声明 > 请求客观属性 > 发送方身份。
    ///
    /// 最容易写错的是「带图的 coding 请求」——它必须算 vision，
    /// 因为用户标 vision 池就是为了让图片走特定账号。
    #[test]
    fn purpose_inference_order_is_declared_then_image_then_agent() {
        // 显式声明压倒一切，包括图片
        assert_eq!(
            Purpose::infer(Some(Purpose::Coding), true, true),
            Purpose::Coding
        );
        // 带图就是 vision，哪怕发送方是 coding agent
        assert_eq!(Purpose::infer(None, true, true), Purpose::Vision);
        // 没图但来自已注入的 Agent
        assert_eq!(Purpose::infer(None, false, true), Purpose::Coding);
        // 什么线索都没有
        assert_eq!(Purpose::infer(None, false, false), Purpose::Chat);
    }

    #[test]
    fn purpose_round_trips_through_json_as_lowercase() {
        let a = Account {
            purposes: vec![Purpose::Coding, Purpose::Vision],
            ..Account::new("k", "sk-****")
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains("\"purposes\":[\"coding\",\"vision\"]"), "{s}");

        // 老配置没有这个字段 → 空 = 不限，任何用途都接
        let old: Account = serde_json::from_str(r#"{"id":"a1","name":"x"}"#).unwrap();
        assert!(old.purposes.is_empty());
        for p in Purpose::ALL {
            assert!(old.admits(p));
        }
    }

    #[test]
    fn admits_is_empty_means_unrestricted() {
        let mut a = Account::new("k", "sk-****");
        assert!(a.admits(Purpose::Probe), "没打标签的号巡检也能用");

        a.purposes = vec![Purpose::Coding];
        assert!(a.admits(Purpose::Coding));
        assert!(!a.admits(Purpose::Chat));
        assert!(!a.admits(Purpose::Probe), "标了用途就不该被探针烧额度");
    }

    #[test]
    fn purpose_parse_accepts_aliases_and_rejects_noise() {
        assert_eq!(Purpose::parse(" Coding "), Some(Purpose::Coding));
        assert_eq!(Purpose::parse("image"), Some(Purpose::Vision));
        assert_eq!(Purpose::parse("patrol"), Some(Purpose::Probe));
        assert_eq!(Purpose::parse("whatever"), None);
        for p in Purpose::ALL {
            assert_eq!(Purpose::parse(p.as_str()), Some(p));
        }
    }

    #[test]
    fn infer_reports_which_rule_won() {
        // 顺序就是优先级：声明 > 图片 > Agent 身份 > 兜底
        let cases = [
            (Some(Purpose::Coding), true, true, PurposeSource::Header),
            (None, true, true, PurposeSource::Image),
            (None, false, true, PurposeSource::AgentKey),
            (None, false, false, PurposeSource::Default),
        ];
        for (declared, image, agent, want) in cases {
            let (p, src) = Purpose::infer_with_source(declared, image, agent);
            assert_eq!(src, want, "{declared:?}/{image}/{agent}");
            // 依据和结果必须是同一份判定，不能各算一遍
            assert_eq!(p, Purpose::infer(declared, image, agent));
        }
        // 写进日志后能被前端直接读懂
        assert_eq!(
            serde_json::to_string(&PurposeSource::AgentKey).unwrap(),
            "\"agent-key\""
        );
    }

    #[test]
    fn heuristic_flags_agent_like_requests() {
        // tools 定义 + 代码围栏 = 典型编码请求
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "帮我看这段为什么崩：\n```rust\nfn main(){}\n```"}
            ],
            "tools": [{"type": "function", "function": {"name": "read_file"}}]
        });
        assert_eq!(Purpose::heuristic(&body), Some(Purpose::Coding));

        // 编码类 system 单独不够（+1），但代码围栏(+2) + tool 回传(+1) = 3 就够
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are Claude Code, an interactive CLI tool."},
                {"role": "user", "content": "接着修：\n```rust\nfn main(){}\n```"},
                {"role": "tool", "content": "ok"}
            ]
        });
        assert_eq!(Purpose::heuristic(&body), Some(Purpose::Coding));
    }

    #[test]
    fn heuristic_keeps_plain_chat_in_chat() {
        // 贴了报错/代码片段问概念：只有围栏 +2，不到阈值
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "这段 ```SELECT 1``` SQL 是什么意思？"}
            ]
        });
        assert_eq!(Purpose::heuristic(&body), None);

        // 纯聊天
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "今天天气怎么样"}]
        });
        assert_eq!(Purpose::heuristic(&body), None);

        // 空消息也不炸
        let body = serde_json::json!({"messages": []});
        assert_eq!(Purpose::heuristic(&body), None);
    }

    #[test]
    fn deep_inference_slots_heuristic_before_default() {
        // agent key 比启发式可靠，顺序不能颠倒
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "```py\nprint(1)\n```"}],
            "tools": [{"type": "function"}]
        });
        let (p, src) =
            Purpose::infer_deep_with_source(None, false, true, Some(&body));
        assert_eq!((p, src), (Purpose::Coding, PurposeSource::AgentKey));

        // 三信号都没命中才轮到启发式
        let (p, src) =
            Purpose::infer_deep_with_source(None, false, false, Some(&body));
        assert_eq!((p, src), (Purpose::Coding, PurposeSource::Heuristic));

        // 不带 payload 的老调用点行为不变
        let (p, src) = Purpose::infer_deep_with_source(None, false, false, None);
        assert_eq!((p, src), (Purpose::Chat, PurposeSource::Default));

        // 依据写进日志能被前端直接读懂
        assert_eq!(
            serde_json::to_string(&PurposeSource::Heuristic).unwrap(),
            "\"heuristic\""
        );
    }
}
