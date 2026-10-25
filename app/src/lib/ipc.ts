/**
 * Tauri IPC 适配层。
 *
 * 把 `invoke` 包装成 `window.freeport` 这一稳定的门面对象，页面与 store
 * 只依赖门面，不直接接触 Tauri API。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import type {
  AgentDiffLine,
  AgentInfo,
  DesktopPrefs,
  FreeportBridge,
  GwBackupFile,
  GwCatalog,
  GwDigest,
  GwExt,
  GwFeed,
  GwIntel,
  GwKey,
  GwLoginStart,
  GwLoginState,
  GwLog,
  GwModel,
  GwModelProbe,
  GwObserved,
  GwPatrol,
  GwPro,
  GwProvider,
  GwQuota,
  GwReports,
  GwRoute,
  GwRouting,
  GwStatus,
  GwUsage,
  ImportSummary,
  KeyCreated,
  PatrolReport,
  ProjectInjection,
  ProviderPreset,
  UpdateCheck,
  UpdaterStatus,
} from './bridge';

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function guessPlatform(): string {
  const ua = navigator.userAgent;
  if (ua.includes('Mac')) return 'darwin';
  if (ua.includes('Win')) return 'win32';
  return 'linux';
}

/** 把 invoke 包成与 preload 一致的调用面 */
export function installTauriBridge(): boolean {
  if (!isTauri()) return false;
  if (window.freeport) return true;

  const bridge: FreeportBridge = {
    // 页面里的 `isDesktop` 只看这个布尔
    isDesktop: true,
    platform: guessPlatform() as FreeportBridge['platform'],

    app: {
      info: () => invoke<Record<string, unknown>>('app_info'),
      checkUpdate: () => invoke<UpdateCheck>('check_update'),
      updaterCheck: () => invoke<UpdaterStatus>('updater_check'),
      updaterInstall: () => invoke<boolean>('updater_install'),
      diagnostics: () => invoke<Record<string, unknown>>('gw_diagnostics'),
      saveDiagnostics: () => invoke<string | null>('gw_save_diagnostics'),
      prefs: () => invoke<DesktopPrefs>('win_prefs'),
      setPrefs: (patch) => invoke<DesktopPrefs>('win_set_prefs', { patch }),
      autostart: () => invoke<boolean>('win_autostart_get'),
      setAutostart: (enabled) => invoke<boolean>('win_autostart_set', { enabled }),
    },

    win: {
      minimize: () => invoke<void>('win_minimize'),
      toggleMaximize: () => invoke<boolean>('win_toggle_maximize'),
      close: () => invoke<void>('win_close'),
      toggleDevTools: () => invoke<void>('win_toggle_devtools'),
    },

    gateway: {
      status: () => invoke<GwStatus>('gw_status'),
      pause: (paused) => invoke<GwStatus>('gw_pause', { paused }),
      logs: () => invoke<GwLog[]>('gw_logs'),
      digest: () => invoke<GwDigest>('gw_digest'),
      observed: () => invoke<GwObserved>('gw_window'),
      providers: () => invoke<GwProvider[]>('gw_providers'),
      models: () => invoke<GwModel[]>('gw_models'),
      routing: () => invoke<GwRouting | null>('gw_routing'),
      route: (model, session, purpose) =>
        invoke<GwRoute>('gw_route', {
          model: model ?? 'auto',
          session: session ?? '',
          purpose: purpose ?? 'chat',
        }),
      presets: () => invoke<ProviderPreset[]>('gw_presets'),

      addProvider: (payload) =>
        invoke<{ provider: GwProvider; modelCount: number }>('gw_add_provider', { input: payload }),
      removeProvider: (id) => invoke<GwProvider[]>('gw_remove_provider', { id }),
      toggleProvider: (id, enabled) => invoke<GwProvider[]>('gw_toggle_provider', { id, enabled }),
      setProviderProxy: (id, proxy) =>
        invoke<GwProvider[]>('gw_set_provider_proxy', { id, proxy }),
      probe: (id) =>
        invoke<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>('gw_probe', { id }),
      revealKey: (id) => invoke<string>('gw_reveal_key', { id }),

      addAccount: (input) => invoke<GwProvider[]>('gw_add_account', { input }),
      loginStart: (providerId) => invoke<GwLoginStart>('gw_login_start', { providerId }),
      loginPoll: (loginId) => invoke<GwLoginState>('gw_login_poll', { loginId }),
      loginWait: (loginId, waitSecs) =>
        invoke<GwLoginState>('gw_login_wait', { loginId, waitSecs }),
      loginComplete: (loginId, callback) =>
        invoke<GwLoginState>('gw_login_complete', { loginId, callback }),
      addCookieAccount: (providerId, name, cookie) =>
        invoke<GwProvider[]>('gw_add_cookie_account', { providerId, name, cookie }),
      setAccountCookie: (providerId, accountId, cookie) =>
        invoke<GwProvider[]>('gw_set_account_cookie', { providerId, accountId, cookie }),
      removeAccount: (providerId, accountId) =>
        invoke<GwProvider[]>('gw_remove_account', { providerId, accountId }),
      patchAccount: (providerId, accountId, patch) =>
        invoke<GwProvider[]>('gw_patch_account', { providerId, accountId, patch }),
      probeAccount: (providerId, accountId) =>
        invoke<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>(
          'gw_probe_account',
          { id: providerId, accountId },
        ),

      exportBundle: (passphrase) => invoke<string>('gw_export_bundle', { passphrase }),
      exportBundleFile: (passphrase) =>
        invoke<string | null>('gw_export_bundle_file', { passphrase }),
      importBundleFile: (passphrase, mode) =>
        invoke<ImportSummary | null>('gw_import_bundle_file', { passphrase, mode }),
      importBundle: (blob, passphrase, mode) =>
        invoke<ImportSummary>('gw_import_bundle', { blob, passphrase, mode }),

      intel: () => invoke<GwIntel>('gw_intel'),
      reports: (days) => invoke<GwReports>('gw_reports', { days: days ?? 30 }),
      reportsExport: () => invoke<string | null>('gw_reports_export_file'),
      pro: () => invoke<GwPro>('gw_pro'),
      setPro: (patch) => invoke<GwPro>('gw_set_pro', { patch }),
      backupNow: () => invoke<string>('gw_backup_now'),
      backupList: () => invoke<GwBackupFile[]>('gw_backup_list'),
      backupRestore: (name) => invoke<ImportSummary>('gw_backup_restore', { name }),
      setProfiles: (profiles) => invoke<GwRouting>('gw_set_profiles', { profiles }),
      setRemote: (on) => invoke<GwStatus>('gw_set_remote', { on }),

      setRouting: (patch) => invoke<GwRouting>('gw_set_routing', { patch }),
      clearCooldowns: () => invoke<boolean>('gw_clear_cooldowns'),
      setConfig: (patch) => invoke<GwStatus>('gw_set_config', { patch }),

      catalog: () => invoke<GwCatalog>('gw_catalog'),
      catalogRefresh: () => invoke<GwCatalog>('gw_catalog_refresh'),
      probeModel: (provider, model, deep) =>
        invoke<GwModelProbe>('gw_probe_model', { provider, model, deep: deep ?? false }),
      probeFailed: () => invoke<PatrolReport>('gw_probe_failed'),
      patrol: () => invoke<GwPatrol>('gw_patrol'),
      patrolPass: (all) => invoke<PatrolReport>('gw_patrol_pass', { all: all ?? false }),
      setPatrol: (patch) => invoke<GwPatrol>('gw_set_patrol', { patch }),

      feed: () => invoke<GwFeed>('gw_feed'),
      setFeed: (patch) => invoke<GwFeed>('gw_set_feed', { patch }),
      feedCheck: () => invoke<GwFeed>('gw_feed_check', { manual: true }),
      feedApply: () => invoke<GwFeed>('gw_feed_apply'),

      quota: () => invoke<GwQuota[]>('gw_quota'),
      usage: (accountId) => invoke<GwUsage>('gw_usage', { accountId }),
      keys: () => invoke<GwKey[]>('gw_keys'),
      createKey: (input) => invoke<KeyCreated>('gw_create_key', { input }),
      deleteKey: (id) => invoke<GwKey[]>('gw_delete_key', { id }),
      toggleKey: (id, enabled) => invoke<GwKey[]>('gw_toggle_key', { id, enabled }),

      agents: () => invoke<AgentInfo[]>('gw_agents'),
      agentPreview: (id) => invoke<AgentDiffLine[]>('gw_agent_preview', { id }),
      agentApply: (id) => invoke<AgentInfo>('gw_agent_apply', { id }),
      agentRevert: (id) => invoke<AgentInfo>('gw_agent_revert', { id }),

      agentProjectList: (root) => invoke<ProjectInjection[]>('gw_agent_project_list', { root }),
      agentProjectPreview: (id, root) =>
        invoke<AgentDiffLine[]>('gw_agent_project_preview', { id, root }),
      agentProjectApply: (id, root) => invoke<AgentInfo>('gw_agent_project_apply', { id, root }),
      agentProjectRevert: (id, root) => invoke<AgentInfo>('gw_agent_project_revert', { id, root }),

      extList: () => invoke<GwExt[]>('gw_ext_list'),
      extSetConfig: (id, enabled, baseUrl, model) =>
        invoke<GwExt[]>('gw_ext_set_config', { id, enabled, baseUrl, model }),
      extSetKey: (id, key) => invoke<GwExt[]>('gw_ext_set_key', { id, key }),
      extStop: (id) => invoke<GwExt[]>('gw_ext_stop', { id }),

      onUpdaterProgress: (cb) => {
        let dispose: (() => void) | null = null;
        let cancelled = false;
        void listen<{ phase: string; downloaded?: number; total?: number | null }>(
          'updater://progress',
          (event) => cb(event.payload),
        ).then((un) => {
          if (cancelled) un();
          else dispose = un;
        });
        return () => {
          cancelled = true;
          dispose?.();
        };
      },
      onLog: (cb) => {
        let dispose: (() => void) | null = null;
        let cancelled = false;
        void listen<GwLog>('gateway:log', (event) => cb(event.payload)).then((un) => {
          if (cancelled) un();
          else dispose = un;
        });
        return () => {
          cancelled = true;
          dispose?.();
        };
      },
    },

    shell: {
      openExternal: (url) => invoke<boolean>('open_external', { url }),
    },
  };

  window.freeport = bridge;
  return true;
}

/**
 * 模块加载即装桥，不等调用方。
 *
 * 为什么必须自举：`gateway.ts` 的 store 在**模块求值那一刻**就读
 * `window.freeport` 判断「有没有桌面端」。ESM 的求值顺序跟着 import 走，
 * 只要有任何模块比安装动作先求值，桌面端就会把自己当成浏览器：
 * 界面照着兜底数据渲染，网关明明在跑也显示「未启动」。这种错不抛异常，
 * 只是安静地显示错的东西，最难查。
 */
installTauriBridge();
