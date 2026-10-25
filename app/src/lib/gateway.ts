/**
 * 网关状态桥：桌面端从宿主拉真实数据，浏览器里自动退回空态。
 * 单一数据源，组件只读这里，不各自轮询。
 *
 * 轮询分两档：
 * - 8 秒：状态、厂商、模型、日志、巡检（都很小）
 * - 按需：目录（几百个厂商，只在打开「发现」页和手动刷新时拉）
 */

import { create } from 'zustand';
// 纯副作用导入：保证桥在本文件求值前已装好（store 的 available 是模块级常量）
import './ipc';
import type {
  AccountInput,
  AccountPatch,
  AccountPurpose,
  AgentInfo,
  ConfigPatch,
  GwCatalog,
  GwFeed,
  GwLoginStart,
  GwLoginState,
  GwLog,
  GwModel,
  GwModelProbe,
  GwPatrol,
  GwProvider,
  GwQuota,
  GwRoute,
  GwRouting,
  GwStatus,
  GwUsage,
  FeedPatch,
  ImportSummary,
  PatrolPatch,
  PatrolReport,
  GwDigest,
  GwObserved,
  GwIntel,
  GwPro,
  GwReports,
  GwBackupFile,
  ProPatch,
  StrategyProfile,
  ProviderInput,
  ProviderPreset,
} from './bridge';

interface GwState {
  available: boolean;
  status: GwStatus | null;
  providers: GwProvider[];
  models: GwModel[];
  logs: GwLog[];
  /** 每日摘要：一行结论 + 三件事，概览页顶部 */
  digest: GwDigest | null;
  /** 近 24h 观测：p50/p95、token、折算美元。与 inspect 的 window 段同源 */
  observed: GwObserved | null;
  routing: GwRouting | null;
  presets: ProviderPreset[];
  catalog: GwCatalog | null;
  patrol: GwPatrol | null;
  feed: GwFeed | null;
  quota: GwQuota[];
  agents: AgentInfo[];
  /** 许可证状态。启动时拉一次，激活/解除后更新——不需要进 8 秒轮询 */
  /** 路由解释：只在「路由」页按需拉，不进 8 秒轮询（会消耗 SWRR 轮转预算） */
  route: GwRoute | null;
  /** 本机情报（Pro）：报表页按需拉，不进轮询 */
  intel: GwIntel | null;
  /** 用量报表（Pro）：报表页按需拉 */
  reports: GwReports | null;
  /** Pro 功能开关（设置页按需拉） */
  pro: GwPro | null;
  loading: boolean;
  error: string | null;

