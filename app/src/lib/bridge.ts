/** 桌面宿主 ↔ 渲染层的类型约定（`window.freeport` 门面） */

export interface GwCooldown {
  key: string;
  until: number;
  reason: string;
  hits: number;
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

export type LicenseStatus = 'missing' | 'valid' | 'grace' | 'expired' | 'invalid' | 'wrong_machine';
/**
 * 许可证状态。
 *
 * `tier` 是生效档位（过期就是 free），`licensedTier` 是买过的档位——
 * 两个都拿才能说清「你买的是 Pro，已于 10-01 到期，现在是免费版」。
 */
export interface GwLicense {
  status: LicenseStatus;
  tier: 'free' | 'pro';
  licensedTier: 'free' | 'pro';
  pro: boolean;
  subject?: string;
  features: string[];
  /** unix 秒 */
  expiresAt?: number;
  graceUntil?: number;
  daysLeft?: number;
  seats?: number;
  /** 本机安装码。绑机器的 license 用它核对。 */
  machineId: string;
  message?: string;
}

export type HealthStatus =
  | 'unknown'
  | 'probing'
  | 'healthy'
  | 'degraded'
  | 'cooling'
  | 'unavailable'
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
}

/** 发起登录后要展示给用户的信息 */
export interface GwLoginStart {
  loginId: string;
  userCode: string;
  verificationUri: string;
  verificationUriComplete?: string;
  interval: number;
  expiresIn: number;
  /** 「在浏览器打开 X 并输入 Y」，界面直接显示这句 */
  instruction: string;
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
  error?: string;
}

export interface GwRouting {
  strategy: string;
  chain: string[];
  cooldowns: GwCooldown[];
}

/** 权重三因子：档位 × 余量 × 健康。界面上逐个摊开，不做黑箱打分。 */
export interface RouteFactors {
  tier: number;
  headroom: number;
  health: number;
}

export interface GwRouteHop {
  provider: string;
  model: string;
  tier: string;
  weight: number;
  remaining: number;
  health: HealthStatus;
  factors: RouteFactors;
}

/** kind 用于分区展示：quota / health / cooldown */
export interface GwRouteReject {
  provider: string;
  model: string;
  reason: string;
  kind: string;
}

/** `/v1/route` 的结构化版本：这一秒真的会按 `chain` 的顺序发出去。 */
export interface GwRoute {
  model: string;
  session: string;
  sticky: boolean;
  strategy: string;
  budgetMs: number;
  chain: GwRouteHop[];
  rejected: GwRouteReject[];
  cooldowns: GwCooldown[];
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
}

/** 改一条凭据。`clearQuota` 用来把限额清空（`undefined` 表示不动）。 */
export interface AccountPatch {
  name?: string;
  enabled?: boolean;
  quota?: ProviderQuotaInput;
  clearQuota?: boolean;
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
  enabled: boolean;
  createdAt: string;
  lastUsedAt?: string;
  usedToday: number;
}

export interface KeyInput {
  name: string;
  rpmLimit?: number;
  dailyLimit?: number;
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
    /** 脱敏诊断快照：不含明文密钥、不含掩码、不含 authToken */
    diagnostics: () => Promise<Record<string, unknown>>;
    /** 存成 json 文件；null = 用户取消 */
    saveDiagnostics: () => Promise<string | null>;
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
    providers: () => Promise<GwProvider[]>;
    models: () => Promise<GwModel[]>;
    routing: () => Promise<GwRouting | null>;
    /** 走一遍真实的 Gate → Order → Pick，返回候选链与拦下原因 */
    route: (model?: string, session?: string) => Promise<GwRoute>;
    presets: () => Promise<ProviderPreset[]>;

    addProvider: (payload: ProviderInput) => Promise<{ provider: GwProvider; modelCount: number }>;
    removeProvider: (id: string) => Promise<GwProvider[]>;
    toggleProvider: (id: string, enabled: boolean) => Promise<GwProvider[]>;
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

    /** 许可证状态。离线验签，不联网。 */
    license: () => Promise<GwLicense>;
    /** 激活：签名不对 / 绑了别的机器 / 已过宽限期都会被拒，且不落盘 */
    activateLicense: (token: string) => Promise<GwLicense>;
    /** 解除激活，回到免费版（转发不受影响） */
    clearLicense: () => Promise<GwLicense>;

    setRouting: (patch: Partial<GwRouting>) => Promise<GwRouting>;
    clearCooldowns: () => Promise<boolean>;
    setConfig: (patch: {
      requireAuth?: boolean;
      port?: number;
      requestBudgetMs?: number;
    }) => Promise<GwStatus>;

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

    onLog: (cb: (entry: GwLog) => void) => () => void;
  };
  shell: { openExternal: (url: string) => Promise<boolean> };
}

declare global {
  interface Window {
    freeport?: FreeportBridge;
  }
}
