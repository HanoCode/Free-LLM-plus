/**
 * 网关状态桥：桌面端从主进程拉真实数据，浏览器里自动退回空态。
 * 单一数据源，组件只读这里，不各自轮询。
 */

import { create } from 'zustand';
import type { GwLog, GwModel, GwProvider, GwRouting, GwStatus, ProviderInput, ProviderPreset } from './bridge';

interface GwState {
  available: boolean;
  status: GwStatus | null;
  providers: GwProvider[];
  models: GwModel[];
  logs: GwLog[];
  routing: GwRouting | null;
  presets: ProviderPreset[];
  loading: boolean;
  error: string | null;

  refresh: () => Promise<void>;
  addProvider: (p: ProviderInput) => Promise<{ modelCount: number }>;
  removeProvider: (id: string) => Promise<void>;
  toggleProvider: (id: string, enabled: boolean) => Promise<void>;
  probe: (id: string) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;
  clearCooldowns: () => Promise<void>;
  setRequireAuth: (v: boolean) => Promise<void>;
}

const bridge = () => (typeof window !== 'undefined' ? window.freeport?.gateway : undefined);

export const useGateway = create<GwState>((set, get) => ({
  available: !!bridge(),
  status: null,
  providers: [],
  models: [],
  logs: [],
  routing: null,
  presets: [],
  loading: false,
  error: null,

  async refresh() {
    const gw = bridge();
    if (!gw) return;
    try {
      const [status, providers, models, logs, routing, presets] = await Promise.all([
        gw.status(),
        gw.providers(),
        gw.models(),
        gw.logs(),
        gw.routing(),
        gw.presets(),
      ]);
      set({ status, providers, models, logs, routing, presets, error: null });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },

  async addProvider(p) {
    const gw = bridge();
    if (!gw) throw new Error('仅桌面端可用');
    const res = await gw.addProvider(p);
    await get().refresh();
    return { modelCount: res.modelCount };
  },

  async removeProvider(id) {
    const gw = bridge();
    if (!gw) return;
    await gw.removeProvider(id);
    await get().refresh();
  },

  async toggleProvider(id, enabled) {
    const gw = bridge();
    if (!gw) return;
    await gw.toggleProvider(id, enabled);
    await get().refresh();
  },

  async probe(id) {
    const gw = bridge();
    if (!gw) return { ok: false, error: '仅桌面端可用' };
    const res = await gw.probe(id);
    await get().refresh();
    return res;
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
}));

/** 应用启动时调用一次：拉一轮 + 订阅实时日志 */
export function initGateway(): () => void {
  const gw = bridge();
  if (!gw) return () => undefined;

  void useGateway.getState().refresh();

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
