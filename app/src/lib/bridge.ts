/** 桌面宿主 ↔ 渲染层的类型约定（`window.freeport` 门面） */

export interface GwCooldown {
  key: string;
  until: number;
  reason: string;
  hits: number;
  /** 到期后放不放一个试探请求过去（半开） */
  half_open?: boolean;
  /** 已有试探在途，别的请求仍按冷却处理 */
  probing?: boolean;
  /**
   * 冷却的出处。**只有 `heuristic` 能被探针提前解除**：
   * - `authoritative`：上游给了 Retry-After / 额度重置时刻，这是事实；
   * - `credit`：402 欠费，key 有效不代表充了钱；
   * - `tier`：403 套餐不含该模型，key 有效但模型进不来。
   */
  source?: 'heuristic' | 'authoritative' | 'credit' | 'tier';
}

export interface GwStatus {
  running: boolean;
  /** 暂停中：业务请求 503，控制面与 /health 正常 */
  paused: boolean;
  port: number | null;
  bind: string;
  since: string | null;
  authToken: string;
  requireAuth: boolean;
  requests24h: number;
  /** 单次请求总预算（毫秒）。设置页直接显示这个值，不自己编默认数 */
  requestBudgetMs: number;
  tokensIn: number;
  tokensOut: number;
  errors: number;
  /** 工具输出压缩是否开启（关着就是原样转发） */
  compress: boolean;
  /** 超过多少字符才压 */
  compressMaxToolChars: number;
  /** 内容感知过滤：认出 diff / grep / 日志后按结构裁，关掉只剩首尾截断 */
  compressSmart: boolean;
  /** 同一来源每分钟最多多少次；0 = 不限 */
  identifierLimit: number;
  /** 非流式响应的精确匹配缓存 */
  cache: boolean;
  cacheTtlSecs: number;
  /** OTLP 导出（traces）。默认关，开了把请求元数据推给用户自己的 collector */
  otlpEnabled: boolean;
  otlpEndpoint: string;
  /** 一行一个 `Key: Value` */
  otlpHeaders: string[];
  cooldowns: GwCooldown[];
  error?: string | null;
}

/** 更新检查结果。`latest` 与服务端 tag 同名去 `v` 前缀。 */
export interface UpdateCheck {
  current: string;
  latest: string;
  updateAvailable: boolean;
  url?: string | null;
  notes?: string | null;
  publishedAt?: string | null;
}

/** 自动更新通道（tauri-plugin-updater）。走签名校验 + latest.json。 */
export interface UpdaterStatus {
  current: string;
  available: boolean;
  /** 有新版本时的版本号（不含 v 前缀） */
  version?: string | null;
  /** 发布说明 */
  notes?: string | null;
}

/**
 * 桌面宿主自己的偏好，和网关配置分开：
 * 这三项只影响「这台机器的窗口与进程怎么活」，headless / MCP 形态不关心。
 *
 * 开机自启不在这里——它的真相在系统里（用户可能手删登录项），
 * 所以单独走 `autostart()` / `setAutostart()` 直接问系统。
 */
export interface DesktopPrefs {
  startMinimized: boolean;
  cleanupOnQuit: boolean;
}

export interface PrefsPatch {
  startMinimized?: boolean;
  cleanupOnQuit?: boolean;
}

export type HealthStatus =
  | 'unknown'
  | 'probing'
  | 'healthy'
  | 'degraded'
  | 'cooling'
  | 'unavailable'
  | 'retired'
  | 'error';

/** 能力标记。缺省 = 不知道，false = 测过且不支持。 */
export interface ModelCaps {
  tools?: boolean;
  vision?: boolean;
  json?: boolean;
  reasoning?: boolean;
}

export interface ProviderHealth {
  status: HealthStatus;
  latencyMs?: number;
  checkedAt?: string;
  modelCount?: number;
  error?: string;
}

/**
 * 账号池的用途标签。
 *
 * 空数组 = 不限用途（这是默认，也是老配置的语义）。
 * 打了标签就是**准入限制**：这个号只接列出来的那几类活。
 */
export type AccountPurpose = 'chat' | 'coding' | 'vision' | 'probe';

/** 一条凭据（同一个厂商的第 N 把 key）。额度、冷却、粘性都按它算。 */
export interface GwAccount {
  id: string;
  name: string;
  keyMask: string;
  enabled: boolean;
  hasKey: boolean;
  /** 这条凭据自己的限额。同一厂商的两条 key 是两本独立的账。 */
  quota?: ProviderQuotaInput;
  /** 这条凭据最近一次校验的结果。没测过就没有这个字段。 */
  health?: ProviderHealth;
  /** 凭据形态：copilot / oauth。没有这个字段说明是手填的 API key。 */
  auth?: string;
  /** OAuth token 的到期时间（unix 秒） */
  expiresAt?: number;
  /** 用途标签。空数组 = 不限用途。 */
  purposes: AccountPurpose[];
}

