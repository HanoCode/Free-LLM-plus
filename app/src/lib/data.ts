/* ============================================================
   Mock 数据层
   后续接入真实网关时，把这里替换成 IPC / HTTP 调用即可，
   页面组件的 props 形状保持不变。
   ============================================================ */

export type Tier = 'A' | 'B' | 'C';
export type ProviderStatus = 'connected' | 'available' | 'error' | 'disabled';
export type Kind = 'apikey' | 'oauth' | 'cookie' | 'anon';

export interface Provider {
  id: string;
  name: string;
  short: string;
  tier: Tier;
  kind: Kind;
  status: ProviderStatus;
  models: number;
  latency: number | null;
  note?: string;
  homepage: string;
}

export const providers: Provider[] = [
  { id: 'groq', name: 'Groq', short: 'GQ', tier: 'A', kind: 'apikey', status: 'connected', models: 7, latency: 310, homepage: 'console.groq.com', note: 'RPD 1K/模型' },
  { id: 'google', name: 'Google AI Studio', short: 'GO', tier: 'A', kind: 'apikey', status: 'connected', models: 6, latency: 620, homepage: 'aistudio.google.com', note: '2.5 系列免费层' },
  { id: 'cerebras', name: 'Cerebras', short: 'CB', tier: 'A', kind: 'apikey', status: 'connected', models: 4, latency: 180, homepage: 'cloud.cerebras.ai', note: '最快 TTFT' },
  { id: 'mistral', name: 'Mistral', short: 'MI', tier: 'A', kind: 'apikey', status: 'connected', models: 5, latency: 540, homepage: 'console.mistral.ai' },
  { id: 'nvidia', name: 'NVIDIA NIM', short: 'NV', tier: 'A', kind: 'apikey', status: 'connected', models: 9, latency: 890, homepage: 'build.nvidia.com' },
  { id: 'cloudflare', name: 'Cloudflare Workers AI', short: 'CF', tier: 'A', kind: 'apikey', status: 'connected', models: 9, latency: 700, homepage: 'dash.cloudflare.com', note: '10K neurons/天' },
  { id: 'github', name: 'GitHub Models', short: 'GH', tier: 'C', kind: 'oauth', status: 'connected', models: 6, latency: 760, homepage: 'github.com/marketplace/models' },
  { id: 'openrouter', name: 'OpenRouter', short: 'OR', tier: 'A', kind: 'apikey', status: 'connected', models: 12, latency: 950, homepage: 'openrouter.ai', note: '仅 :free' },
  { id: 'siliconflow', name: 'SiliconFlow', short: 'SF', tier: 'A', kind: 'apikey', status: 'connected', models: 5, latency: 480, homepage: 'cloud.siliconflow.cn' },
  { id: 'zhipu', name: 'Zhipu', short: 'ZP', tier: 'A', kind: 'apikey', status: 'connected', models: 3, latency: 410, homepage: 'open.bigmodel.cn', note: 'glm-4.5-flash 永久免费' },
  { id: 'modelscope', name: 'ModelScope', short: 'MS', tier: 'A', kind: 'apikey', status: 'error', models: 0, latency: null, homepage: 'modelscope.cn', note: '401 · 待换 key' },
  { id: 'ovh', name: 'OVHcloud AI', short: 'OV', tier: 'A', kind: 'apikey', status: 'available', models: 4, latency: null, homepage: 'endpoints.ai.cloud.ovh.net', note: '未添加凭据' },
  { id: 'pollinations', name: 'Pollinations', short: 'PO', tier: 'B', kind: 'anon', status: 'available', models: 6, latency: 840, homepage: 'pollinations.ai' },
  { id: 'llm7', name: 'LLM7', short: 'L7', tier: 'B', kind: 'anon', status: 'available', models: 3, latency: 1200, homepage: 'llm7.io', note: '无 tools' },
  { id: 'opencode', name: 'OpenCode Zen', short: 'OC', tier: 'C', kind: 'cookie', status: 'available', models: 4, latency: null, homepage: 'opencode.ai/zen', note: '需网页会话' },
  { id: 'copilot', name: 'GitHub Copilot', short: 'CP', tier: 'C', kind: 'oauth', status: 'available', models: 5, latency: null, homepage: 'github.com/features/copilot', note: 'Device Code' },
];

