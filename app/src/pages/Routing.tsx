import { useEffect, useState } from 'react';
import { Play, RotateCcw, ShieldAlert, Waves } from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, toast } from '../components/ui';
import { useGateway } from '../lib/gateway';
import type { GwRouteHop, HealthStatus } from '../lib/bridge';

/**
 * 路由页 = 真实链路解释器。
 *
 * 这里不放任何「打分权重」滑杆：现行实现没有连乘打分，权重是
 * 档位 × 余量 × 健康 三个因子相乘的结果，而顺序由 SWRR 决定，
 * 会话粘性命中时还会整段跳过 SWRR。给出可调的假参数只会让人
 * 以为调了有用。
 *
 * 数据来自 `gw_route`，走的是 chat 用的同一条 build_plan，
 * 只是不提交 SWRR 轮转状态——看预览不该影响真实流量分配。
 */

const healthTone: Record<HealthStatus, 'ok' | 'warn' | 'bad' | 'gray' | 'info'> = {
  healthy: 'ok',
  degraded: 'warn',
  cooling: 'warn',
  unavailable: 'bad',
  error: 'bad',
  probing: 'info',
  unknown: 'gray',
};

export function Routing() {
  const route = useGateway((s) => s.route);
  const providers = useGateway((s) => s.providers);
  const models = useGateway((s) => s.models);
  const explainRoute = useGateway((s) => s.explainRoute);
  const clearCooldowns = useGateway((s) => s.clearCooldowns);

  const [model, setModel] = useState('auto');
  const [session, setSession] = useState('');
  const [busy, setBusy] = useState(false);

  const run = async (m = model, s = session) => {
    setBusy(true);
    try {
      await explainRoute(m, s);
    } finally {
      setBusy(false);
    }
  };

  // 进页面先解释一次：默认 auto、无会话（不粘）
  useEffect(() => {
    void explainRoute('auto', '');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const chain = route?.chain ?? [];
  const rejected = route?.rejected ?? [];
  const maxWeight = chain.reduce((m, h) => Math.max(m, h.weight), 0) || 1;
  const connected = providers.filter((p) => p.enabled);
  const realModels = models.filter((m) => m.freeport?.upstreamModel).length;

  const byKind = {
    quota: rejected.filter((r) => r.kind === 'quota'),
    health: rejected.filter((r) => r.kind === 'health'),
    cooldown: rejected.filter((r) => r.kind === 'cooldown'),
    disabled: rejected.filter((r) => r.kind === 'disabled'),
    nokey: rejected.filter((r) => r.kind === 'nokey'),
    capability: rejected.filter((r) => r.kind === 'capability'),
  };

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="路由" en="Routing" />
          </h1>
          <p>
            <T
              zh="这条链就是下一秒真的会发出去的顺序：Gate 硬过滤 → Order 平滑加权 → Pick 会话粘性。"
              en="This is the exact order the next request would follow: Gate filters, Order weights, Pick sticks."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => void run('auto', '')} disabled={busy}>
            <RotateCcw className={busy ? 'spin' : undefined} />
            <T zh="重新解释" en="Re-explain" />
          </button>
        </div>
      </div>

      <div className="routing-grid">
        {/* ── 左：链路 ───────────────────────────── */}
        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="试算一条请求" en="Explain a request" />
              </h3>
              <span className="sub">
                <T zh="不消耗轮转预算" en="no rotation cost" />
              </span>
            </div>
            <div className="card-body">
              <div className="route-form">
                <label>
                  <span>
                    <T zh="模型" en="Model" />
                  </span>
                  <input
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    placeholder="auto / groq:llama-3.3-70b"
                    spellCheck={false}
                  />
                </label>
                <label>
                  <span>
                    <T zh="会话" en="Session" />
                  </span>
                  <input
                    value={session}
                    onChange={(e) => setSession(e.target.value)}
                    placeholder="留空 = 不粘"
                    spellCheck={false}
                  />
                </label>
                <button className="btn primary" onClick={() => void run()} disabled={busy}>
                  <Play />
                  <T zh="解释" en="Explain" />
                </button>
              </div>

              <div className="route-sum" style={{ marginTop: 12 }}>
                <Stat
                  n={chain.length}
                  zh="放行"
                  en="passed"
                  tone={chain.length ? 'ok' : 'bad'}
                />
                <Stat n={rejected.length} zh="拦下" en="rejected" tone={rejected.length ? 'warn' : 'gray'} />
                <Stat
                  n={route?.sticky ? 1 : 0}
                  zh="粘性命中"
                  en="sticky"
                  tone={route?.sticky ? 'info' : 'gray'}
                  hideZero
                />
                <Stat
                  n={Math.round((route?.budgetMs ?? 0) / 100) / 10}
                  zh="预算秒"
                  en="budget s"
                  tone="gray"
                  raw
                />
              </div>

              {connected.length === 0 && chain.length === 0 && (
                <div className="hint-line" style={{ marginTop: 10 }}>
                  <T
                    zh="还没有启用任何厂商，链路必然为空——先去「厂商」页接一个。"
                    en="No provider enabled yet, so the chain is empty by definition."
                  />
                </div>
              )}
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="候选链" en="Candidate chain" />
              </h3>
              <span className="sub">
                {route?.sticky
                  ? <T zh="粘性优先，本轮不轮转" en="sticky, no rotation this round" />
                  : <T zh="SWRR 加权轮转" en="SWRR rotation" />}
              </span>
            </div>
            <div className="card-body" style={{ paddingTop: 6, paddingBottom: 10 }}>
              {chain.length === 0 ? (
                <div className="empty" style={{ padding: '22px 10px' }}>
                  <ShieldAlert />
                  <p>
                    <T
                      zh="没有可用候选。下面「被拦下」里能看到每一跳为什么被排除。"
                      en="No candidates. The rejected panel below says why each hop was dropped."
                    />
                  </p>
                </div>
              ) : (
                <div className="chain">
                  {chain.slice(0, 12).map((h, i) => (
                    <HopRow key={`${h.provider}:${h.model}`} hop={h} rank={i + 1} max={maxWeight} />
                  ))}
                </div>
              )}
              {chain.length > 12 && (
                <div className="hint-line" style={{ padding: '0 4px' }}>
                  <T
                    zh={`另有 ${chain.length - 12} 跳未展开（前 12 跳已足够看清轮转比例）`}
                    en={`${chain.length - 12} more hops hidden (first 12 show the ratio)`}
                  />
                </div>
              )}
            </div>
          </div>

          {rejected.length > 0 && (
            <div className="card">
              <div className="card-head">
                <h3>
                  <T zh="被拦下的候选" en="Rejected" />
                </h3>
                <span className="sub">
                  <T zh={`${rejected.length} 个`} en={`${rejected.length} entries`} />
                </span>
              </div>
              <div className="card-body" style={{ paddingTop: 8 }}>
                <div className="reject-groups">
                  {byKind.quota.length > 0 && (
                    <RejectGroup
                      zh="额度耗尽"
                      en="quota exhausted"
                      tone="warn"
                      items={byKind.quota}
                    />
                  )}
                  {byKind.health.length > 0 && (
                    <RejectGroup zh="模型判死" en="model judged dead" tone="bad" items={byKind.health} />
                  )}
                  {byKind.cooldown.length > 0 && (
                    <RejectGroup zh="冷却 / 熔断" en="cooling / breaker" tone="gray" items={byKind.cooldown} />
                  )}
                  {byKind.nokey.length > 0 && (
                    <RejectGroup zh="没有 Key" en="no key" tone="bad" items={byKind.nokey} />
                  )}
                  {byKind.capability.length > 0 && (
                    <RejectGroup
                      zh="能力不匹配"
                      en="capability mismatch"
                      tone="warn"
                      items={byKind.capability}
                    />
                  )}
                  {byKind.disabled.length > 0 && (
                    <RejectGroup zh="已停用" en="disabled" tone="gray" items={byKind.disabled} />
                  )}
                </div>
                <div className="hint-line" style={{ marginTop: 8 }}>
                  <T
                    zh="额度耗尽不是故障：这一轮不打它，重置后自动回到链里。能力不匹配只在带图 / 工具 / JSON 请求下出现。"
                    en="Quota exhaustion is not a failure: skipped this round, back after reset. Capability mismatches only appear on vision, tool or JSON requests."
                  />
                </div>
              </div>
            </div>
          )}
        </div>

        {/* ── 右：机制 ───────────────────────────── */}
        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="三层，依次收窄" en="Three layers, narrowing" />
              </h3>
              <span className="sub">{route?.strategy ?? 'auto'}</span>
            </div>
            <div className="card-body">
              <div className="layers">
                <Layer
                  n="1"
                  name_zh="Gate"
                  name_en="Gate"
                  desc_zh="冷却、额度、模型判死 —— 硬过滤，没有商量"
                  desc_en="Cooldown, quota, dead models — hard filter"
                  count={rejected.length}
                  unit_zh="拦下"
                  unit_en="cut"
                  tone="bad"
                />
                <Layer
                  n="2"
                  name_zh="Order"
                  name_en="Order"
                  desc_zh="档位 × 余量 × 健康；SWRR 保证按比例轮流，不连中"
                  desc_en="Tier × headroom × health; SWRR rotates by ratio"
                  count={route?.sticky ? 0 : chain.length}
                  unit_zh="排序"
                  unit_en="ranked"
                  tone="info"
                />
                <Layer
                  n="3"
                  name_zh="Pick"
                  name_en="Pick"
                  desc_zh="同一段对话回到上次那家，保 prompt cache；余量 <10% 主动解粘"
                  desc_en="Same conversation returns to its previous provider; unsticks below 10% headroom"
                  count={route?.sticky ? 1 : 0}
                  unit_zh="命中"
                  unit_en="hit"
                  tone="accent"
                />
              </div>

              <div className="formula" style={{ marginTop: 14 }}>
                weight = tier × headroom × health
                <br />
                tier: A <b>1.00</b> · B <b>0.85</b> · C <b>0.60</b>
                <br />
                headroom = 0.2 + 0.8 × 剩余额度
                <br />
                health: healthy <b>1.00</b> · unknown <b>0.95</b> · degraded <b>0.55</b> · cooling <b>0.30</b>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="冷却中的平台" en="Cooling providers" />
              </h3>
              {route && route.cooldowns.length > 0 && (
                <button
                  className="btn ghost sm"
                  style={{ marginLeft: 'auto' }}
                  onClick={async () => {
                    await clearCooldowns();
                    await run();
                    toast('已清空冷却 / Cooldowns cleared');
                  }}
                >
                  <T zh="清空" en="Clear" />
                </button>
              )}
            </div>
            <div className="card-body" style={{ paddingTop: 8 }}>
              {!route || route.cooldowns.length === 0 ? (
                <div className="hint-line">
                  <T zh="没有冷却中的 key，所有已接厂商都能参与路由。" en="Nothing cooling: every provider is routable." />
                </div>
              ) : (
                <div className="cd-list">
                  {route.cooldowns.map((c) => (
                    <div className="cd-row" key={c.key}>
                      <code>{c.key}</code>
                      <Badge tone="warn">{c.reason}</Badge>
                      <span className="faint">
                        {untilIn(c.until)}
                        {c.hits > 1 ? ` · ${c.hits}×` : ''}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="接力语义" en="Failover semantics" />
              </h3>
              <Waves size={14} className="faint" />
            </div>
            <div className="card-body">
              <ul className="sem-list">
                <li>
                  <b>401</b> <T zh="只跳这个 key，同家其余 key 照常" en="that key only" />
                </li>
                <li>
                  <b>402</b> <T zh="整家平台跳 24h" en="whole provider, 24h" />
                </li>
                <li>
                  <b>403/404</b> <T zh="该模型的全部 key 一起跳" en="that model across keys" />
                </li>
                <li>
                  <b>429</b> <T zh="读到 Retry-After 就按它，读不到走 2m/10m/1h/24h 阶梯" en="honor Retry-After, else 2m/10m/1h/24h ladder" />
                </li>
                <li>
                  <b>5xx</b> <T zh="只影响本请求，跳到下一家" en="this request only" />
                </li>
              </ul>
              <div className="hint-line" style={{ marginTop: 8 }}>
                <T
                  zh="真实请求数（已接厂商 / 可路由模型）："
                  en="Connected providers / routable models: "
                />
                <b>
                  {connected.length} / {realModels}
                </b>
              </div>
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

/* ── 片段 ─────────────────────────────────── */

function Stat({
  n,
  zh,
  en,
  tone,
  hideZero,
  raw,
}: {
  n: number;
  zh: string;
  en: string;
  tone: 'ok' | 'warn' | 'bad' | 'info' | 'gray';
  hideZero?: boolean;
  raw?: boolean;
}) {
  if (hideZero && !n) return null;
  return (
    <div className={`route-stat ${tone}`}>
      <b>{raw ? n.toFixed(1) : n}</b>
      <span>
        <T zh={zh} en={en} />
      </span>
    </div>
  );
}

function HopRow({ hop, rank, max }: { hop: GwRouteHop; rank: number; max: number }) {
  const f = hop.factors;
  return (
    <div className={`chain-row route-hop ${hop.weight < 0.2 ? 'cool' : ''}`}>
      <span className="idx">{rank}</span>
      <div className="body">
        <div className="top">
          <span className="mname">{hop.model}</span>
          <span className="prov">{hop.provider}</span>
          <Badge tone={hop.tier === 'A' ? 'accent' : hop.tier === 'B' ? 'info' : 'gray'}>
            Tier {hop.tier}
          </Badge>
          <Badge tone={healthTone[hop.health] ?? 'gray'}>{hop.health}</Badge>
        </div>
        <div className="route-fac">
          <span title="档位">tier {f.tier.toFixed(2)}</span>
          <i>×</i>
          <span title="余量">headroom {f.headroom.toFixed(2)}</span>
          <i>×</i>
          <span title="健康">health {f.health.toFixed(2)}</span>
          <span className="eq">= {hop.weight.toFixed(3)}</span>
        </div>
        <div className="axes" style={{ marginTop: 6 }}>
          <span className="axis r" title="剩余额度">
            <i style={{ width: `${Math.round(hop.remaining * 100)}%` }} />
          </span>
        </div>
      </div>
      <div className="sc">
        <b>{(hop.weight / max).toFixed(2)}</b>
        <div className="sc-bar" style={{ marginTop: 4 }}>
          <i style={{ width: `${Math.round((hop.weight / max) * 100)}%` }} />
        </div>
      </div>
    </div>
  );
}

function Layer({
  n,
  name_zh,
  name_en,
  desc_zh,
  desc_en,
  count,
  unit_zh,
  unit_en,
  tone,
}: {
  n: string;
  name_zh: string;
  name_en: string;
  desc_zh: string;
  desc_en: string;
  count: number;
  unit_zh: string;
  unit_en: string;
  tone: 'bad' | 'info' | 'accent';
}) {
  return (
    <div className="layer">
      <span className={`ln ${tone}`}>{n}</span>
      <div className="lb">
        <div className="lh">
          <b>
            <T zh={name_zh} en={name_en} />
          </b>
          <span className="lc">
            {count} <T zh={unit_zh} en={unit_en} />
          </span>
        </div>
        <p>
          <T zh={desc_zh} en={desc_en} />
        </p>
      </div>
    </div>
  );
}

/** 把 reason 里的原始毫秒时间戳换成人类能读的剩余时间 */
function humanize(reason: string) {
  return reason.replace(/until (\d{10,})/g, (_m, ms) => `until ${untilIn(Number(ms))}`);
}

function RejectGroup({
  zh,
  en,
  tone,
  items,
}: {
  zh: string;
  en: string;
  tone: 'warn' | 'bad' | 'gray';
  items: { provider: string; model: string; reason: string }[];
}) {
  const [open, setOpen] = useState(items.length <= 4);
  const shown = open ? items : items.slice(0, 3);
  return (
    <div className="reject-group">
      <div className="rg-head">
        <Badge tone={tone}>
          <T zh={zh} en={en} />
        </Badge>
        <span className="faint">{items.length}</span>
        {items.length > 4 && (
          <button className="link-mini" onClick={() => setOpen(!open)}>
            {open ? <T zh="收起" en="less" /> : <T zh="展开全部" en="all" />}
          </button>
        )}
      </div>
      <div className="rg-items">
        {shown.map((r) => (
          <div className="rg-item" key={`${r.provider}:${r.model}`}>
            <code>{r.model === '*' ? r.provider : `${r.provider}:${r.model}`}</code>
            <span className="faint">{humanize(r.reason)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

/** 冷却剩余时间：只给人看一个近似值，不参与判定 */
function untilIn(untilMs: number) {
  const left = untilMs - Date.now();
  if (left <= 0) return '即将恢复';
  const s = Math.round(left / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h}h` : `${Math.round(h / 24)}d`;
}