  refresh: () => Promise<void>;
  explainRoute: (
    model?: string,
    session?: string,
    purpose?: AccountPurpose,
  ) => Promise<GwRoute | null>;
  loadCatalog: () => Promise<void>;
  refreshCatalog: () => Promise<GwCatalog>;
  addProvider: (p: ProviderInput) => Promise<{ modelCount: number }>;
  removeProvider: (id: string) => Promise<void>;
  toggleProvider: (id: string, enabled: boolean) => Promise<void>;
  setProviderProxy: (id: string, proxy: string) => Promise<void>;
  addAccount: (input: AccountInput) => Promise<GwProvider[]>;
  /** 发起设备码登录，返回要展示给用户的 code 与地址 */
  loginStart: (providerId: string) => Promise<GwLoginStart>;
  /** 轮询登录状态；ready 之后凭据已经落库，直接 refresh 即可看到 */
  loginPoll: (loginId: string) => Promise<GwLoginState>;
  loginWait: (loginId: string, waitSecs?: number) => Promise<GwLoginState>;
  loginComplete: (loginId: string, callback: string) => Promise<GwLoginState>;
  /** 用粘贴的 web 会话新建凭据 */
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
  removeAccount: (providerId: string, accountId: string) => Promise<GwProvider[]>;
  patchAccount: (
    providerId: string,
    accountId: string,
    patch: AccountPatch,
  ) => Promise<GwProvider[]>;
  probe: (id: string) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;
  probeAccount: (
    providerId: string,
    accountId: string,
  ) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;
  exportBundle: (passphrase: string) => Promise<string>;
  importBundle: (
    blob: string,
    passphrase: string,
    mode?: 'merge' | 'replace',
  ) => Promise<ImportSummary>;
  /** 存成 .fpk 文件；返回路径，null 表示用户取消 */
  exportBundleFile: (passphrase: string) => Promise<string | null>;
  importBundleFile: (
    passphrase: string,
    mode?: 'merge' | 'replace',
  ) => Promise<ImportSummary | null>;
  /* 订阅与授权管理 */
  /** 解绑失败原样上抛；成功返回是否真的解掉了一条绑定 */
  probeModel: (provider: string, model: string, deep?: boolean) => Promise<GwModelProbe>;
  probeFailed: () => Promise<PatrolReport>;
  runPatrol: (all?: boolean) => Promise<PatrolReport>;
  setPatrol: (patch: PatrolPatch) => Promise<void>;
  setFeed: (patch: FeedPatch) => Promise<void>;
  checkFeed: () => Promise<GwFeed>;
  applyFeed: () => Promise<GwFeed>;
  clearCooldowns: () => Promise<void>;
  /** 问厂商要这个账号的**官方**额度（账本是另一回事） */
  fetchUsage: (accountId: string) => Promise<GwUsage>;
  saveRouting: (patch: Partial<GwRouting>) => Promise<GwRouting>;
  /** Pro 本机功能：情报 / 报表 / 开关（报表页与设置页按需拉） */
  loadIntel: () => Promise<GwIntel | null>;
  loadReports: (days: number) => Promise<GwReports | null>;
  loadPro: () => Promise<GwPro | null>;
  updatePro: (patch: ProPatch) => Promise<GwPro>;
  backupNow: () => Promise<string>;
  backupList: () => Promise<GwBackupFile[]>;
  backupRestore: (name: string) => Promise<ImportSummary>;
  setProfiles: (profiles: StrategyProfile[]) => Promise<GwRouting>;
  /** 远程接入（Pro）：切换回环 / 0.0.0.0 监听。重启网关后生效 */
  setRemote: (on: boolean) => Promise<GwStatus>;
  patchConfig: (patch: ConfigPatch) => Promise<GwStatus>;
  setRequireAuth: (v: boolean) => Promise<void>;
  /** 单次请求总预算；落库后下一次请求生效 */
  setBudget: (ms: number) => Promise<void>;
  setCompress: (v: boolean) => Promise<void>;
  /** 暂停 / 恢复业务转发 */
  setPaused: (paused: boolean) => Promise<void>;
}

const bridge = () => (typeof window !== 'undefined' ? window.freeport?.gateway : undefined);