export interface Cap {
  tools: boolean;
  vision: boolean;
  json: boolean;
}

export interface Model {
  id: string;
  name: string;
  provider: string;
  ctx: number;
  caps: Cap;
  latency: number;
  reliability: number;
  speed: number;
  smart: number;
  quota: number; // 0-1 已用比例
  free: boolean;
  status: 'healthy' | 'cooling' | 'degraded';
  meta?: string;
}

export const models: Model[] = [
  { id: 'groq/llama-3.3-70b-versatile', name: 'llama-3.3-70b-versatile', provider: 'groq', ctx: 131072, caps: { tools: true, vision: false, json: true }, latency: 310, reliability: 0.982, speed: 0.94, smart: 0.78, quota: 0.34, free: true, status: 'healthy', meta: 'RPD 1K' },
  { id: 'cerebras/qwen3-coder-480b', name: 'qwen3-coder-480b', provider: 'cerebras', ctx: 131072, caps: { tools: true, vision: false, json: true }, latency: 180, reliability: 0.961, speed: 0.99, smart: 0.9, quota: 0.52, free: true, status: 'healthy', meta: 'canary 0.3×' },
  { id: 'google/gemini-2.5-flash', name: 'gemini-2.5-flash', provider: 'google', ctx: 1048576, caps: { tools: true, vision: true, json: true }, latency: 620, reliability: 0.974, speed: 0.81, smart: 0.88, quota: 0.61, free: true, status: 'healthy', meta: '1M 上下文' },
  { id: 'google/gemini-2.5-pro', name: 'gemini-2.5-pro', provider: 'google', ctx: 2097152, caps: { tools: true, vision: true, json: true }, latency: 1180, reliability: 0.944, speed: 0.55, smart: 0.96, quota: 0.78, free: true, status: 'degraded', meta: 'RPD 25' },
  { id: 'mistral/mistral-small-3.2', name: 'mistral-small-3.2', provider: 'mistral', ctx: 131072, caps: { tools: true, vision: true, json: true }, latency: 540, reliability: 0.938, speed: 0.83, smart: 0.72, quota: 0.22, free: true, status: 'healthy' },
  { id: 'mistral/codestral-latest', name: 'codestral-latest', provider: 'mistral', ctx: 262144, caps: { tools: false, vision: false, json: true }, latency: 470, reliability: 0.955, speed: 0.9, smart: 0.81, quota: 0.18, free: true, status: 'healthy', meta: 'chat-only' },
  { id: 'nvidia/nemotron-4-340b', name: 'nemotron-4-340b', provider: 'nvidia', ctx: 4096, caps: { tools: false, vision: false, json: false }, latency: 890, reliability: 0.871, speed: 0.62, smart: 0.66, quota: 0.41, free: true, status: 'cooling', meta: '5xx 冷却 2m' },
  { id: 'cloudflare/@cf/meta/llama-4-8b', name: '@cf/meta/llama-4-8b', provider: 'cloudflare', ctx: 131072, caps: { tools: true, vision: false, json: true }, latency: 700, reliability: 0.91, speed: 0.76, smart: 0.6, quota: 0.29, free: true, status: 'healthy' },
  { id: 'github/gpt-4o-mini', name: 'gpt-4o-mini', provider: 'github', ctx: 128000, caps: { tools: true, vision: true, json: true }, latency: 760, reliability: 0.963, speed: 0.79, smart: 0.74, quota: 0.44, free: true, status: 'healthy', meta: 'Tier C' },
  { id: 'openrouter/deepseek-r1:free', name: 'deepseek-r1:free', provider: 'openrouter', ctx: 163840, caps: { tools: false, vision: false, json: false }, latency: 1450, reliability: 0.83, speed: 0.34, smart: 0.93, quota: 0.12, free: true, status: 'healthy', meta: 'reasoning' },
  { id: 'openrouter/qwen3-coder:free', name: 'qwen3-coder:free', provider: 'openrouter', ctx: 262144, caps: { tools: true, vision: false, json: true }, latency: 980, reliability: 0.9, speed: 0.66, smart: 0.87, quota: 0.09, free: true, status: 'healthy' },
  { id: 'siliconflow/Qwen3-8B', name: 'Qwen3-8B', provider: 'siliconflow', ctx: 131072, caps: { tools: true, vision: false, json: true }, latency: 480, reliability: 0.947, speed: 0.86, smart: 0.68, quota: 0.15, free: true, status: 'healthy' },
  { id: 'zhipu/glm-4.5-flash', name: 'glm-4.5-flash', provider: 'zhipu', ctx: 131072, caps: { tools: true, vision: false, json: true }, latency: 410, reliability: 0.958, speed: 0.88, smart: 0.7, quota: 0.07, free: true, status: 'healthy', meta: '永久免费' },
  { id: 'modelscope/Qwen3-Coder-30B', name: 'Qwen3-Coder-30B', provider: 'modelscope', ctx: 262144, caps: { tools: true, vision: false, json: true }, latency: 0, reliability: 0.42, speed: 0, smart: 0.83, quota: 0, free: true, status: 'degraded', meta: '401 · 需换 key' },
];

