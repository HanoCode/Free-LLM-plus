import { useEffect, useMemo, useState } from 'react';
import { Check, ExternalLink, RefreshCw, Radar, KeyRound, Radio, TriangleAlert, ShieldCheck } from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, Status, toast } from '../components/ui';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import type { GwCatalogProvider, GwFeed } from '../lib/bridge';

function ago(iso?: string): string {
  if (!iso) return '—';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '—';
  const min = Math.max(0, Math.round((Date.now() - t) / 60000));
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const h = Math.round(min / 60);
  if (h < 24) return `${h} 小时前`;
  return `${Math.round(h / 24)} 天前`;
}

export function Discover() {
  const { catalog, providers, refreshCatalog, addProvider, feed } = useGateway();
  const { setPage } = useApp();
  const [scanning, setScanning] = useState(false);
  const [adding, setAdding] = useState<GwCatalogProvider | null>(null);

  const cat = catalog?.providers ?? [];
  const meta = catalog?.meta;
  const addedIds = useMemo(() => new Set(providers.map((p) => p.id)), [providers]);

  const anonymous = useMemo(
    () => cat.filter((p) => p.tier === 'B' && !addedIds.has(p.id) && p.api),
    [cat, addedIds],
  );
  const keyed = useMemo(
    () => cat.filter((p) => p.tier === 'A' && p.freeTier && !addedIds.has(p.id)),
    [cat, addedIds],
  );
  const watching = useMemo(
    () =>
      cat
        .filter((p) => !addedIds.has(p.id) && p.tier === 'C' && p.freeModels > 0)
        .sort((a, b) => b.freeModels - a.freeModels)
        .slice(0, 10),
    [cat, addedIds],
  );

  const scan = async () => {
    setScanning(true);
    try {
      const fresh = await refreshCatalog();
      toast(
        `目录已更新：${fresh.meta.providers} 家厂商 · ${fresh.meta.models} 个模型 · ${fresh.meta.freeModels} 个零价`,
      );
    } catch (e) {
      toast(`同步失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setScanning(false);
    }
  };

  const enable = async (p: GwCatalogProvider) => {
    if (!p.api) return;
    try {
      const res = await addProvider({
        id: p.id,
        name: p.name,
        baseUrl: p.api,
        apiKey: '',
        tier: 'B',
      });
      toast(`已启用 ${p.name}：${res.modelCount} 个模型 / enabled`);
    } catch (e) {
      toast(`启用失败：${e instanceof Error ? e.message : String(e)}`);
    }
  };

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="发现" en="Discover" />
          </h1>
          <p>
            <T
              zh="内置基线 + models.dev，只有实测通过的才进路由。候选池里的东西不进路由。"
              en="Bundled baseline plus models.dev. Nothing routes until a live probe passes."
            />
          </p>
        </div>
        <div className="head-actions">
          <span className="badge info">
            {meta?.fetchedAt ? (
              <T zh={`同步于 ${ago(meta.fetchedAt)}`} en={`synced ${ago(meta.fetchedAt)}`} />
            ) : (
              <T zh="仅内置基线" en="baseline only" />
            )}
          </span>
          <button className="btn ghost" onClick={() => void scan()} disabled={scanning}>
            <RefreshCw className={scanning ? 'spin' : ''} />
            <T zh="立即同步" en="Sync now" />
          </button>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 6 }}>
        <div className="card-head">
          <h3>
            <T zh="数据来源" en="Sources" />
          </h3>
          <span className="sub">
            {meta ? `${meta.providers} 家 · ${meta.models} 模型 · ${meta.freeModels} 个零价` : '—'}
          </span>
        </div>
        <div className="src-row">
          <span className="nm">S1 · 内置基线</span>
          <span className="meta">
            <T zh="随版本发布，离线可用；免费额度的唯一人工来源" en="Shipped with the app; the only human-curated source for free quotas" />
          </span>
          <span className="right">
            <Status tone={meta?.builtAt ? 'ok' : 'warn'}>{meta?.builtAt ?? '—'}</Status>
          </span>
        </div>
        <div className="src-row">
          <span className="nm">S2 · models.dev</span>
          <span className="meta">
            <T zh="模型事实：上下文、价格、工具/视觉/推理能力" en="Model facts: context, price, tools/vision/reasoning" />
          </span>
          <span className="right">
            {meta?.error ? (
              <Status tone="warn">{meta.error.slice(0, 40)}</Status>
            ) : (
              <Status tone={meta?.source === 'models.dev' ? 'ok' : 'warn'}>
                {meta?.source === 'models.dev' ? '200 OK' : '未同步'}
              </Status>
            )}
            <span className="ev-time">{ago(meta?.fetchedAt)}</span>
          </span>
        </div>
        <div className="src-row">
          <span className="nm">S3 · 上游 /models</span>
          <span className="meta">
            <T zh="添加厂商时真实调用一次；拉不到就不入库" en="Called once on connect; failures are never listed" />
          </span>
          <span className="right">
            <Status tone="ok">{providers.length} 家已接入</Status>
          </span>
        </div>
        <div className="src-row">
          <span className="nm">S4 · 签名目录 feed</span>
          <span className="meta">
            <T
              zh="发布即到：Ed25519 验签 + sha256 校验 + 变更事件 + 新模型 canary"
              en="Instant updates: Ed25519 + sha256, change events, canary for new models"
            />
          </span>
          <span className="right">
            {feed?.configured ? (
              <Status tone={feed.halted || feed.lastError ? 'bad' : 'ok'}>
                {feed.policy === 'pinned' ? '已锁定' : `seq ${feed.seq}`}
              </Status>
            ) : (
              <Status tone="warn">
                <T zh="未配置" en="not set" />
              </Status>
            )}
            <span className="ev-time">{feed?.configured ? ago(feed.lastCheckAt) : '—'}</span>
          </span>
        </div>
      </div>

      <FeedCard feed={feed} />

      <div className="sec-title">
        <T zh="可直接启用 · 无需 Key" en="Ready to enable · no key" />
        <span className="ln" />
      </div>
      <div className="grid-3">
        {anonymous.length === 0 && (
          <EmptyCard
            zh="没有待启用的匿名端点。目录里的 Tier B 都已接入。"
            en="No anonymous endpoints left — every Tier B provider is connected."
          />
        )}
        {anonymous.map((p) => (
          <DiscCard
            key={p.id}
            name={p.name}
            tier={p.tier}
            meta_zh={`${p.models} 个模型 · ${p.freeModels} 个零价\n${p.freeNote ?? '匿名公开端点'}`}
            meta_en={`${p.models} models · ${p.freeModels} free\n${p.freeNote ?? 'anonymous endpoint'}`}
            foot={p.quota?.rpd ? `${p.quota.rpm ?? '—'} RPM / ${p.quota.rpd} RPD` : 'unverified'}
          >
            <button className="btn primary sm" onClick={() => void enable(p)}>
              <T zh="启用" en="Enable" />
            </button>
          </DiscCard>
        ))}
      </div>

      <div className="sec-title">
        <T zh="需要 Key · 有免费额度" en="Needs a key · free tier" />
        <span className="ln" />
      </div>
      <div className="grid-3">
        {keyed.length === 0 && (
          <EmptyCard
            zh="目录里带免费额度的厂商都已接入。"
            en="Every free-tier provider in the catalog is already connected."
          />
        )}
        {keyed.slice(0, 9).map((p) => (
          <DiscCard
            key={p.id}
            name={p.name}
            tier={p.tier}
            meta_zh={`${p.freeModels} 个零价模型 · 共 ${p.models} 个\n${p.freeNote ?? '官方免费档'}`}
            meta_en={`${p.freeModels} zero-price · ${p.models} total\n${p.freeNote ?? 'official free tier'}`}
            foot={p.quota?.rpd ? `${p.quota.rpm ?? '—'} RPM / ${p.quota.rpd} RPD` : 'quota unknown'}
          >
            {p.docs && (
              <button
                className="btn sm"
                onClick={() => window.freeport?.shell.openExternal(p.docs!)}
              >
                <T zh="获取 Key" en="Get key" />
                <ExternalLink size={11} />
              </button>
            )}
            <button className="btn ghost sm" onClick={() => setAdding(p)}>
              <KeyRound size={11} />
              <T zh="粘贴" en="Paste" />
            </button>
          </DiscCard>
        ))}
      </div>

      <div className="sec-title">
        <T zh="候选池 · 未实测，不进路由" en="Candidate pool · not routed" />
        <span className="ln" />
      </div>
      <div className="card">
        <table>
          <thead>
            <tr>
              <th>PROVIDER</th>
              <th style={{ width: 150 }}>SOURCE</th>
              <th style={{ width: 150 }}>
                <T zh="状态" en="STATE" />
              </th>
              <th style={{ width: 110 }} className="right">
                <T zh="零价模型" en="FREE MODELS" />
              </th>
              <th style={{ width: 90 }} className="right">
                <T zh="操作" en="ACTION" />
              </th>
            </tr>
          </thead>
          <tbody>
            {watching.map((p) => (
              <tr key={p.id}>
                <td>
                  <div className="t-main">{p.name}</div>
                  <div className="t-sub">{p.api ?? '—'}</div>
                </td>
                <td className="t-sub">{p.source}</td>
                <td>
                  <Badge tone="warn">
                    <T zh="待实测" en="unverified" />
                  </Badge>
                </td>
                <td className="num right">{p.freeModels}</td>
                <td className="right">
                  <button className="btn ghost sm" onClick={() => setAdding(p)}>
                    <T zh="添加" en="Add" />
                  </button>
                </td>
              </tr>
            ))}
            {watching.length === 0 && (
              <tr>
                <td colSpan={5}>
                  <div className="empty">
                    <Radar />
                    <p>
                      <T
                        zh="候选池为空。同步目录后才会出现新的零价厂商。"
                        en="Nothing pending. Sync the catalog to discover new zero-price providers."
                      />
                    </p>
                  </div>
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div className="hint-line">
        <Radar size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T
          zh="免费判定只看价格字段为 0 或目录显式标注，绝不看模型名里有没有 free。新模型以 0.3 倍权重进路由，连续 20 次成功才转正。"
          en="Free is decided by zero price fields or an explicit catalog annotation — never by a name. New models route at 0.3× weight until 20 consecutive successes."
        />
      </div>

      {adding && (
        <ConnectDialog
          provider={adding}
          onClose={() => setAdding(null)}
          onDone={() => {
            setAdding(null);
            setPage('providers');
          }}
        />
      )}
    </>
  );
}

function DiscCard({
  name,
  tier,
  meta_zh,
  meta_en,
  foot,
  children,
}: {
  name: string;
  tier: string;
  meta_zh: string;
  meta_en: string;
  foot: string;
  children: React.ReactNode;
}) {
  return (
    <div className="card disc">
      <div className="disc-top">
        <div className="disc-name">{name}</div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={tier === 'A' ? 'ok' : 'accent'}>Tier {tier}</Badge>
        </span>
      </div>
      <div className="disc-meta" style={{ whiteSpace: 'pre-line' }}>
        <T zh={meta_zh} en={meta_en} />
      </div>
      <div className="disc-foot">
        {children}
        <span className="time">{foot}</span>
      </div>
    </div>
  );
}

function EmptyCard({ zh, en }: { zh: string; en: string }) {
  return (
    <div className="card disc" style={{ opacity: 0.7 }}>
      <div className="disc-meta">
        <Check size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T zh={zh} en={en} />
      </div>
    </div>
  );
}

function ConnectDialog({
  provider,
  onClose,
  onDone,
}: {
  provider: GwCatalogProvider;
  onClose: () => void;
  onDone: () => void;
}) {
  const { addProvider } = useGateway();
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  const submit = async () => {
    setErr('');
    if (!apiKey.trim()) {
      setErr('请填入 API Key');
      return;
    }
    setBusy(true);
    try {
      const res = await addProvider({
        id: provider.id,
        name: provider.name,
        baseUrl: provider.api ?? '',
        apiKey: apiKey.trim(),
        tier: provider.tier,
      });
      toast(`已接入 ${provider.name}：${res.modelCount} 个模型 / connected`);
      onDone();
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
          <T zh={`接入 ${provider.name}`} en={`Connect ${provider.name}`} />
        </h3>
        <p className="sub">
          <T
            zh="粘贴后会立刻请求一次 /models 校验；校验失败不会保存。"
            en="A /models call validates the key immediately; failures are not saved."
          />
        </p>

        <div className="field">
          <label>BASE URL</label>
          <input value={provider.api ?? ''} disabled />
        </div>
        <div className="field">
          <label>API KEY</label>
          <input
            type="password"
            autoFocus
            value={apiKey}
            placeholder="sk-..."
            onChange={(e) => setApiKey(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !busy && void submit()}
          />
        </div>

        {err && <div className="modal-err">{err}</div>}

        <div className="modal-actions">
          {provider.docs && (
            <button
              className="btn ghost"
              style={{ marginRight: 'auto' }}
              onClick={() => window.freeport?.shell.openExternal(provider.docs!)}
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

/* ── S4：签名目录 feed ─────────────────────────── */

const EVENT_LABEL: Record<string, { zh: string; en: string; tone: string }> = {
  'model.added': { zh: '新模型', en: 'model added', tone: 'ok' },
  'model.removed': { zh: '已下架', en: 'withdrawn', tone: 'bad' },
  'provider.added': { zh: '新厂商', en: 'provider added', tone: 'accent' },
  'quota.changed': { zh: '额度变更', en: 'quota changed', tone: 'info' },
  'capability.changed': { zh: '能力变更', en: 'caps changed', tone: 'info' },
  incident: { zh: '故障公告', en: 'incident', tone: 'warn' },
  'quirk.changed': { zh: '兼容性更新', en: 'quirk', tone: 'warn' },
};

function FeedCard({ feed }: { feed: GwFeed | null }) {
  const { setFeed, checkFeed, applyFeed } = useGateway();
  const [url, setUrl] = useState('');
  const [pubkey, setPubkey] = useState('');
  const [advanced, setAdvanced] = useState(false);
  const [busy, setBusy] = useState<'' | 'save' | 'check' | 'apply'>('');
  const [synced, setSynced] = useState(false);

  // 只在第一次拿到状态时回填输入框，之后不覆盖用户正在输入的内容
  useEffect(() => {
    if (!feed || synced) return;
    setUrl(feed.url);
    setSynced(true);
  }, [feed, synced]);

  const save = async (patch: Parameters<typeof setFeed>[0]) => {
    setBusy('save');
    try {
      await setFeed(patch);
    } catch (e) {
      toast(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy('');
    }
  };

  const check = async () => {
    setBusy('check');
    try {
      const f = await checkFeed();
      if (f.halted) toast(`已暂停：${f.halted}`);
      else if (f.lastError) toast(`检查失败：${f.lastError}`);
      else if (f.pending) toast(`发现新版本 seq ${f.pending.seq}，等你确认`);
      else toast(`已是最新：seq ${f.seq}`);
    } catch (e) {
      toast(`检查失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy('');
    }
  };

  const apply = async () => {
    setBusy('apply');
    try {
      const f = await applyFeed();
      toast(`已应用 seq ${f.seq} · ${f.events.length} 条变更事件`);
    } catch (e) {
      toast(`应用失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy('');
    }
  };

  const tone = !feed?.configured
    ? 'warn'
    : feed.halted || feed.lastError
      ? 'bad'
      : feed.pending
        ? 'warn'
        : 'ok';

  const events = feed?.events.slice(0, 6) ?? [];
  const canApply = !!feed?.configured && (!!feed.pending || !!feed.halted);

  return (
    <div className="card feed-card">
      <div className="card-head">
        <h3>
          <Radio size={13} style={{ verticalAlign: -2, marginRight: 6 }} />
          <T zh="目录 feed" en="Catalog feed" />
        </h3>
        <span className="sub">
          {feed?.version ? `${feed.version} · seq ${feed.seq}` : 'seq —'}
          {feed?.tombstones.length ? ` · ${feed.tombstones.length} 个已下架` : ''}
        </span>
      </div>

      <div className="card-body">
      <div className="feed-bar">
        <Status tone={tone}>
          <ShieldCheck size={11} style={{ verticalAlign: -1, marginRight: 4 }} />
          {!feed?.configured ? (
            <T zh="未配置" en="not configured" />
          ) : feed.policy === 'pinned' ? (
            <T zh="已锁定目录" en="pinned" />
          ) : feed.halted ? (
            <T zh="需要人工确认" en="needs review" />
          ) : feed.lastError ? (
            <T zh="拉取失败" en="fetch failed" />
          ) : feed.pending ? (
            <T zh={`有新版本 seq ${feed.pending.seq}`} en={`update seq ${feed.pending.seq}`} />
          ) : (
            <T zh="已连接" en="connected" />
          )}
        </Status>
        <span className="feed-at">
          <T zh="上次检查" en="checked" /> {ago(feed?.lastCheckAt)}
        </span>
        {feed?.hasPubkey ? (
          <span className="feed-at ok">
            <T zh="公钥已内置" en="public key ready" />
          </span>
        ) : (
          <span className="feed-at bad">
            <T zh="缺公钥" en="no public key" />
          </span>
        )}
        {feed?.servedTier ? (
          <span className={`feed-at ${feed.servedTier === 'pro' ? 'ok' : ''}`}>
            {feed.servedTier === 'pro' ? (
              <T zh="Pro 目录" en="Pro catalog" />
            ) : (
              <T zh="公共目录" en="public catalog" />
            )}
          </span>
        ) : null}
        <span className="spacer" />
        <button
          className="btn ghost sm"
          onClick={() => void check()}
          disabled={busy !== '' || !feed?.configured}
        >
          <RefreshCw size={12} className={busy === 'check' ? 'spin' : ''} />
          <T zh="检查" en="Check" />
        </button>
        <button className="btn primary sm" onClick={() => void apply()} disabled={busy !== '' || !canApply}>
          <T zh="应用" en="Apply" />
        </button>
      </div>

      {(feed?.halted || feed?.lastError) && (
        <div className={`feed-alert ${feed.halted ? 'warn' : 'bad'}`}>
          <TriangleAlert size={12} />
          <span>{feed.halted ?? feed.lastError}</span>
        </div>
      )}

      <div className="feed-form">
        <label>
          <span>FEED URL</span>
          <input
            value={url}
            placeholder="https://feed.example.com/catalog/latest.json"
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && void save({ url: url.trim(), enabled: url.trim() !== '' })}
          />
        </label>
        <label>
          <span>
            <T zh="应用策略" en="Policy" />
          </span>
          <select
            value={feed?.policy ?? 'auto'}
            onChange={(e) => void save({ policy: e.target.value })}
            disabled={!feed}
          >
            <option value="auto">auto · 自动应用 + canary</option>
            <option value="notify">notify · 先问再应用</option>
            <option value="pinned">pinned · 锁定不接收</option>
          </select>
        </label>
        <label>
          <span>
            <T zh="频道" en="Channel" />
          </span>
          <select
            value={feed?.channel ?? 'stable'}
            onChange={(e) => void save({ channel: e.target.value })}
            disabled={!feed}
          >
            <option value="stable">stable</option>
            <option value="beta">beta</option>
          </select>
        </label>
        <label>
          <span>
            <T zh="间隔（分钟）" en="Interval (min)" />
          </span>
          <input
            type="number"
            min={1}
            max={1440}
            value={feed?.checkMinutes ?? 15}
            onChange={(e) => void save({ checkMinutes: Number(e.target.value) || 15 })}
            disabled={!feed}
          />
        </label>
        <button
          className="btn ghost sm feed-save"
          onClick={() => void save({ url: url.trim(), enabled: url.trim() !== '' })}
          disabled={busy !== ''}
        >
          {busy === 'save' ? <T zh="保存中…" en="Saving…" /> : <T zh="保存地址" en="Save URL" />}
        </button>
      </div>

      <button className="link-mini feed-adv" onClick={() => setAdvanced((v) => !v)}>
        {advanced ? '隐藏自建公钥' : '自建 / 私有 feed：填公钥'}
      </button>
      {advanced && (
        <div className="feed-form" style={{ gridTemplateColumns: '1fr auto' }}>
          <label>
            <span>PUBKEY (hex, 32 bytes)</span>
            <input
              value={pubkey}
              placeholder="c78a1596cfbfcb2e…"
              onChange={(e) => setPubkey(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && void save({ pubkey: pubkey.trim() })}
            />
          </label>
          <button
            className="btn ghost sm feed-save"
            onClick={() => void save({ pubkey: pubkey.trim() })}
            disabled={busy !== ''}
          >
            <T zh="保存公钥" en="Save key" />
          </button>
        </div>
      )}

      <div className="feed-note">
        <T
          zh="每次检查都会带上本机档位；激活 Pro 后还会带上许可证签名（仅 https 或本机地址）。下发什么内容由服务端按档位决定，本机只负责验签——所以许可证过期也不会让转发停摆。"
          en="Every check reports this machine's tier; with Pro it also carries the license signature (https or loopback only). What comes back is decided server-side by tier — the client only verifies the signature, so an expired license never stops routing."
        />
      </div>

      <div className="feed-events">
        {events.length === 0 ? (
          <div className="feed-empty">
            <T
              zh="还没有变更事件。应用一次 feed 后，新模型/下架/额度变化会按事件列出——不给你看版本号。"
              en="No events yet. After a feed is applied, additions/removals/quota changes show up here."
            />
          </div>
        ) : (
          events.map((e) => {
            const meta = EVENT_LABEL[e.kind] ?? { zh: e.kind, en: e.kind, tone: 'info' };
            return (
              <div className="feed-ev" key={e.id}>
                <span className={`ev-kind ${meta.tone}`}>{meta.zh}</span>
                <div className="ev-body">
                  <div className="ev-title">{e.title || e.kind}</div>
                  {e.detail && <div className="ev-detail">{e.detail}</div>}
                </div>
                <span className="ev-time">{ago(e.at)}</span>
              </div>
            );
          })
        )}
      </div>
      </div>
    </div>
  );
}