export const useGateway = create<GwState>((set, get) => ({
  available: !!bridge(),
  status: null,
  providers: [],
  models: [],
  logs: [],
  digest: null,
  observed: null,
  routing: null,
  presets: [],
  catalog: null,
  patrol: null,
  feed: null,
  quota: [],
  agents: [],
  route: null,
  intel: null,
  reports: null,
  pro: null,
  loading: false,
  error: null,

  async refresh() {
    const gw = bridge();
    if (!gw) return;
    try {
      const [
        status,
        providers,
        models,
        logs,
        digest,
        observed,
        routing,
        presets,
        patrol,
        feed,
        quota,
        agents,
      ] = await Promise.all([
        gw.status(),
        gw.providers(),
        gw.models(),
        gw.logs(),
        gw.digest(),
        gw.observed(),
        gw.routing(),
        gw.presets(),
        gw.patrol(),
        gw.feed(),
        gw.quota(),
        gw.agents(),
      ]);
      set({
        status,
        providers,
        models,
        logs,
        digest,
        observed,
        routing,
        presets,
        patrol,
        feed,
        quota,
        agents,
        error: null,
      });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  async explainRoute(model, session, purpose) {
    const gw = bridge();
    if (!gw) return null;
    try {
      const route = await gw.route(model, session, purpose);
      set({ route });
      return route;
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
      return null;
    }
  },

  async loadCatalog() {
    const gw = bridge();
    if (!gw) return;
    try {
      const catalog = await gw.catalog();
      set({ catalog });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  async refreshCatalog() {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const catalog = await gw.catalogRefresh();
    set({ catalog });
    await get().refresh();
    return catalog;
  },

  async addProvider(p) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const res = await gw.addProvider(p);
    await get().refresh();
    void get().loadCatalog();
    return { modelCount: res.modelCount };
  },

  async removeProvider(id) {
    const gw = bridge();
    if (!gw) return;
    await gw.removeProvider(id);
    await get().refresh();
    void get().loadCatalog();
  },

  async toggleProvider(id, enabled) {
    const gw = bridge();
    if (!gw) return;
    await gw.toggleProvider(id, enabled);
    await get().refresh();
  },

  /** 厂商级代理。只影响这一家的上游请求，目录同步仍走系统链路。 */
  async setProviderProxy(id, proxy) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.setProviderProxy(id, proxy);
    set({ providers });
  },

  // 凭据三件套：就地替换厂商列表，再整轮 refresh 拿账本。
  // 只 set providers 会出现「列表更新了、额度条还是旧账」的错位。
  async addAccount(input) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.addAccount(input);
    set({ providers });
    await get().refresh();
    return providers;
  },

  async loginStart(providerId) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    return gw.loginStart(providerId);
  },

  async loginPoll(loginId) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const state = await gw.loginPoll(loginId);
    // 登录成功会新建一条凭据，列表与额度都要跟着变
    if (state.status === 'ready') await get().refresh();
    return state;
  },

  async loginWait(loginId, waitSecs) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const state = await gw.loginWait(loginId, waitSecs);
    if (state.status === 'ready') await get().refresh();
    return state;
  },

  async loginComplete(loginId, callback) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const state = await gw.loginComplete(loginId, callback);
    if (state.status === 'ready') await get().refresh();
    return state;
  },

  async addCookieAccount(providerId, name, cookie) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.addCookieAccount(providerId, name, cookie);
    set({ providers });
    await get().refresh();
    return providers;
  },

  async setAccountCookie(providerId, accountId, cookie) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.setAccountCookie(providerId, accountId, cookie);
    set({ providers });
    await get().refresh();
    return providers;
  },

  async removeAccount(providerId, accountId) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.removeAccount(providerId, accountId);
    set({ providers });
    await get().refresh();
    return providers;
  },

  async patchAccount(providerId, accountId, patch) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const providers = await gw.patchAccount(providerId, accountId, patch);
    set({ providers });
    await get().refresh();
    return providers;
  },

  async probe(id) {
    const gw = bridge();
    if (!gw) return { ok: false, error: '仅桌面端可用' };
    const res = await gw.probe(id);
    await get().refresh();
    return res;
  },

  async probeAccount(providerId, accountId) {
    const gw = bridge();
    if (!gw) return { ok: false, error: '仅桌面端可用' };
    const res = await gw.probeAccount(providerId, accountId);
    await get().refresh();
    return res;
  },

  async exportBundle(passphrase) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    return gw.exportBundle(passphrase);
  },

  async importBundle(blob, passphrase, mode) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const sum = await gw.importBundle(blob, passphrase, mode);
    // 导入会新增厂商/凭据/虚拟 key，列表与额度都要整体重拉
    await get().refresh();
    await get().loadCatalog();
    return sum;
  },

  async exportBundleFile(passphrase) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    return gw.exportBundleFile(passphrase);
  },

  async importBundleFile(passphrase, mode) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const sum = await gw.importBundleFile(passphrase, mode);
    if (sum) {
      await get().refresh();
      await get().loadCatalog();
    }
    return sum;
  },

  async probeModel(provider, model, deep) {
    const gw = bridge();
    if (!gw) {
      return { ok: false, status: 0, latencyMs: 0, health: 'unknown' as const, error: '仅桌面端可用' };
    }
    const res = await gw.probeModel(provider, model, deep);
    await get().refresh();
    return res;
  },

  async probeFailed() {
    const gw = bridge();
    if (!gw) return { ran: 0, ok: 0, failed: 0, skipped: '仅桌面端可用' };
    const res = await gw.probeFailed();
    await get().refresh();
    return res;
  },

  async runPatrol(all) {
    const gw = bridge();
    if (!gw) return { ran: 0, ok: 0, failed: 0, skipped: '仅桌面端可用' };
    const res = await gw.patrolPass(all);
    await get().refresh();
    return res;
  },

  async setPatrol(patch) {
    const gw = bridge();
    if (!gw) return;
    const patrol = await gw.setPatrol(patch);
    set({ patrol });
  },

  async setFeed(patch) {
    const gw = bridge();
    if (!gw) return;
    const feed = await gw.setFeed(patch);
    set({ feed });
  },

  async checkFeed() {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const feed = await gw.feedCheck();
    set({ feed });
    await get().loadCatalog();
    return feed;
  },

  async applyFeed() {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const feed = await gw.feedApply();
    set({ feed });
    await get().refresh();
    void get().loadCatalog();
    return feed;
  },

  async clearCooldowns() {
    const gw = bridge();
    if (!gw) return;
    await gw.clearCooldowns();
    await get().refresh();
  },

  async setRequireAuth(v) {
    const gw = bridge();
    if (!gw) return;
    const status = await gw.setConfig({ requireAuth: v });
    set({ status });
  },

  async setBudget(ms) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const status = await gw.setConfig({ requestBudgetMs: ms });
    set({ status });
  },

  /** 工具输出压缩。默认关：开了会丢信息，得由用户自己决定。 */
  async setCompress(v) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const status = await gw.setConfig({ compress: v });
    set({ status });
  },

  async fetchUsage(accountId) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    return gw.usage(accountId);
  },

  /** 写路由配置（策略 / 链 / 命名链）。返回落盘后的完整视图。 */
  async saveRouting(patch) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const routing = await gw.setRouting(patch);
    set({ routing });
    return routing;
  },

  /* ── Pro 本机功能（v1.12）───────────────────────── */

  async loadIntel() {
    const gw = bridge();
    if (!gw) return null;
    try {
      const intel = await gw.intel();
      set({ intel });
      return intel;
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
      return null;
    }
  },

  async loadReports(days) {
    const gw = bridge();
    if (!gw) return null;
    try {
      const reports = await gw.reports(days);
      set({ reports });
      return reports;
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
      return null;
    }
  },

  async loadPro() {
    const gw = bridge();
    if (!gw) return null;
    try {
      const pro = await gw.pro();
      set({ pro });
      return pro;
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
      return null;
    }
  },

  async updatePro(patch) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const pro = await gw.setPro(patch);
    set({ pro });
    return pro;
  },

  async backupNow() {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const path = await gw.backupNow();
    set({ pro: await gw.pro() });
    return path;
  },

  async backupList() {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    return gw.backupList();
  },

  async backupRestore(name) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const summary = await gw.backupRestore(name);
    await get().refresh();
    return summary;
  },

  async setProfiles(profiles) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const routing = await gw.setProfiles(profiles);
    set({ routing });
    return routing;
  },

  /** 远程接入（Pro）：改的是监听配置，重启网关后生效 */
  async setRemote(on) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const status = await gw.setRemote(on);
    set({ status });
    return status;
  },

  /** 通用配置写入：新增开关不用每次都加一个动作 */
  async patchConfig(patch) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const status = await gw.setConfig(patch);
    set({ status });
    return status;
  },

  async setPaused(paused) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const status = await gw.pause(paused);
    set({ status });
  },
}));

/** 应用启动时调用一次：拉一轮 + 订阅实时日志 */
export function initGateway(): () => void {
  const gw = bridge();
  // 再确认一次：桥是运行时装上的，模块级那次判断不算最终答案
  useGateway.setState({ available: !!gw });
  if (!gw) return () => undefined;

  void useGateway.getState().refresh();
  void useGateway.getState().loadCatalog();

  const off = gw.onLog((entry) => {
    useGateway.setState((s) => ({ logs: [entry, ...s.logs].slice(0, 300) }));
    // 顺带刷新计数，避免为计数再开轮询
    void gw.status().then((status) => useGateway.setState({ status }));
  });

  const timer = window.setInterval(() => {
    if (document.visibilityState === 'visible') void useGateway.getState().refresh();
  }, 8000);

  return () => {
    off();
    window.clearInterval(timer);
  };
}
