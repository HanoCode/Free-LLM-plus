import { useState } from 'react';
import { ExternalLink, Plus, RefreshCw, ShieldAlert, Trash2 } from 'lucide-react';
import { T } from '../lib/i18n';
import { providers as mockProviders, type Provider } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import type { GwProvider, ProviderPreset } from '../lib/bridge';

export function Providers() {
  const { setPage } = useApp();
  const desktop = useGateway((s) => s.available);
  const real = useGateway((s) => s.providers);
  const presets = useGateway((s) => s.presets);
  const removeProvider = useGateway((s) => s.removeProvider);
  const probe = useGateway((s) => s.probe);
  const [target, setTarget] = useState<ProviderPreset | null>(null);

  if (!desktop) return <BrowserFallback onDiscover={() => setPage('discover')} />;

  const used = new Set(real.map((p) => p.id));
  const available = presets.filter((p) => !used.has(p.id));
  const totalModels = real.reduce((n, p) => n + p.models, 0);

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="厂商" en="Providers" />
          </h1>
          <p>
            <T
              zh="填入官方免费 Key 即接入，校验通过才会入库。凭据用 AES-256-GCM 加密后存于本机应用目录，日志里只出现掩码。"
              en="Paste an official free key — it is validated before saving, encrypted at rest with AES-256-GCM in the app data dir, and always masked in logs."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => setPage('discover')}>
            <RefreshCw />
            <T zh="发现更多" en="Discover" />
          </button>
          <button
            className="btn primary"
            onClick={() => setTarget(presets.find((p) => p.id === 'custom') ?? null)}
          >
            <Plus />
            <T zh="自定义端点" en="Custom endpoint" />
          </button>
        </div>
      </div>

      <div className="group-title">
        <T zh={`已接入 · ${real.length}`} en={`Connected · ${real.length}`} />
        <span className="line" />
        {real.length > 0 && (
          <span className="badge gray">
            <T zh={`${totalModels} 个模型`} en={`${totalModels} models`} />
          </span>
        )}
      </div>

      {real.length === 0 ? (
        <div className="card" style={{ padding: '28px 20px', textAlign: 'center' }}>
          <div style={{ fontWeight: 600, fontSize: 13.5 }}>
            <T zh="还没有接入任何厂商" en="No providers connected" />
          </div>
          <div className="muted" style={{ fontSize: 12.5, marginTop: 6 }}>
            <T
              zh="从下面挑一个官方免费额度，粘一个 Key 就能跑通第一条链路。"
              en="Pick an official free tier below and paste a key to get the first request through."
            />
          </div>
        </div>
      ) : (
        <div className="pgrid">
          {real.map((p) => (
            <RealCard
              key={p.id}
              p={p}
              onProbe={async () => {
                const r = await probe(p.id);
                toast(
                  r.ok
                    ? `探测通过：${r.models} 个模型 · ${r.latencyMs}ms`
                    : `探测失败：${r.error ?? '未知错误'}`,
                );
              }}
              onRemove={async () => {
                await removeProvider(p.id);
                toast(`已移除 ${p.name}，密钥同时删除`);
              }}
            />
          ))}
        </div>
      )}

      <div className="group-title">
        <T zh="可接入 · 官方免费额度" en="Available · official free tier" />
        <span className="line" />
      </div>
      <div className="pgrid">
        {available.map((p) => (
          <PresetCard key={p.id} p={p} onConnect={() => setTarget(p)} />
        ))}
      </div>

      <div className="risk-note">
        <ShieldAlert />
        <p>
          <T
            zh="Freeport 只做本机转发：不代注册、不共享凭据、不绕过付费墙。凭据加密存放在本机应用目录，删除厂商即删除密钥，可随时撤销。"
            en="Freeport only forwards locally: no signup, no credential sharing, no paywall bypass. Keys are encrypted at rest and deleted together with the provider."
          />
        </p>
      </div>

      <div className="hint-line">
        <T
          zh="接入后会立刻用真实 Key 拉取 /models 做校验；拉不到就不入库，避免界面上摆一排假连接。"
          en="Keys are validated with a real /models call; failures are rejected instead of stored as fake connections."
        />
      </div>

      {target && <ConnectDialog preset={target} onClose={() => setTarget(null)} />}
    </>
  );
}