/**
 * 发起登录后要展示给用户的信息。
 *
 * 两条路子的字段互斥，界面按 `kind` 分开渲染：
 * - `device`：设备码（userCode + verificationUri + interval）
 * - `pkce`：授权码（url + redirectUri + auto）
 */
export interface GwLoginStart {
  kind: 'device' | 'pkce';
  loginId: string;
  expiresIn: number;
  /** 「在浏览器打开 X 并输入 Y」，界面直接显示这句 */
  instruction: string;

  /* 设备码 */
  userCode?: string;
  verificationUri?: string;
  verificationUriComplete?: string;
  interval?: number;

  /* 授权码 */
  /** 点开就走的授权地址 */
  url?: string;
  /** 浏览器会跳回的地址；打不开是正常的，用户要把这一整条粘回来 */
  redirectUri?: string;
  /** 回环端口是否就位。false = 只能手工粘贴回调地址 */
  auto: boolean;
}

export interface GwLoginState {
  /** pending / ready / denied / expired / error */
  status: string;
  message?: string;
  accountId?: string;
  login?: string;
}

export interface GwProvider {
  id: string;
  name: string;
  baseUrl: string;
  tier: string;
  homepage?: string;
  note?: string;
  enabled: boolean;
  keyMask: string;
  hasKey: boolean;
  /** 这个厂商下的所有凭据 */
  accounts: GwAccount[];
  models: number;
  health: ProviderHealth;
  /** 厂商级代理地址；空 = 跟随环境变量 */
  proxy?: string;
  freeTier: boolean;
  freeNote?: string;
}

export interface GwModel {
  id: string;
  object: string;
  owned_by: string;
  context_length?: number;
  created?: number;
  freeport?: {
    provider?: string;
    upstreamModel?: string;
    tier?: string;
    health?: string;
    virtual?: boolean;
    note?: string;
    free?: boolean;
    caps?: ModelCaps;
    capsProbed?: boolean;
    latencyMs?: number;
    /** feed 新加入、仍在 canary 里 */
    candidate?: boolean;
    /** canary 进度：连续成功次数 */
    canary?: number;
    /** 模型组：同名模型还有哪几家也能跑（不含本行这家）。裸模型名按组路由 */
    alsoOn?: string[];
  };
}

export interface GwAttempt {
  provider: string;
  /** 这一跳用的是哪条凭据（多账号时才知道是哪个 key 挂的） */
  account?: string;
  model: string;
  ms: number;
  status: number;
  kind: string;
  scope?: string;
  cooledMs?: number;
  detail?: string;
  skipProvider?: boolean;
}

export interface GwLog {
  id: string;
  at: string;
  path: string;
  requestedModel: string;
  model: string | null;
  provider: string | null;
  stream: boolean;
  ok: boolean;
  status: number;
  ms: number;
  usage: { prompt_tokens?: number; completion_tokens?: number; total_tokens?: number } | null;
  attempts: GwAttempt[];
  /** 虚拟 key 的归因。名字是当次快照，key 删了这条日志也还能读懂。 */
  keyId?: string;
  keyName?: string;
  /** 这条请求最终落在哪条凭据上。同样是快照。 */
  accountId?: string;
  accountName?: string;
  /** 这次是不是靠会话粘性插的队 */
  sticky?: boolean;
  /** 进了哪个账号池：chat / coding / vision / probe */
  purpose?: string;
  /** 用途判定的依据：header / image / agent-key / heuristic / default */
  purposeSource?: string;
  error?: string;
}

/** 官方口径的一个额度窗口（不是本机账本）。 */
export interface GwUsageWindow {
  /** `five_hour` / `seven_day` / `seven_day_opus` …，界面自己翻译 */
  name: string;
  /** 已用百分比 0..100 */
  usedPct: number;
  resetsAt?: string;
}

/**
 * 官方额度查询结果。
 *
 * `ok=false` 时 `note` 一定有值——查额度失败就是这个样子，
 * 界面上它是一行字，不是一个错误页。
 */
export interface GwUsage {
  kind: string;
  ok: boolean;
  windows: GwUsageWindow[];
  /** 余额；`undefined` = 没有这个概念或不限量，**不是 0** */
  credits?: number;
  note?: string;
  checkedAt: string;
}

/**
 * 网关运行时开关。全部可选：只发要改的那一项，
 * 没发的保持原值（后端也是这个语义）。
 */