export interface Account {
  id: string;
  provider: string;
  label: string;
  kind: Kind;
  status: 'active' | 'cooldown' | 'expired' | 'error';
  masked: string;
  windows: { rpm: number; rpd: number; tpm: number; tpd: number };
  detail: string;
}

export const accounts: Account[] = [
  { id: 'a1', provider: 'groq', label: 'personal', kind: 'apikey', status: 'active', masked: 'gsk_••••••4f2a', windows: { rpm: 0.18, rpd: 0.34, tpm: 0.11, tpd: 0.22 }, detail: '上次校验 2 分钟前' },
  { id: 'a2', provider: 'google', label: 'main', kind: 'apikey', status: 'active', masked: 'AIza••••••9c31', windows: { rpm: 0.42, rpd: 0.61, tpm: 0.3, tpd: 0.48 }, detail: '上次校验 4 分钟前' },
  { id: 'a3', provider: 'cerebras', label: 'personal', kind: 'apikey', status: 'active', masked: 'csk_••••••8b7e', windows: { rpm: 0.26, rpd: 0.52, tpm: 0.2, tpd: 0.4 }, detail: '上次校验 2 分钟前' },
  { id: 'a4', provider: 'mistral', label: 'work', kind: 'apikey', status: 'active', masked: 'mis•••••2d90', windows: { rpm: 0.09, rpd: 0.22, tpm: 0.06, tpd: 0.14 }, detail: '上次校验 6 分钟前' },
  { id: 'a5', provider: 'github', label: 'octocat', kind: 'oauth', status: 'active', masked: 'gho_••••••71ac', windows: { rpm: 0.31, rpd: 0.44, tpm: 0.12, tpd: 0.28 }, detail: 'token 1h12m 后过期' },
  { id: 'a6', provider: 'openrouter', label: 'personal', kind: 'apikey', status: 'cooldown', masked: 'sk-o•••••b13d', windows: { rpm: 0.96, rpd: 0.12, tpm: 0.88, tpd: 0.1 }, detail: '429 · 01:42 后恢复' },
  { id: 'a7', provider: 'modelscope', label: 'personal', kind: 'apikey', status: 'error', masked: 'ms-••••••0c55', windows: { rpm: 0, rpd: 0, tpm: 0, tpd: 0 }, detail: '401 · 24h 内需换 key' },
  { id: 'a8', provider: 'siliconflow', label: 'personal', kind: 'apikey', status: 'active', masked: 'sk-••••••e8f1', windows: { rpm: 0.14, rpd: 0.15, tpm: 0.08, tpd: 0.11 }, detail: '上次校验 9 分钟前' },
  { id: 'a9', provider: 'zhipu', label: 'personal', kind: 'apikey', status: 'active', masked: 'zp-••••••3a77', windows: { rpm: 0.05, rpd: 0.07, tpm: 0.03, tpd: 0.05 }, detail: '永久免费额度' },
  { id: 'a10', provider: 'opencode', label: 'web session', kind: 'cookie', status: 'expired', masked: 'session ••••9d2', windows: { rpm: 0, rpd: 0, tpm: 0, tpd: 0 }, detail: '会话 3 天前过期' },
];

