import { useState } from 'react';
import { ExternalLink, Network, Plus, RefreshCw, ShieldAlert, Trash2, Unplug } from 'lucide-react';
import { T, tierLabel } from '../lib/i18n';
import { Empty, Status, TierBadge, toast } from '../components/ui';
import { BrandLogo } from '../components/BrandLogo';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import type { GwProvider, ProviderPreset } from '../lib/bridge';

export function Providers() {
  const { setPage } = useApp();
  const online = useGateway((s) => s.available);
  const real = useGateway((s) => s.providers);
  const presets = useGateway((s) => s.presets);
  const removeProvider = useGateway((s) => s.removeProvider);
  const probe = useGateway((s) => s.probe);
  const setProviderProxy = useGateway((s) => s.setProviderProxy);
  const [target, setTarget] = useState<ProviderPreset | null>(null);
  /* 档位筛选：同一组控件作用于「已接入」与「可接入」两张列表 */
  const [tier, setTier] = useState<'all' | 'A' | 'B' | 'C'>('all');

  if (!online) return <NoGateway onDiscover={() => setPage('discover')} />;

  const used = new Set(real.map((p) => p.id));
  const available = presets.filter((p) => !used.has(p.id));
  const totalModels = real.reduce((n, p) => n + p.models, 0);

  const match = (t: string) => tier === 'all' || t === tier;
  const realShown = real.filter((p) => match(p.tier));
  const availShown = available.filter((p) => match(p.tier));
  const countOf = (t: 'A' | 'B' | 'C') => available.filter((p) => p.tier === t).length;

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

      <div className="tier-filter">
        <div className="seg">
          <button className={tier === 'all' ? 'on' : ''} onClick={() => setTier('all')}>
            <T zh="全部" en="All" />
            <span className="cnt">{available.length}</span>
          </button>
          {(['A', 'B', 'C'] as const).map((t) => (
            <button key={t} className={tier === t ? 'on' : ''} onClick={() => setTier(t)}>
              <T zh={tierLabel(t, 'zh')} en={tierLabel(t, 'en')} />
              <span className="cnt">{countOf(t)}</span>
            </button>
          ))}
        </div>
        <span className="sub">
          <T
            zh="免费额度＝官方免费档；匿名端点＝无需 key；订阅付费＝走订阅或余额"
            en="Free tier = official free plan; anonymous = no key; subscription = your own plan or credits"
          />
        </span>
      </div>

      <div className="group-title">
        <T
          zh={`已接入 · ${realShown.length}${tier === 'all' ? '' : ` / ${real.length}`}`}
          en={`Connected · ${realShown.length}${tier === 'all' ? '' : ` / ${real.length}`}`}
        />
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
      ) : realShown.length === 0 ? (
        <div className="hint-line" style={{ marginTop: 0 }}>
          <T zh="当前档位下没有已接入的厂商" en="Nothing connected under this tier" />
        </div>
      ) : (
        <div className="pgrid">
          {realShown.map((p) => (
            <RealCard
              key={p.id}
              p={p}
              onProxy={async (proxy) => {
                try {
                  await setProviderProxy(p.id, proxy);
                  toast(proxy.trim() ? '代理已保存' : '代理已清除，回落到系统代理');
                } catch (e) {
                  toast(String(e));
                }
              }}
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
        <T zh={`可接入 · ${availShown.length}`} en={`Available · ${availShown.length}`} />
        <span className="line" />
      </div>
      {availShown.length === 0 ? (
        <Empty
          title={<T zh="这个档位没有可接入的厂商" en="Nothing available under this tier" />}
          hint={
            <T
              zh="换个筛选，或者到「发现更多」按目录找。"
              en="Try another tier, or browse the catalog from Discover."
            />
          }
        />
      ) : (
        <div className="pgrid">
          {availShown.map((p) => (
            <PresetCard key={p.id} p={p} onConnect={() => setTarget(p)} />
          ))}
        </div>
      )}

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
  onProxy,
}: {
  p: GwProvider;
  onProbe: () => Promise<void>;
  onRemove: () => Promise<void>;
  onProxy: (proxy: string) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  // 代理是「网络环境」不是「每次都要看的字段」：默认收起，点开才填
  const [editProxy, setEditProxy] = useState(false);
  const [proxy, setProxy] = useState(p.proxy ?? '');
  const healthy = p.health.status === 'healthy';

  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">
          <BrandLogo id={p.id} name={p.name} size={17} />
        </div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta mono" style={{ fontSize: 10.5 }}>
            {p.baseUrl}
          </div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <TierBadge tier={p.tier} />
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
          <button className="btn ghost sm" onClick={() => setEditProxy((v) => !v)}>
            <Network size={11} />
            <T zh="代理" en="Proxy" />
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

      {editProxy && (
        <div style={{ display: 'flex', gap: 6, marginTop: 10 }}>
          <input
            className="input"
            style={{ flex: 1 }}
            placeholder="http://127.0.0.1:7890"
            value={proxy}
            onChange={(e) => setProxy(e.target.value)}
          />
          <button
            className="btn primary sm"
            onClick={async () => {
              setBusy(true);
              try {
                await onProxy(proxy);
                setEditProxy(false);
              } finally {
                setBusy(false);
              }
            }}
          >
            <T zh="保存" en="Save" />
          </button>
        </div>
      )}
      {editProxy && (
        <div className="t-sub" style={{ marginTop: 6 }}>
          <T
            zh="只影响这一家的上游请求（聊天 / 拉模型 / 探针）；留空则跟随系统代理，本机回环永远直连。"
            en="Applies only to this provider's upstream calls (chat / models / probes). Empty = inherit the system proxy; loopback always connects directly."
          />
        </div>
      )}

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
  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">
          <BrandLogo id={p.id} name={p.name} size={17} />
        </div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta">{p.note || p.baseUrl}</div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <TierBadge tier={p.tier} />
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

/**
 * 订阅类预设：接入时**没有 key 可填**，凭据要在下一步「登录」时才产生。
 * 单独列出来是为了把表单从「粘贴 key」换成「说明下一步做什么」——
 * 留着输入框只会让人以为这里必须填点什么。
 */
const LOGIN_ONLY = new Set(['claude-sub', 'codex-sub']);

function ConnectDialog({ preset, onClose }: { preset: ProviderPreset; onClose: () => void }) {
  const addProvider = useGateway((s) => s.addProvider);
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState(preset.baseUrl);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  const isCustom = preset.id === 'custom';
  const loginOnly = LOGIN_ONLY.has(preset.id);

  const submit = async () => {
    setErr('');
    if (!loginOnly && !apiKey.trim()) return setErr('请填入 API Key');
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
      toast(
        loginOnly
          ? `已接入 ${preset.name}。现在去「凭据」页点「登录」完成授权，凭据才会生效。`
          : `已接入，校验通过：${res.modelCount} 个模型 / connected`,
      );
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
        {loginOnly ? (
          /* 订阅号不填 key：额度来自你自己的订阅，凭据由登录产生 */
          <div className="field">
            <label>凭据</label>
            <p className="modal-note">
              <T
                zh="这一项不用填 Key。接入之后到「凭据」页点「登录」，用你自己的订阅账号授权即可。"
                en="No key needed. After connecting, open the Credentials page and click Sign in to authorize with your own subscription."
              />
            </p>
            <p className="modal-note" style={{ color: 'var(--warn)' }}>
              <T
                zh="注意：用订阅额度驱动第三方客户端可能不符合厂商的服务条款，账号风险由你自己承担。"
                en="Note: driving third-party clients with subscription quota may violate the vendor's terms. The account risk is yours."
              />
            </p>
          </div>
        ) : (
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
        )}

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

/* ── 没有网关时：如实说没有 ─────────────────────
   这里原来摆了一排带延迟数字的假厂商卡片，纯粹为了让浏览器预览好看。
   代价是用户分不清哪条是真实连接——现在只留空态。 */
function NoGateway({ onDiscover }: { onDiscover: () => void }) {
  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="厂商" en="Providers" />
          </h1>
          <p>
            <T
              zh="厂商列表来自本机网关的配置文件与实时探测结果，不在前端存一份。"
              en="The provider list comes from the local gateway's config and live probes — the UI keeps no copy."
            />
          </p>
        </div>
      </div>

      <div className="card">
        <Empty
          icon={<Unplug />}
          title={<T zh="没有连接到网关" en="No gateway connection" />}
          hint={
            <T
              zh="这张列表只显示真实接入的厂商，所以现在一条都没有。桌面版启动时会自动把网关拉起来（127.0.0.1:8787）；如果你是在浏览器里打开的这个页面，那是开发预览，它连不上本机进程。"
              en="This list only shows providers that are really connected, so it is empty. The desktop build starts the gateway on launch (127.0.0.1:8787); if you opened this page in a browser, that is a dev preview and it cannot reach the local process."
            />
          }
        />
        <div style={{ display: 'flex', gap: 8, justifyContent: 'center', paddingBottom: 22 }}>
          <button className="btn ghost" onClick={onDiscover}>
            <T zh="看目录里有哪些免费厂商" en="Browse free providers" />
          </button>
        </div>
      </div>
    </>
  );
}