export interface ConfigPatch {
  requireAuth?: boolean;
  port?: number;
  requestBudgetMs?: number;
  /** 工具输出压缩：默认关，开了会丢信息 */
  compress?: boolean;
  compressMaxToolChars?: number;
  /** 内容感知过滤（diff/grep/日志去重）；关掉只剩头尾截断 */
  compressSmart?: boolean;
  /** 同一来源每分钟最多多少次；0 = 不限 */
  identifierLimit?: number;
  /** 非流式响应的精确匹配缓存 */
  cache?: boolean;
  cacheTtlSecs?: number;
  /** OTLP 导出开关 */
  otlpEnabled?: boolean;
  /** collector 地址（基址或完整 /v1/traces） */
  otlpEndpoint?: string;
  /** 自定义请求头，一行一个 `Key: Value` */
  otlpHeaders?: string[];
}

/** 路由顺序策略。与 Rust 侧 `route::Strategy` 一一对应。 */
export type RouteStrategy = 'balanced' | 'order' | 'rotate' | 'usage' | 'pace' | 'fastest' | 'stablest' | 'smartest';

export interface GwRouting {
  strategy: string;
  chain: string[];
  /**
   * 命名链：`auto:<名字>` 走这里。链是**优先**不是独占——
   * 都没命中时仍然回落到全局链和其余候选。
   */
  chains?: Record<string, string[]>;
  /** 用途 → 链名。默认把 coding 指到 `coding`，省得每个用户自己配。 */
  purposeChain?: Record<string, string>;
  /** 三维评分权重。始终下发，未启用时 enabled=false + 70/15/15 默认值。 */
  weights?: GwWeights;
  /** 策略档（Pro）：按 key 前缀 / 用途绑定的策略与权重。 */
  profiles?: StrategyProfile[];
  cooldowns: GwCooldown[];
}

/** 策略档（Pro）。keyPrefix 与 purpose 至少绑一个，第一条命中的生效。 */
export interface StrategyProfile {
  id: string;
  name: string;
  /** 匹配的虚拟 key id 前缀，如 `agent:claude-code` */
  keyPrefix?: string | null;
  /** 匹配的用途：chat / coding / vision / probe */
  purpose?: string | null;
  strategy: string;
  weights?: GwWeights | null;
}

/* ── Pro 本机功能（v1.12）：情报 / 报表 / 备份，全部本地计算 ── */

/** 本机额度情报：按 provider×model 聚合最近请求 + 账本 + 健康度。 */
export interface GwIntel {
  /** false 时 models 恒为空——直接进 paywall，不渲染假数据 */
  pro: boolean;
  models: GwModelIntel[];
  /** 参与聚合的日志条数（内存环上限 300） */
  scanned: number;
}

export interface GwModelIntel {
  provider: string;
  model: string;
  requests: number;
  errors: number;
  okRate: number;
  p50Ms: number | null;
  p95Ms: number | null;
  /** 该厂商账本最紧窗口的剩余比例；null = 没记过账 */
  remaining: number | null;
  health: string | null;
  lastError: string | null;
  lastUsedAt: string | null;
}

/** 用量报表：基于本地持久化日志（Pro = 90 天）。 */
export interface GwReports {
  pro: boolean;
  /** 持久化覆盖的天数（0 = 没有留存文件） */
  days: number;
  daily: GwDayStat[];
  topModels: GwModelStat[];
  byPurpose: GwBucket[];
  byKey: GwBucket[];
  requests: number;
  errors: number;
  tokensIn: number;
  tokensOut: number;
}

export interface GwDayStat {
  date: string;
  requests: number;
  errors: number;
  tokensIn: number;
  tokensOut: number;
}

export interface GwModelStat {
  provider: string;
  model: string;
  requests: number;
  errors: number;
  tokensIn: number;
  tokensOut: number;
  avgOkMs: number;
}

/** Pro 功能开关与本机数据。 */
export interface GwPro {
  pro: boolean;
  prewarm: boolean;
  unstickRatio?: number | null;
  /** 粘性窗口（分钟）。null = 默认 30 分钟；免费版恒为 null */
  stickTtlMin?: number | null;
  backup: GwBackupCfg;
}

export interface GwBackupCfg {
  enabled: boolean;
  dir?: string | null;
  intervalHours: number;
  keep: number;
  lastAt?: number | null;
}

export interface GwBackupFile {
  name: string;
  bytes: number;
  modified?: string | null;
}

export interface ProPatch {
  prewarm?: boolean;
  /** null = 回到默认 0.10 */
  unstickRatio?: number | null;
  /** 粘性窗口（分钟，5–1440）；null = 回默认 30 */
  stickTtlMin?: number | null;
  backupEnabled?: boolean;
  backupDir?: string;
  backupIntervalHours?: number;
  backupKeep?: number;
}

/**
 * 「均衡」策略的三维权重（百分比，按总和归一）。
 * 关掉即回到旧公式：档位 × 余量 × 健康。
 */