export interface Agent {
  id: string;
  name: string;
  short: string;
  protocol: 'openai' | 'anthropic' | 'gemini';
  path: string;
  status: 'injected' | 'detected' | 'guide';
  scope: string;
  managed: boolean;
  diff: DiffLine[];
}

export interface DiffLine {
  kind: 'ctx' | 'add' | 'del';
  no: number;
  text: string;
  managed?: boolean;
}

export const agents: Agent[] = [
  {
    id: 'claude-code',
    name: 'Claude Code',
    short: 'CC',
    protocol: 'anthropic',
    path: '~/.claude/settings.json',
    status: 'injected',
    scope: 'user',
    managed: true,
    diff: [
      { kind: 'ctx', no: 1, text: '{' },
      { kind: 'ctx', no: 2, text: '  "permissions": { "allow": ["Bash(npm:*)"] },' },
      { kind: 'add', no: 3, text: '  "env": {' },
      { kind: 'add', no: 4, text: '    "ANTHROPIC_BASE_URL": "http://127.0.0.1:8787",', managed: true },
      { kind: 'add', no: 5, text: '    "ANTHROPIC_AUTH_TOKEN": "fp_local_7k2x…",', managed: true },
      { kind: 'add', no: 6, text: '    "ANTHROPIC_MODEL": "freeport/auto"' , managed: true },
      { kind: 'add', no: 7, text: '  }', managed: true },
      { kind: 'ctx', no: 8, text: '  "includeCoAuthoredBy": false' },
      { kind: 'ctx', no: 9, text: '}' },
    ],
  },
  {
    id: 'codex',
    name: 'Codex CLI',
    short: 'CX',
    protocol: 'openai',
    path: '~/.codex/config.toml',
    status: 'injected',
    scope: 'user',
    managed: true,
    diff: [
      { kind: 'ctx', no: 1, text: 'model = "gpt-5-codex"' },
      { kind: 'ctx', no: 2, text: 'approval_policy = "on-request"' },
      { kind: 'add', no: 3, text: '# >>> freeport managed begin', managed: true },
      { kind: 'add', no: 4, text: 'model_provider = "freeport"', managed: true },
      { kind: 'add', no: 5, text: '' , managed: true },
      { kind: 'add', no: 6, text: '[model_providers.freeport]', managed: true },
      { kind: 'add', no: 7, text: 'name = "Freeport"' , managed: true },
      { kind: 'add', no: 8, text: 'base_url = "http://127.0.0.1:8787/v1"', managed: true },
      { kind: 'add', no: 9, text: 'env_key = "FREEPORT_API_KEY"', managed: true },
      { kind: 'add', no: 10, text: '# <<< freeport managed end', managed: true },
    ],
  },
  {
    id: 'cline',
    name: 'Cline',
    short: 'CL',
    protocol: 'openai',
    path: '~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_settings.json',
    status: 'detected',
    scope: 'user',
    managed: false,
    diff: [
      { kind: 'ctx', no: 1, text: '{' },
      { kind: 'ctx', no: 2, text: '  "apiProvider": "anthropic",' },
      { kind: 'ctx', no: 3, text: '  "anthropicModelId": "claude-sonnet-4-5",' },
      { kind: 'ctx', no: 4, text: '  "alwaysAllowReadOnly": true' },
      { kind: 'add', no: 5, text: '  "apiProvider": "openai",' },
      { kind: 'add', no: 6, text: '  "openAiBaseUrl": "http://127.0.0.1:8787/v1",' },
      { kind: 'add', no: 7, text: '  "openAiModelId": "freeport/auto",' },
      { kind: 'add', no: 8, text: '  "openAiApiKey": "fp_local_7k2x…"' },
      { kind: 'ctx', no: 9, text: '}' },
    ],
  },
  {
    id: 'continue',
    name: 'Continue',
    short: 'CT',
    protocol: 'openai',
    path: '~/.continue/config.yaml',
    status: 'injected',
    scope: 'user',
    managed: true,
    diff: [
      { kind: 'ctx', no: 1, text: 'name: my-config' },
      { kind: 'ctx', no: 2, text: 'version: 1.0.0' },
      { kind: 'ctx', no: 3, text: 'models:' },
      { kind: 'add', no: 4, text: '  - name: Freeport auto', managed: true },
      { kind: 'add', no: 5, text: '    provider: openai', managed: true },
      { kind: 'add', no: 6, text: '    model: freeport/auto', managed: true },
      { kind: 'add', no: 7, text: '    apiBase: http://127.0.0.1:8787/v1', managed: true },
      { kind: 'add', no: 8, text: '    apiKey: fp_local_7k2x…', managed: true },
      { kind: 'ctx', no: 9, text: 'context:' },
      { kind: 'ctx', no: 10, text: '  - provider: code' },
    ],
  },
  {
    id: 'opencode',
    name: 'OpenCode',
    short: 'OC',
    protocol: 'openai',
    path: '~/.config/opencode/opencode.json',
    status: 'detected',
    scope: 'user',
    managed: false,
    diff: [
      { kind: 'ctx', no: 1, text: '{' },
      { kind: 'ctx', no: 2, text: '  "$schema": "https://opencode.ai/config.json",' },
      { kind: 'add', no: 3, text: '  "provider": {' },
      { kind: 'add', no: 4, text: '    "freeport": {' },
      { kind: 'add', no: 5, text: '      "npm": "@ai-sdk/openai-compatible",' },
      { kind: 'add', no: 6, text: '      "options": { "baseURL": "http://127.0.0.1:8787/v1" },' },
      { kind: 'add', no: 7, text: '      "models": { "auto": { "name": "Freeport auto" } }' },
      { kind: 'add', no: 8, text: '    }' },
      { kind: 'add', no: 9, text: '  }' },
      { kind: 'ctx', no: 10, text: '}' },
    ],
  },
  {
    id: 'gemini-cli',
    name: 'Gemini CLI',
    short: 'GC',
    protocol: 'gemini',
    path: '~/.gemini/settings.json',
    status: 'detected',
    scope: 'user',
    managed: false,
    diff: [
      { kind: 'ctx', no: 1, text: '{' },
      { kind: 'ctx', no: 2, text: '  "theme": "GitHub"' },
      { kind: 'add', no: 3, text: '  ,"env": {' },
      { kind: 'add', no: 4, text: '    "GOOGLE_GEMINI_BASE_URL": "http://127.0.0.1:8787"', managed: true },
      { kind: 'add', no: 5, text: '  }', managed: true },
      { kind: 'ctx', no: 6, text: '}' },
    ],
  },
  {
    id: 'roo',
    name: 'Roo Code',
    short: 'RO',
    protocol: 'openai',
    path: 'VS Code globalStorage/rooveterinaryinc.roo-cline',
    status: 'guide',
    scope: 'manual',
    managed: false,
    diff: [],
  },
  {
    id: 'cursor',
    name: 'Cursor',
    short: 'CU',
    protocol: 'openai',
    path: 'Settings → Models → OpenAI API Key',
    status: 'guide',
    scope: 'manual',
    managed: false,
    diff: [],
  },
];

