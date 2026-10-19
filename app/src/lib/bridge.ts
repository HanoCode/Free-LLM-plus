/** 桌面宿主 ↔ 渲染层的类型约定（`window.freeport` 门面） */

export interface GwCooldown {
  key: string;
  until: number;
  reason: string;
  hits: number;
}

export interface GwStatus {
  running: boolean;
  port: number | null;
  bind: string;
  since: string | null;
  authToken: string;
  requireAuth: boolean;
  requests24h: number;
  tokensIn: number;
  tokensOut: number;
  errors: number;
  cooldowns: GwCooldown[];
  error?: string | null;
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
  models: number;
  health: {
    status: 'unknown' | 'probing' | 'healthy' | 'degraded' | 'error';
    latencyMs?: number;
    checkedAt?: string;
    modelCount?: number;
    error?: string;
  };
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
  };
}

export interface GwAttempt {
  provider: string;
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
  error?: string;
}

export interface GwRouting {
  strategy: string;
  chain: string[];
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

export interface FreeportBridge {
  isDesktop: true;
  platform: 'darwin' | 'win32' | 'linux';
  app: { info: () => Promise<Record<string, unknown>> };
  win: {
    minimize: () => Promise<void>;
    toggleMaximize: () => Promise<boolean>;
    close: () => Promise<void>;
    toggleDevTools: () => Promise<void>;
  };
  gateway: {
    status: () => Promise<GwStatus>;
    logs: () => Promise<GwLog[]>;
    providers: () => Promise<GwProvider[]>;
    models: () => Promise<GwModel[]>;
    routing: () => Promise<GwRouting | null>;
    presets: () => Promise<ProviderPreset[]>;

    addProvider: (payload: ProviderInput) => Promise<{ provider: GwProvider; modelCount: number }>;
    removeProvider: (id: string) => Promise<GwProvider[]>;
    toggleProvider: (id: string, enabled: boolean) => Promise<GwProvider[]>;
    probe: (id: string) => Promise<{ ok: boolean; models?: number; latencyMs?: number; error?: string }>;
    revealKey: (id: string) => Promise<string>;

    setRouting: (patch: Partial<GwRouting>) => Promise<GwRouting>;
    clearCooldowns: () => Promise<boolean>;
    setConfig: (patch: { requireAuth?: boolean; port?: number }) => Promise<GwStatus>;

    onLog: (cb: (entry: GwLog) => void) => () => void;
  };
  shell: { openExternal: (url: string) => Promise<boolean> };
}

declare global {
  interface Window {
    freeport?: FreeportBridge;
  }
}