export interface GwWeights {
  enabled: boolean;
  reliability: number;
  speed: number;
  intelligence: number;
}

/** 权重三因子：档位 × 余量 × 健康。界面上逐个摊开，不做黑箱打分。 */
export interface RouteFactors {
  tier: number;
  headroom: number;
  health: number;
  /**
   * 启用三维权重后，实际乘进去的第三个因子（可靠性/速度/智能 的合成）。
   * 没启用时不出现——它不是 0，是「不适用」。
   */
  blend?: number | null;
  /**
   * 长期可靠性后验（衰减加权 Beta 的期望值）。
   * `null` = 样本不足，此时可靠性因子退回健康值，界面要说「样本不足」而不是 0%。
   */
  reliability?: number | null;
  /** 能力门禁的乘数：请求要的工具/视觉/JSON 这个模型没实测支持时 < 1 */
  caps?: number;
}

export interface GwRouteHop {
  provider: string;
  model: string;
  /** 这条跳属于哪个凭据。同一厂商多把 key 时靠它区分 */
  account?: string;
  tier: string;
  weight: number;
  remaining: number;
  health: HealthStatus;
  factors: RouteFactors;
  /** 请求要的能力这个模型没测过：仍可用，但排在确认支持的后面 */
  capsUnknown?: boolean;
}

/** kind 用于分区展示：quota / health / cooldown */
export interface GwRouteReject {
  provider: string;
  model: string;
  reason: string;
  kind: string;
}

/** 扩展（ASR / 图片生成等 sidecar 能力）。字段由 freeport-ext::GwExt 序列化而来 */
export interface GwExt {
  id: string;
  name: string;
  kind: 'asr' | 'image' | string;
  version: string;
  author: string;
  /** manifest 授权的上游域名清单 */
  domains: string[];
  paths: string[];
  /** manifest 验签是否通过；false 时整条不可用 */
  trusted: boolean;
  /** sidecar 进程是否在跑（按需拉起，空闲 10 分钟自动回收） */
  running: boolean;
  enabled: boolean;
  baseUrl: string;
  model: string;
  keyMask: string;
  hasKey: boolean;
  /** 调用方实际请求的网关端点 */
  endpoint: string;
}

/** `/v1/route` 的结构化版本：这一秒真的会按 `chain` 的顺序发出去。 */
export interface GwRoute {
  model: string;
  session: string;
  /** 这单按哪个账号池算的。分池出问题时第一个要看的就是它。 */
  purpose: AccountPurpose;
  sticky: boolean;
  strategy: string;
  budgetMs: number;
  chain: GwRouteHop[];
  rejected: GwRouteReject[];
  cooldowns: GwCooldown[];
  /** 池体检查：链路之外的第二个问题——「这个池有几个号接着、第一跳够不够」 */
  pool?: GwRoutePool;
}

/**
 * 预演的结论码。网关只给码，措辞留给各端：
 * GUI 要中英双语，MCP 只要一句能直接回给用户的话。
 */
export type RouteVerdict =
  | 'ok'
  | 'tight'
  | 'too-big'
  | 'pool-failing'
  | 'empty-pool'
  | 'no-candidate';

/** 池体检查。回答「这条请求现在会落在谁身上、余量够不够」。 */
export interface GwRoutePool {
  purpose: AccountPurpose;
  /** 启用的凭据里，有几条接这个池。空池是最常见的现场：勾错了标签。 */
  accounts: { total: number; admits: number; blocked: number };
  /** 24h 内这个池的战绩。没有请求是 `null`，不是 0%——两件事不一样 */
  recent: { requests: number; errors: number; okRate: number | null } | null;
  /** 这一单会落在谁身上。没有候选时为 null */
  landsOn: { provider: string; account: string; model: string; headroom: number } | null;
  /** 链上余量最紧的一跳：hedge 换家时会用到它，不能只看第一跳 */
  tightest: {
    provider: string;
    account: string;
    /** tpm / tpd / rpm / rpd：光看百分比不知道是什么见底了 */
    window: string;
    /** 剩余绝对值（token 或次数） */
    left: number;
    headroom: number;
    resetAt: number;
  } | null;
  /** 传给预演的 token 估算。没传就是 null，此时 `fits` 也不判 */
  cost: number | null;
  fits: boolean | null;
  shortfall: { window: string; left: number } | null;
  verdict: RouteVerdict;
}

export interface ProviderPreset {
  id: string;
  name: string;
  baseUrl: string;
  tier: string;
  homepage?: string;
  note?: string;
}

export interface ProviderInput {
  id: string;
  name?: string;
  baseUrl: string;
  apiKey: string;
  tier?: string;
}