export interface Hop {
  n: number;
  model: string;
  provider: string;
  state: 'ok' | 'fail' | 'run';
  code: string;
  reason: string;
  ms: number;
  scope: string;
}

export interface LogRow {
  id: string;
  time: string;
  model: string;
  provider: string;
  status: 'ok' | 'fail' | 'retry';
  code: number;
  tokens: number;
  ms: number;
  hops: Hop[];
}

export const logs: LogRow[] = [
  {
    id: 'l1', time: '11:18:42', model: 'llama-3.3-70b-versatile', provider: 'groq', status: 'ok', code: 200, tokens: 1842, ms: 690,
    hops: [
      { n: 0, model: 'llama-3.3-70b-versatile', provider: 'groq', state: 'ok', code: '200', reason: '首选命中，工具调用 3 次', ms: 690, scope: '—' },
    ],
  },
  {
    id: 'l2', time: '11:17:55', model: 'gemini-2.5-flash', provider: 'google', status: 'retry', code: 200, tokens: 4120, ms: 2180,
    hops: [
      { n: 0, model: 'llama-3.3-70b-versatile', provider: 'groq', state: 'fail', code: '429', reason: 'RPD 已耗尽', ms: 210, scope: 'model+key → 至重置' },
      { n: 1, model: 'qwen3-coder-480b', provider: 'cerebras', state: 'fail', code: '402', reason: '额度不足', ms: 160, scope: 'provider → 24h' },
      { n: 2, model: 'gemini-2.5-flash', provider: 'google', state: 'ok', code: '200', reason: '成功，上下文 41K', ms: 1810, scope: '—' },
    ],
  },
  {
    id: 'l3', time: '11:16:03', model: 'glm-4.5-flash', provider: 'zhipu', status: 'ok', code: 200, tokens: 620, ms: 430,
    hops: [{ n: 0, model: 'glm-4.5-flash', provider: 'zhipu', state: 'ok', code: '200', reason: '粘性会话命中', ms: 430, scope: '—' }],
  },
  {
    id: 'l4', time: '11:14:31', model: 'codestral-latest', provider: 'mistral', status: 'retry', code: 200, tokens: 980, ms: 1420,
    hops: [
      { n: 0, model: 'qwen3-coder:free', provider: 'openrouter', state: 'fail', code: '400', reason: 'tools 不被支持', ms: 320, scope: '该模型（仅本请求类）' },
      { n: 1, model: 'codestral-latest', provider: 'mistral', state: 'ok', code: '200', reason: '降级为 chat-only 链', ms: 1100, scope: '—' },
    ],
  },
  {
    id: 'l5', time: '11:12:08', model: '—', provider: 'nvidia', status: 'fail', code: 502, tokens: 0, ms: 5400,
    hops: [
      { n: 0, model: 'nemotron-4-340b', provider: 'nvidia', state: 'fail', code: '5xx', reason: '上游 503', ms: 2400, scope: 'provider → 90s' },
      { n: 1, model: 'nemotron-4-340b', provider: 'nvidia', state: 'fail', code: '5xx', reason: '重试仍 503', ms: 3000, scope: 'provider → 阶梯 2m' },
    ],
  },
  {
    id: 'l6', time: '11:09:44', model: 'gpt-4o-mini', provider: 'github', status: 'ok', code: 200, tokens: 2240, ms: 810,
    hops: [{ n: 0, model: 'gpt-4o-mini', provider: 'github', state: 'ok', code: '200', reason: '视觉输入 1 张', ms: 810, scope: '—' }],
  },
];

