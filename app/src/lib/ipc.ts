/**
 * Tauri IPC 适配层。
 *
 * 把 `invoke` 包装成 `window.freeport` 这一稳定的门面对象，页面与 store
 * 只依赖门面，不直接接触 Tauri API。
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import type {
  FreeportBridge,
  GwLog,
  GwModel,
  GwProvider,
  GwRouting,
  GwStatus,
  ProviderPreset,
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
    },

    win: {
      minimize: () => invoke<void>('win_minimize'),
      toggleMaximize: () => invoke<boolean>('win_toggle_maximize'),
      close: () => invoke<void>('win_close'),
      toggleDevTools: () => invoke<void>('win_toggle_devtools'),
    },

    gateway: {
      status: () => invoke<GwStatus>('gw_status'),
      logs: () => invoke<GwLog[]>('gw_logs'),
      providers: () => invoke<GwProvider[]>('gw_providers'),
      models: () => invoke<GwModel[]>('gw_models'),
      routing: () => invoke<GwRouting | null>('gw_routing'),
      presets: () => invoke<ProviderPreset[]>('gw_presets'),

      addProvider: (payload) =>
        invoke<{ provider: GwProvider; modelCount: number }>('gw_add_provider', { input: payload }),
      removeProvider: (id) => invoke<GwProvider[]>('gw_remove_provider', { id }),
      toggleProvider: (id, enabled) => invoke<GwProvider[]>('gw_toggle_provider', { id, enabled }),
      probe: (id) =>
        invoke<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>('gw_probe', { id }),
      revealKey: (id) => invoke<string>('gw_reveal_key', { id }),

      setRouting: (patch) => invoke<GwRouting>('gw_set_routing', { patch }),
      clearCooldowns: () => invoke<boolean>('gw_clear_cooldowns'),
      setConfig: (patch) => invoke<GwStatus>('gw_set_config', { patch }),

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