/** 额度上限。缺省 = 这个维度不拦，只记账。 */
export interface ProviderQuotaInput {
  rpm?: number;
  rpd?: number;
  tpm?: number;
  tpd?: number;
}

/** 给已有厂商再加一条 key。 */
export interface AccountInput {
  providerId: string;
  apiKey: string;
  name?: string;
  quota?: ProviderQuotaInput;
  purposes?: AccountPurpose[];
}

/** 改一条凭据。`clearQuota` 用来把限额清空（`undefined` 表示不动）。 */
export interface AccountPatch {
  name?: string;
  enabled?: boolean;
  quota?: ProviderQuotaInput;
  clearQuota?: boolean;
  /** 传空数组就是清成「不限用途」 */
  purposes?: AccountPurpose[];
}

/* ── 目录 ─────────────────────────────────────────── */

export interface GwCatalogMeta {
  source: string;
  fetchedAt?: string;
  builtAt?: string;
  providers: number;
  models: number;
  freeModels: number;
  error?: string;
}

export interface GwCatalogQuota {
  rpm?: number;
  rpd?: number;
}

export interface GwCatalogProvider {
  id: string;
  name: string;
  api?: string;
  docs?: string;
  tier: string;
  freeTier: boolean;
  freeNote?: string;
  quota?: GwCatalogQuota;
  models: number;
  freeModels: number;
  added: boolean;
  source: string;
}

export interface GwCatalog {
  meta: GwCatalogMeta;
  providers: GwCatalogProvider[];
}

/* ── 探针与巡检 ───────────────────────────────────── */

export interface GwModelProbe {
  ok: boolean;
  status: number;
  latencyMs: number;
  error?: string;
  health: HealthStatus;
  caps?: ModelCaps;
}

/** feed 的一条变更事件。界面渲染事件，不渲染版本号。 */
export interface GwFeedEvent {
  id: string;
  seq: number;
  kind: string;
  title: string;
  detail: string;
  at: string;
  severity?: string;
}

export interface GwFeedPending {
  seq: number;
  version: string;
  publishedAt?: string;
  at?: string;
}

export interface GwFeed {
  enabled: boolean;
  url: string;
  channel: 'stable' | 'beta' | string;
  /** auto | notify | pinned */
  policy: 'auto' | 'notify' | 'pinned' | string;
  configured: boolean;
  hasPubkey: boolean;
  checkMinutes: number;
  seq: number;
  version: string;
  /** 最近一次应用的快照被服务端标注的档位（free / pro），纯展示 */
  servedTier?: string;
  /** 本次同步实际应用了哪些字段包（quota.intel 之类）。服务端按 feat 挑的，不是本机判档 */
  servedFeats?: string[];
  /** 服务端列了但没应用的字段包与原因（不认识的、摘要不符的） */
  featsSkipped?: string[];
  lastCheckAt?: string;
  lastOkAt?: string;
  lastError?: string;
  /** 安全阀触发原因：需要人工确认，与 lastError 不是一回事 */
  halted?: string;
  pending?: GwFeedPending;
  events: GwFeedEvent[];
  appliedAt?: string;
  rejected: number;
  tombstones: string[];
}

export interface FeedPatch {
  enabled?: boolean;
  url?: string;
  channel?: string;
  policy?: string;
  pubkey?: string;
  checkMinutes?: number;
}

export interface GwPatrol {
  enabled: boolean;
  hotHours: number;
  coldHours: number;
  sweepAt: string;
  budgetToday: number;
  usedToday: number;
  remaining: number;
  dueNow: number;
  lastRunAt?: string;
  lastSweepAt?: string;
  probesTotal: number;
  probesOk: number;
  inflight: number;
}

export interface GwDigest {
  at: string;
  headline: string;
  added: number;
  removed: number;
  recovered: number;
  probeUsed: number;
  probeBudget: number;
  health: {
    healthy: number;
    degraded: number;
    cooling: number;
    unavailable: number;
    retired: number;
    unknown: number;
  };
  /** 按用途池分的请求战绩。样本是内存日志环，条数见 logScanned。 */
  pools: { purpose: string; requests: number; errors: number; okRate?: number }[];
  /** 这次池统计实际看了几条日志——别把比例读成全天总量 */
  logScanned: number;
  changes: { kind: string; title: string; detail?: string }[];
  attention: string[];
}

/** 一维分桶：请求数与成功请求的平均耗时。 */
export interface GwBucket {
  /** 厂商 id 或用途名 */
  key: string;
  requests: number;
  avgOkMs: number;
}

/**
 * 近期观测（默认 24h）。样本是内存日志环（≤300 条），
 * 与 `GwStatus` 里的累计计数不是一个口径——一个是全生命周期，一个是近期样本。
 */