export interface CatalogEvent {
  kind: 'add' | 'del' | 'back' | 'chg';
  label_zh: string;
  label_en: string;
  detail: string;
  badge: { zh: string; en: string; tone: string };
  time: string;
}

export const catalogEvents: CatalogEvent[] = [
  { kind: 'add', label_zh: '新增 3 个免费模型', label_en: '3 new free models', detail: 'cerebras/qwen3-coder-480b · groq/llama-4-scout-17b · cloudflare/@cf/meta/llama-4-8b', badge: { zh: '已灰度', en: 'canary', tone: 'ok' }, time: '06:04' },
  { kind: 'del', label_zh: '1 个模型下线', label_en: '1 model removed', detail: 'openrouter/deepseek-r1:free · 上游已移除该 id', badge: { zh: '已摘除', en: 'removed', tone: 'gray' }, time: '06:04' },
  { kind: 'back', label_zh: '2 个模型恢复', label_en: '2 recovered', detail: 'gemini-2.5-flash · mistral-small-3.2 · 冷却提前解除', badge: { zh: '已可用', en: 'ready', tone: 'info' }, time: '02:41' },
  { kind: 'chg', label_zh: '1 处配额变更', label_en: '1 quota change', detail: 'groq/llama-3.3-70b-versatile · RPD 14.4K → 1K（已重排权重）', badge: { zh: '已重排', en: 're-ranked', tone: 'warn' }, time: '06:05' },
];