/* ── 已接入卡片 ─────────────────────────────── */
function RealCard({
  p,
  onProbe,
  onRemove,
}: {
  p: GwProvider;
  onProbe: () => Promise<void>;
  onRemove: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const tone = p.tier === 'A' ? 'ok' : p.tier === 'B' ? 'accent' : 'warn';
  const healthy = p.health.status === 'healthy';

  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">{p.name.slice(0, 2).toUpperCase()}</div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta mono" style={{ fontSize: 10.5 }}>
            {p.baseUrl}
          </div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={tone}>Tier {p.tier}</Badge>
        </span>
      </div>

      <div className="pcard-stats">
        <div>
          <b>{p.models || '—'}</b>
          <span>
            <T zh="模型" en="MODELS" />
          </span>
        </div>
        <div>
          <b>{p.health.latencyMs ? `${p.health.latencyMs}ms` : '—'}</b>
          <span>LATENCY</span>
        </div>
        <div>
          <b className="mono" style={{ fontSize: 11 }}>
            {p.keyMask || '—'}
          </b>
          <span>
            <T zh="凭据" en="KEY" />
          </span>
        </div>
      </div>

      <div className="pcard-foot">
        {healthy ? (
          <Status tone="ok">
            <T zh="已连接" en="connected" />
          </Status>
        ) : p.health.status === 'error' ? (
          <Status tone="bad">
            <T zh="校验失败" en="key invalid" />
          </Status>
        ) : (
          <Status tone="idle">
            <T zh="待探测" en="unprobed" />
          </Status>
        )}
        <span className="right" style={{ display: 'flex', gap: 6 }}>
          <button
            className="btn ghost sm"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                await onProbe();
              } finally {
                setBusy(false);
              }
            }}
          >
            <RefreshCw size={11} className={busy ? 'spin' : undefined} />
            <T zh="探测" en="Probe" />
          </button>
          <button
            className="btn ghost sm"
            onClick={async () => {
              await onRemove();
            }}
          >
            <Trash2 size={11} />
            <T zh="移除" en="Remove" />
          </button>
        </span>
      </div>

      {p.health.error && (
        <div className="modal-err" style={{ marginTop: 10, marginBottom: 0 }}>
          {p.health.error}
        </div>
      )}
    </div>
  );
}

/* ── 可接入卡片 ─────────────────────────────── */
function PresetCard({ p, onConnect }: { p: ProviderPreset; onConnect: () => void }) {
  const tone = p.tier === 'A' ? 'ok' : p.tier === 'B' ? 'accent' : 'warn';
  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">{p.name.slice(0, 2).toUpperCase()}</div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta">{p.note || p.baseUrl}</div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={tone}>Tier {p.tier}</Badge>
        </span>
      </div>

      <div className="pcard-stats">
        <div>
          <b>—</b>
          <span>
            <T zh="模型" en="MODELS" />
          </span>
        </div>
        <div>
          <b>KEY</b>
          <span>
            <T zh="凭据" en="AUTH" />
          </span>
        </div>
        <div>
          <b>{p.tier === 'A' ? '免费' : '自定'}</b>
          <span>
            <T zh="额度" en="QUOTA" />
          </span>
        </div>
      </div>

      <div className="pcard-foot">
        <Status tone="idle">
          <T zh="未接入" en="not connected" />
        </Status>
        <span className="right" style={{ display: 'flex', gap: 6 }}>
          {p.homepage && (
            <button
              className="btn ghost sm"
              onClick={() => window.freeport?.shell.openExternal(p.homepage!)}
            >
              <T zh="获取 Key" en="Get key" />
              <ExternalLink size={10} />
            </button>
          )}
          <button className="btn primary sm" onClick={onConnect}>
            <T zh="接入" en="Connect" />
          </button>
        </span>
      </div>
    </div>
  );
}