export interface GwObserved {
  sinceMs: number;
  requests: number;
  ok: number;
  errors: number;
  /** 只统计成功请求：90s 的超时会把分位数带偏 */
  p50Ms: number | null;
  p95Ms: number | null;
  maxMs: number | null;
  tokensIn: number;
  tokensOut: number;
  /** 按目录价折算的美元；目录里没价格的走 costUnknown */
  costUsd: number;
  /** 折不出钱的请求数：这是「不知道」，不是 0 */
  costUnknown: number;
  byProvider: GwBucket[];
  byPurpose: GwBucket[];
}

export interface PatrolReport {
  ran: number;
  ok: number;
  failed: number;
  skipped?: string;
}

export interface PatrolPatch {
  enabled?: boolean;
  hotHours?: number;
  coldHours?: number;
  sweepAt?: string;
  budgetCap?: number;
}

/** 单个额度窗口。limit = 0 表示没配限额，只记账不拦。 */
export interface GwWindow {
  kind: 'rpm' | 'rpd' | 'tpm' | 'tpd';
  usedUser: number;
  usedProbe: number;
  limit: number;
  resetAt: number;
  remaining: number;
  exhausted: boolean;
}

export interface GwQuota {
  provider: string;
  name: string;
  /** 这条窗口属于哪条凭据。额度按凭据算，所以一个厂商可能出多行。 */
  accountId: string;
  accountName: string;
  windows: GwWindow[];
  /** 任一窗口耗尽时的恢复时刻（ms） */
  resetAt?: number;
  /** 最紧的那个窗口的余量比例 */
  headroom: number;
}

/** 虚拟 key。永远不含 hash 与明文。 */
export interface GwKey {
  id: string;
  name: string;
  prefix: string;
  rpmLimit?: number;
  dailyLimit?: number;
  /** 每天最多多少 token（含输入输出）；不给 = 不限 */
  dailyTokenLimit?: number;
  /**
   * 模型白名单。支持三种写法：`*`、`provider:model`、`前缀*`。
   * 空数组 = 不限。只对显式点名的模型生效，`auto` 由路由之后再过滤。
   */
  allowedModels: string[];
  /** 到期时间（RFC3339）。过了就 401，不用手动停用 */
  expiresAt?: string;
  enabled: boolean;
  createdAt: string;
  lastUsedAt?: string;
  usedToday: number;
  usedTokensToday: number;
}

export interface KeyInput {
  name: string;
  rpmLimit?: number;
  dailyLimit?: number;
  dailyTokenLimit?: number;
  allowedModels?: string[];
  expiresAt?: string;
}

/** 新建结果：明文只在这里出现一次。 */
export interface KeyCreated {
  key: GwKey;
  secret: string;
}

/** Agent 注入状态。`state` 由后端判定，前端不做推断。 */
export interface AgentInfo {
  id: string;
  name: string;
  protocol: 'openai' | 'anthropic' | 'gemini';
  /** self | pull | push | host —— 模型表从哪来 */
  discovery: 'self' | 'pull' | 'push' | 'host';
  strategy: 'json' | 'toml' | 'push' | 'env' | 'acp' | 'desktop' | 'guide';
  /** injected | detected | missing | guide | unsupported */
  state: string;
  path?: string;
  note: string;
  backup?: string;
  appliedAt?: string;
  /**
   * 项目级落点（相对仓库根）。没有就是「只能写用户级」——
   * 后端只给客户端自己认定为个人文件的落点，key 不进会被提交的文件。
   */
  projectPath?: string;
}

/** 某个仓库里的一条项目级注入记录。 */
export interface ProjectInjection {
  id: string;
  name: string;
  path: string;
  appliedAt: string;
}

export interface AgentDiffLine {
  kind: 'ctx' | 'add' | 'del';
  no: number;
  text: string;
  managed: boolean;
}

/** 迁移码导入结果。要能回答「到底加进来了什么、跳过了什么」。 */
export interface ImportSummary {
  mode: string;
  providersAdded: number;
  providersKept: number;
  accountsAdded: number;
  accountsKept: number;
  keysAdded: number;
  keysKept: number;
  secretsWritten: number;
  secretsOrphaned: number;
  /** 因为本机墓碑（用户删过）而跳过的数量 */
  providersSkipped: number;
  accountsSkipped: number;
  /** 迁移码的生成时间与来源机器，用来提示「这是哪台机器什么时候的」 */
  exportedAt: string;
  source: string;
}