export const discoverSources = [
  { id: 'S1', name: '上游 /models', zh: '12 个已连厂商 · 下架检测', en: '12 connected providers · removal detection', ok: true, time: '10:33', code: '200' },
  { id: 'S2', name: 'OpenRouter', zh: 'pricing.prompt = 0 · 免费模型雷达', en: 'pricing.prompt = 0 · free radar', ok: true, time: '10:33', code: '200' },
  { id: 'S3', name: 'models.dev', zh: '上下文 / 价格 / 能力字段', en: 'context / price / capabilities', ok: true, time: '09:12', code: '200' },
  { id: 'S4', name: 'changelog RSS', zh: 'Groq · Cerebras · Mistral · Cloudflare · Gemini', en: 'Groq · Cerebras · Mistral · Cloudflare · Gemini', ok: false, time: '10:20', code: '1 源超时，退避 30m' },
  { id: 'S5', name: '社区清单', zh: '仅作线索，必须实测才收录', en: 'leads only, must pass live probe', ok: true, time: '08:00', code: 'low trust' },
];

export const candidates = [
  { model: 'nvidia/nemotron-4-340b', provider: 'nvidia', source: 'S1 上游 /models', state: { zh: '第 1/2 次失败', en: 'fail 1/2', tone: 'warn' }, probe: '10:31 · 5xx', action: 'retry' },
  { model: 'codestral-latest', provider: 'mistral', source: 'S2 OpenRouter', state: { zh: '仅对话（无 tools）', en: 'chat-only', tone: 'gray' }, probe: '10:12 · ok', action: 'accept' },
  { model: 'accounts/fireworks/models/deepseek-v3p2', provider: 'fireworks', source: 'S4 changelog', state: { zh: '需 Key 才能验证', en: 'needs key', tone: 'info' }, probe: '—', action: 'add' },
];

export interface Extension {
  id: string;
  name: string;
  version: string;
  state: 'running' | 'stopped';
  signed: boolean;
  caps: string;
  detail_zh: string;
  detail_en: string;
  perms: string;
  warn?: { zh: string; en: string };
}

export const extensions: Extension[] = [
  {
    id: 'image-bridge', name: 'ChatGPT Image Bridge', version: '1.8.0', state: 'running', signed: true,
    caps: 'image: gpt-image-2 · codex-gpt-image-2',
    detail_zh: '端口 51234 · 内存 96 MB · 启动于 2 小时前 · 今日 0 次调用',
    detail_en: 'port 51234 · 96 MB · up 2h · 0 calls today',
    perms: 'network: chatgpt.com, auth.openai.com · fs: data/',
    warn: { zh: '通过网页会话访问第三方服务，账号风险自负；额度归上游所有', en: 'Uses a web session against a third-party service. Account risk is yours.' },
  },
  {
    id: 'whisper', name: 'Whisper Local', version: '0.3.2', state: 'stopped', signed: true,
    caps: 'audio: transcribe',
    detail_zh: '本地推理，无网络 · 空闲 10 分钟后自动停止',
    detail_en: 'local inference, no network · stops after 10 min idle',
    perms: 'network: none · fs: data/, models/',
  },
];

/** 24h 请求直方图（每 30 分钟一格） */
export const traffic24h = [
  12, 8, 5, 3, 2, 2, 4, 9, 18, 26, 31, 28, 34, 41, 38, 44, 52, 47, 39, 45, 51, 6, 3, 2,
];

export const quotaWindows = [
  { name: 'groq', pct: 0.34, warn: false },
  { name: 'gemini-flash', pct: 0.61, warn: false },
  { name: 'cerebras', pct: 0.52, warn: false },
  { name: 'gemini-pro', pct: 0.78, warn: true },
  { name: 'openrouter', pct: 0.94, warn: true },
];