/* ── 接入弹窗 ───────────────────────────────── */
function ConnectDialog({ preset, onClose }: { preset: ProviderPreset; onClose: () => void }) {
  const addProvider = useGateway((s) => s.addProvider);
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState(preset.baseUrl);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  const isCustom = preset.id === 'custom';

  const submit = async () => {
    setErr('');
    if (!apiKey.trim()) return setErr('请填入 API Key');
    if (!baseUrl.trim()) return setErr('请填入 baseUrl');

    let id = preset.id;
    if (isCustom) {
      try {
        id = 'custom-' + new URL(baseUrl).hostname.replace(/[^a-z0-9]/gi, '-').toLowerCase();
      } catch {
        return setErr('baseUrl 不是合法 URL');
      }
    }

    setBusy(true);
    try {
      const res = await addProvider({
        id,
        name: isCustom ? id.replace('custom-', '') : preset.name,
        baseUrl: baseUrl.trim(),
        apiKey: apiKey.trim(),
        tier: preset.tier,
      });
      toast(`已接入，校验通过：${res.modelCount} 个模型 / connected`);
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <h3>
          <T zh={`接入 ${preset.name}`} en={`Connect ${preset.name}`} />
        </h3>
        <p className="sub">
          <T
            zh="粘贴后会立刻请求一次 /models 校验。校验失败不会保存，也不会出现在路由里。"
            en="A /models call validates the key immediately. Failed keys are never saved or routed."
          />
        </p>

        <div className="field">
          <label>BASE URL</label>
          <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} disabled={!isCustom} />
        </div>
        <div className="field">
          <label>API KEY</label>
          <input
            type="password"
            autoFocus
            value={apiKey}
            placeholder={preset.id === 'groq' ? 'gsk_...' : 'sk-...'}
            onChange={(e) => setApiKey(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !busy && void submit()}
          />
        </div>

        {err && <div className="modal-err">{err}</div>}

        <div className="modal-actions">
          {preset.homepage && (
            <button
              className="btn ghost"
              style={{ marginRight: 'auto' }}
              onClick={() => window.freeport?.shell.openExternal(preset.homepage!)}
            >
              <T zh="去拿 Key" en="Get key" />
              <ExternalLink size={11} />
            </button>
          )}
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            <T zh="取消" en="Cancel" />
          </button>
          <button className="btn primary" onClick={() => void submit()} disabled={busy}>
            {busy ? <T zh="校验中…" en="Validating…" /> : <T zh="接入并校验" en="Connect" />}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ── 浏览器预览兜底 ─────────────────────────── */
function BrowserFallback({ onDiscover }: { onDiscover: () => void }) {
  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="厂商" en="Providers" />
          </h1>
          <p>
            <T zh="当前是浏览器预览，接入需在桌面端。" en="Browser preview — connecting requires the desktop app." />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={onDiscover}>
            <RefreshCw />
            <T zh="发现更多" en="Discover" />
          </button>
        </div>
      </div>
      <div className="pgrid">
        {mockProviders.map((p) => (
          <MockCard key={p.id} p={p} />
        ))}
      </div>
    </>
  );
}

function MockCard({ p }: { p: Provider }) {
  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">{p.short}</div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta">{p.note ?? p.homepage}</div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={p.tier === 'A' ? 'ok' : 'warn'}>Tier {p.tier}</Badge>
        </span>
      </div>
      <div className="pcard-stats">
        <div>
          <b>{p.models || '—'}</b>
          <span>MODELS</span>
        </div>
        <div>
          <b>{p.latency ? `${p.latency}ms` : '—'}</b>
          <span>TTFB</span>
        </div>
        <div>
          <b>KEY</b>
          <span>AUTH</span>
        </div>
      </div>
    </div>
  );
}