export interface FreeportBridge {
  isDesktop: true;
  platform: 'darwin' | 'win32' | 'linux';
  app: {
    info: () => Promise<Record<string, unknown>>;
    /** 读 GitHub Releases 的 latest，与本地版本比。没发过 release 时 `updateAvailable: false` */
    checkUpdate: () => Promise<UpdateCheck>;
    updaterCheck: () => Promise<UpdaterStatus>;
    updaterInstall: () => Promise<boolean>;
    /** 脱敏诊断快照：不含明文密钥、不含掩码、不含 authToken */
    diagnostics: () => Promise<Record<string, unknown>>;
    /** 存成 json 文件；null = 用户取消 */
    saveDiagnostics: () => Promise<string | null>;
    /** 启动最小化 / 退出清理注入 */
    prefs: () => Promise<DesktopPrefs>;
    setPrefs: (patch: PrefsPatch) => Promise<DesktopPrefs>;
    /** 问系统「登录时会不会启动」；用户在系统设置里手删过也如实反映 */
    autostart: () => Promise<boolean>;
    setAutostart: (enabled: boolean) => Promise<boolean>;
  };
  win: {
    minimize: () => Promise<void>;
    toggleMaximize: () => Promise<boolean>;
    close: () => Promise<void>;
    toggleDevTools: () => Promise<void>;
  };
  gateway: {
    status: () => Promise<GwStatus>;
    /** 暂停 / 恢复业务转发（运行时开关，不落盘） */
    pause: (paused: boolean) => Promise<GwStatus>;
    logs: () => Promise<GwLog[]>;
    /** 每日摘要：一行结论 + 最多三条变更 + 需要处理的事 */
    digest: () => Promise<GwDigest>;
    /** 近 24h 观测：p50/p95、token、折算美元 */
    observed: () => Promise<GwObserved>;
    providers: () => Promise<GwProvider[]>;
    models: () => Promise<GwModel[]>;
    routing: () => Promise<GwRouting | null>;
    /** 走一遍真实的 Gate → Order → Pick，返回候选链与拦下原因 */
    route: (
      model?: string,
      session?: string,
      purpose?: AccountPurpose,
    ) => Promise<GwRoute>;
    presets: () => Promise<ProviderPreset[]>;

    addProvider: (payload: ProviderInput) => Promise<{ provider: GwProvider; modelCount: number }>;
    removeProvider: (id: string) => Promise<GwProvider[]>;
    toggleProvider: (id: string, enabled: boolean) => Promise<GwProvider[]>;
    /** 厂商级代理；空串 = 清掉，回落到环境变量 */
    setProviderProxy: (id: string, proxy: string) => Promise<GwProvider[]>;
    probe: (id: string) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;
    /** 明文 key 只给一条凭据看一次；`id` 是凭据 id，不是厂商 id */
    revealKey: (id: string) => Promise<string>;

    /** 给已有厂商再加一条 key。额度、冷却、账本都从这条凭据重新起算。 */
    addAccount: (input: AccountInput) => Promise<GwProvider[]>;
    /** 删凭据：密文与四个窗口一起清 */
    removeAccount: (providerId: string, accountId: string) => Promise<GwProvider[]>;
    patchAccount: (
      providerId: string,
      accountId: string,
      patch: AccountPatch,
    ) => Promise<GwProvider[]>;
    /** 发起设备码登录：用户在自己浏览器里授权，我们只等结果 */
    loginStart: (providerId: string) => Promise<GwLoginStart>;
    /** 轮询登录状态。返回 pending 就继续等，别狂刷 */
    loginPoll: (loginId: string) => Promise<GwLoginState>;
    /**
     * 等订阅授权（授权码 + PKCE）的回调。会阻塞到收到回调或超时；
     * 超时返回 `pending`，可以再调一次。
     */
    loginWait: (loginId: string, waitSecs?: number) => Promise<GwLoginState>;
    /** 用户把浏览器地址栏那一整条粘回来完成授权 */
    loginComplete: (loginId: string, callback: string) => Promise<GwLoginState>;

    /** 用粘贴的 web 会话新建凭据（只给没有公开 API 的站点） */
    addCookieAccount: (
      providerId: string,
      name: string | undefined,
      cookie: string,
    ) => Promise<GwProvider[]>;
    /** 会话过期后重新粘贴 */
    setAccountCookie: (
      providerId: string,
      accountId: string,
      cookie: string,
    ) => Promise<GwProvider[]>;

    /** 只校验这一条凭据，结果写回它自己的 health */
    probeAccount: (
      providerId: string,
      accountId: string,
    ) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;

    /** 生成迁移码：口令派生的 AES-256-GCM 信封，里面含明文密钥，只在内存里过一遍 */
    exportBundle: (passphrase: string) => Promise<string>;
    /** 解迁移码并合并。`mode=merge` 只补缺失，`replace` 整体替换并清掉本地多余的密文 */
    importBundle: (
      blob: string,
      passphrase: string,
      mode?: 'merge' | 'replace',
    ) => Promise<ImportSummary>;
    /** 导出成 .fpk 文件（原生保存对话框）。返回保存路径，`null` = 用户取消 */
    exportBundleFile: (passphrase: string) => Promise<string | null>;
    /** 从 .fpk 文件导入（原生打开对话框）。`null` = 用户取消 */
    importBundleFile: (
      passphrase: string,
      mode?: 'merge' | 'replace',
    ) => Promise<ImportSummary | null>;

    /** 本机额度情报（Pro）。非 Pro 返回 pro=false + 空 models。 */
    intel: () => Promise<GwIntel>;
    /** 用量报表（Pro）。days 夹在 1..=90。 */
    reports: (days: number) => Promise<GwReports>;
    /** 导出留存日志（原生保存对话框）。null = 用户取消 */
    reportsExport: () => Promise<string | null>;
    /** Pro 功能开关与本机数据。 */
    pro: () => Promise<GwPro>;
    setPro: (patch: ProPatch) => Promise<GwPro>;
    /** 手动打一份快照。失败（未开启 / 没配目录）会给能行动的报错 */
    backupNow: () => Promise<string>;
    backupList: () => Promise<GwBackupFile[]>;
    backupRestore: (name: string) => Promise<ImportSummary>;
    /** 策略档写入（Pro）。整批校验。 */
    setProfiles: (profiles: StrategyProfile[]) => Promise<GwRouting>;
    /** 远程接入（Pro）：切换回环 / 0.0.0.0 监听。重启网关后生效。 */
    setRemote: (on: boolean) => Promise<GwStatus>;

    setRouting: (patch: Partial<GwRouting>) => Promise<GwRouting>;
    clearCooldowns: () => Promise<boolean>;
    setConfig: (patch: ConfigPatch) => Promise<GwStatus>;
    /** 自动更新的下载/安装进度。应用重启后监听自然失效 */
    onUpdaterProgress: (
      cb: (p: { phase: string; downloaded?: number; total?: number | null }) => void,
    ) => () => void;

    catalog: () => Promise<GwCatalog>;
    catalogRefresh: () => Promise<GwCatalog>;
    probeModel: (provider: string, model: string, deep?: boolean) => Promise<GwModelProbe>;
    probeFailed: () => Promise<PatrolReport>;
    patrol: () => Promise<GwPatrol>;
    patrolPass: (all?: boolean) => Promise<PatrolReport>;
    setPatrol: (patch: PatrolPatch) => Promise<GwPatrol>;

    feed: () => Promise<GwFeed>;
    setFeed: (patch: FeedPatch) => Promise<GwFeed>;
    /** 立即检查一次（忽略轮询间隔） */
    feedCheck: () => Promise<GwFeed>;
    /** 用户确认后应用待定版本，跳过安全阀 */
    feedApply: () => Promise<GwFeed>;

    quota: () => Promise<GwQuota[]>;
    /** 问厂商「这个账号还剩多少」，只有 OAuth 账号有意义 */
    usage: (accountId: string) => Promise<GwUsage>;
    keys: () => Promise<GwKey[]>;
    createKey: (input: KeyInput) => Promise<KeyCreated>;
    deleteKey: (id: string) => Promise<GwKey[]>;
    toggleKey: (id: string, enabled: boolean) => Promise<GwKey[]>;

    agents: () => Promise<AgentInfo[]>;
    agentPreview: (id: string) => Promise<AgentDiffLine[]>;
    agentApply: (id: string) => Promise<AgentInfo>;
    agentRevert: (id: string) => Promise<AgentInfo>;

    /** 这个仓库里已经注入了哪些 Agent */
    agentProjectList: (root: string) => Promise<ProjectInjection[]>;
    agentProjectPreview: (id: string, root: string) => Promise<AgentDiffLine[]>;
    agentProjectApply: (id: string, root: string) => Promise<AgentInfo>;
    agentProjectRevert: (id: string, root: string) => Promise<AgentInfo>;

    /** 扩展清单（已含扩展目录验签结果与运行状态；端口不外露） */
    extList: () => Promise<GwExt[]>;
    /** 启用前会再校验一次上游域名是否在 manifest 授权清单里 */
    extSetConfig: (id: string, enabled: boolean, baseUrl: string, model: string) => Promise<GwExt[]>;
    /** 扩展自己的凭据：进密钥库，配置里只留掩码 */
    extSetKey: (id: string, key: string) => Promise<GwExt[]>;
    /** 手动停 sidecar；下个请求会按需再拉起 */
    extStop: (id: string) => Promise<GwExt[]>;

    onLog: (cb: (entry: GwLog) => void) => () => void;
  };
  shell: { openExternal: (url: string) => Promise<boolean> };
}

declare global {
  interface Window {
    freeport?: FreeportBridge;
  }
}
