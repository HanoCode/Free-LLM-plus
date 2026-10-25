import { useEffect, useState } from 'react';
import { Play, Plus, RotateCcw, ShieldAlert, Trash2, Waves } from 'lucide-react';
import { T, healthLabel, useLang } from '../lib/i18n';
import { Badge, Switch, TierBadge, confirmDanger, toast } from '../components/ui';
import { useGateway } from '../lib/gateway';
import type {
  AccountPurpose,
  GwRouteHop,
  GwRoutePool,
  GwWeights,
  HealthStatus,
  RouteStrategy,
  StrategyProfile,
} from '../lib/bridge';

/**
 * 路由页 = 真实链路解释器。
 *
 * 权重是 档位 × 余量 × 第三因子 连乘，顺序由 SWRR 决定，会话粘性命中时
 * 还会整段跳过 SWRR。第三因子默认是「健康」；用户在「策略与命名链」里
 * 打开三维权重后换成「可靠性 / 速度 / 智能」的合成——滑杆值真的会进
 * `weight_of_model`，不是摆设，所以链上展示的 weight 会跟着变。
 *
 * 数据来自 `gw_route`，走的和真实请求同一条 `build_plan`，
 * 只是不提交 SWRR 轮转状态——看预览不该影响真实流量分配。
 * 用途（账号池）可切换：同一份配置在 chat / coding / vision 下
 * 的链可以完全不同，只给一种等于只解释了三分之一的请求。
 */

const ROUTE_PURPOSES: AccountPurpose[] = ['chat', 'coding', 'vision', 'probe'];

const PURPOSE_LABEL: Record<AccountPurpose, { zh: string; en: string }> = {
  chat: { zh: '对话', en: 'chat' },
  coding: { zh: '编码', en: 'coding' },
  vision: { zh: '图像', en: 'vision' },
  probe: { zh: '巡检', en: 'probe' },
};

const healthTone: Record<HealthStatus, 'ok' | 'warn' | 'bad' | 'gray' | 'info'> = {
  healthy: 'ok',
  degraded: 'warn',
  cooling: 'warn',
  unavailable: 'bad',
  retired: 'bad',
  error: 'bad',
  probing: 'info',
  unknown: 'gray',
};

/**
 * 冷却出处。用户真正要判断的是「等它，还是去改配置」：
 * 本地估算的会自己好，其余三种都是在等一个事实改变。
 */
const SOURCE_TEXT: Record<
  'heuristic' | 'authoritative' | 'credit' | 'tier',
  { zh: string; en: string }
> = {
  heuristic: { zh: '本地估算', en: 'estimated' },
  authoritative: { zh: '上游明说', en: 'upstream said so' },
  credit: { zh: '欠费', en: 'no credit' },
  tier: { zh: '套餐不含', en: 'not in plan' },
};

export function Routing() {
  const route = useGateway((s) => s.route);
  const providers = useGateway((s) => s.providers);
  const models = useGateway((s) => s.models);
  const explainRoute = useGateway((s) => s.explainRoute);
  const clearCooldowns = useGateway((s) => s.clearCooldowns);
  const routing = useGateway((s) => s.routing);
  const saveRouting = useGateway((s) => s.saveRouting);

  const [model, setModel] = useState('auto');
  const [session, setSession] = useState('');
  /** 按哪个账号池算。切了就立刻重算——「同一个号在别的池里能不能用」是最常问的。 */
  const [purpose, setPurpose] = useState<AccountPurpose>('chat');
  const [busy, setBusy] = useState(false);
  /** 自动调度下点「自定义调度」才展开策略面板；不持久化，改了配置自然就不是 auto 了 */
  const [peek, setPeek] = useState(false);
  const [resetting, setResetting] = useState(false);

  // 自动 = 均衡策略 + 三维权重关闭。两者任一被用户改过，就不算 auto。
  const auto =
    (routing?.strategy ?? 'balanced') === 'balanced' && !routing?.weights?.enabled;
  const showStrategy = !auto || peek;

  const run = async (m = model, s = session, p = purpose) => {
    setBusy(true);
    try {
      await explainRoute(m, s, p);
    } finally {
      setBusy(false);
    }
  };

  /** 恢复自动：均衡策略 + 权重关闭，一次回到已知良好的默认 */
  const backToAuto = async () => {
    setResetting(true);
    try {
      await saveRouting({
        strategy: 'balanced',
        weights: {
          ...(routing?.weights ?? { reliability: 70, speed: 15, intelligence: 15 }),
          enabled: false,
        },
      });
      setPeek(false);
      toast('已恢复自动调度');
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setResetting(false);
    }
  };

  // 进页面先解释一次：默认 auto、无会话（不粘）
  useEffect(() => {
    void explainRoute('auto', '', 'chat');
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
    purpose: rejected.filter((r) => r.kind === 'purpose'),
    paid: rejected.filter((r) => r.kind === 'paid'),
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
              zh="自动调度会按档位、剩余额度和健康状态分配每个请求，坏了自动换下一家。这里可以看它是怎么想的。"
              en="Auto routing balances tier, remaining quota and health per request, failing over automatically. Watch how it thinks here."
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

      {/* 自动调度横幅：默认只露这一张卡，策略/命名链/权重全收起来 */}
      <div className="card auto-card" style={{ marginBottom: 14 }}>
        <span className={`auto-dot ${auto ? 'on' : ''}`} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <b>
            {auto ? <T zh="自动调度已开启" en="Auto routing is on" /> : <T zh="自定义调度已启用" en="Custom routing is on" />}
          </b>
          <span className="sub" style={{ display: 'block' }}>
            {auto ? (
              <T
                zh="按档位 × 剩余额度 × 健康自动分配，失败自动换下一家。绝大多数情况不需要调整。"
                en="Balanced by tier × remaining quota × health, with automatic failover. Most setups never need to change this."
              />
            ) : (
              <T
                zh={`当前策略：${routing?.strategy ?? 'balanced'}${routing?.weights?.enabled ? ' · 三维权重已启用' : ''}。改动即刻生效于真实流量。`}
                en={`Strategy: ${routing?.strategy ?? 'balanced'}${routing?.weights?.enabled ? ' · weighted scoring on' : ''}. Changes apply to live traffic immediately.`}
              />
            )}
          </span>
        </div>
        {auto ? (
          <button className="btn ghost sm" onClick={() => setPeek(!peek)}>
            <T zh={peek ? '收起自定义' : '自定义调度'} en={peek ? 'Hide options' : 'Customize'} />
          </button>
        ) : (
          <button className="btn sm" disabled={resetting} onClick={() => void backToAuto()}>
            <RotateCcw size={12} />
            <T zh="恢复自动" en="Back to auto" />
          </button>
        )}
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

              <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 10 }}>
                <span className="sub" style={{ flex: 'none' }}>
                  <T zh="按用途" en="Pool" />
                </span>
                <div className="pool-pick">
                  {ROUTE_PURPOSES.map((p) => (
                    <button
                      key={p}
                      type="button"
                      className={`badge ${route?.purpose === p ? 'accent' : 'gray'}`}
                      aria-pressed={route?.purpose === p}
                      onClick={() => {
                        setPurpose(p);
                        void run(model, session, p);
                      }}
                      disabled={busy}
                    >
                      <T zh={PURPOSE_LABEL[p].zh} en={PURPOSE_LABEL[p].en} />
                    </button>
                  ))}
                </div>
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

              {route?.pool && <PoolCheck pool={route.pool} />}
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
                  : <T zh="按额度比例轮流" en="rotating by quota share" />}
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
                    <RejectGroup zh="模型连续失败" en="model failing repeatedly" tone="bad" items={byKind.health} />
                  )}
                  {byKind.cooldown.length > 0 && (
                    <RejectGroup zh="被限速或自动暂停" en="rate-limited / auto-paused" tone="gray" items={byKind.cooldown} />
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
                  {byKind.purpose.length > 0 && (
                    <RejectGroup
                      zh="用途标签不匹配"
                      en="wrong account pool"
                      tone="gray"
                      items={byKind.purpose}
                    />
                  )}
                  {byKind.paid.length > 0 && (
                    <RejectGroup
                      zh="付费模型（auto 只花免费额度）"
                      en="paid model (auto spends free quota only)"
                      tone="gray"
                      items={byKind.paid}
                    />
                  )}
                </div>
                <div className="hint-line" style={{ marginTop: 8 }}>
                  <T
                    zh="额度耗尽不是故障：这一轮不打它，重置后自动回到链里。能力不匹配只在带图 / 工具 / JSON 请求下出现。用途标签不匹配说明这条凭据被限定在别的池里——去「账号」页改标签。付费模型只在 auto 下被拦，直接点名它照样能发。"
                    en="Quota exhaustion is not a failure: skipped this round, back after reset. Capability mismatches only appear on vision, tool or JSON requests. Wrong-pool rejections mean that credential is restricted to another purpose — change it on the Accounts page. Paid models are only filtered under auto; naming one explicitly still routes to it."
                  />
                </div>
              </div>
            </div>
          )}
        </div>

        {/* ── 右：机制 ───────────────────────────── */}
        <div className="stack">
          {showStrategy && <StrategyCard />}

          {!showStrategy && (
            <div className="card">
              <div className="card-body" style={{ padding: '12px 16px' }}>
                <span className="sub">
                  <T
                    zh="策略、命名链与三维权重已收起——自动调度不需要它们。要改请点上方「自定义调度」。"
                    en="Strategies, named chains and weights are tucked away — auto routing doesn't need them. Use 'Customize' above to change."
                  />
                </span>
              </div>
            </div>
          )}

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
                <br />
                caps: 能力未实测 <b>×0.60</b>（未知 ≠ 不支持）
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
                    if (
                      !(await confirmDanger({
                        zh: '清空全部冷却？被限速的凭据会立刻重新参与路由，可能再次触发上游限流。',
                        en: 'Clear all cooldowns? Rate-limited credentials will rejoin routing immediately and may hit upstream limits again.',
                      }))
                    )
                      return;
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
                      {/* 出处决定「探针能不能提前解除」：只有我们猜的那档能被推翻。
                          上游明说的（Retry-After / 日额度重置）与欠费、套餐不含，
                          试也没用——所以标出来，省得用户等探针救场 */}
                      <Badge tone={c.source && c.source !== 'heuristic' ? 'bad' : 'gray'}>
                        <T
                          zh={SOURCE_TEXT[c.source ?? 'heuristic'].zh}
                          en={SOURCE_TEXT[c.source ?? 'heuristic'].en}
                        />
                      </Badge>
                      {c.probing && (
                        <Badge tone="info">
                          <T zh="试探中" en="probing" />
                        </Badge>
                      )}
                      <span className="faint">
                        {untilIn(c.until)}
                        {c.hits > 1 ? ` · ${c.hits}×` : ''}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
            <div className="hint-line" style={{ marginTop: 8, padding: '0 16px 14px' }}>
              <T
                zh="只有「本地估算」的冷却能被巡检提前解除（要求已经歇过一半、且还剩至少 60 秒）。上游给了明确时刻的、欠费的、套餐不含该模型的，等它自己到期——试探只会再失败一次。"
                en="Only locally estimated cooldowns can be lifted early by a probe (half the wait served, at least 60s left). Upstream-stated, credit and plan-gated ones wait it out — a probe would just fail again."
              />
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
                  <b>402</b> <T zh="欠费：整家平台跳 24h" en="no credit: whole provider, 24h" />
                </li>
                <li>
                  <b>403/404</b>{' '}
                  <T zh="套餐不含该模型：该模型的所有 key 一起跳" en="not in plan: that model across keys" />
                </li>
                <li>
                  <b>429</b>{' '}
                  <T
                    zh="读到 Retry-After 就按它；读不到按类别基准翻倍（2m→4m→…），封顶 24h"
                    en="honor Retry-After; otherwise double from the category base, capped at 24h"
                  />
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

/* ── 池体检查：链路之外，用户真正会问的那两个问题 ───────── */

const VERDICT_TONE: Record<GwRoutePool['verdict'], 'ok' | 'warn' | 'bad'> = {
  ok: 'ok',
  tight: 'warn',
  'no-candidate': 'warn',
  'too-big': 'bad',
  'pool-failing': 'bad',
  'empty-pool': 'bad',
};

/** 结论码翻成人话。网关只给码，双语措辞留在这里（MCP 那边自己写中文）。 */
function verdictText(pool: GwRoutePool): { zh: string; en: string } {
  const who = pool.landsOn ? `${pool.landsOn.provider}/${pool.landsOn.account}` : '';
  const pct = Math.round((pool.landsOn?.headroom ?? 0) * 100);
  const short = units(pool.shortfall?.left ?? 0);
  const cost = units(pool.cost ?? 0);
  switch (pool.verdict) {
    case 'empty-pool':
      return {
        zh: '没有任何凭据接这个用途——去「账号」页给某条凭据勾上，否则这类请求会全部 503。',
        en: 'No credential admits this purpose. Tag one on the Accounts page, or every such request will 503.',
      };
    case 'no-candidate':
      return {
        zh: `有 ${pool.accounts.admits} 条凭据接这个池，但这一轮全被拦下（额度 / 冷却 / 能力）。`,
        en: `${pool.accounts.admits} credentials admit this pool, but all were filtered out this round.`,
      };
    case 'too-big':
      return {
        zh: `会落在 ${who}，但它的 ${pool.shortfall?.window ?? '?'} 只剩 ${short}，这一单要 ${cost}——上游会 429。`,
        en: `Lands on ${who}, but its ${pool.shortfall?.window ?? '?'} window has only ${short} left against a ${cost} request — the upstream will 429.`,
      };
    case 'pool-failing':
      return {
        zh: `池最近 ${pool.recent?.requests} 单挂了 ${pool.recent?.errors} 单——先看是哪家在挂，别只盯这一跳。`,
        en: `${pool.recent?.errors} of the last ${pool.recent?.requests} requests failed in this pool — find the failing provider first.`,
      };
    case 'tight':
      return {
        zh: `会落在 ${who}，剩余额度只有 ${pct}%。这一单还能跑，下一单就不一定了。`,
        en: `Lands on ${who} with only ${pct}% quota left. This one fits; the next may not.`,
      };
    default:
      return {
        zh: `会落在 ${who}，剩余额度 ${pct}%。`,
        en: `Lands on ${who}, ${pct}% quota left.`,
      };
  }
}

function PoolCheck({ pool }: { pool: GwRoutePool }) {
  const tone = VERDICT_TONE[pool.verdict];
  const v = verdictText(pool);
  return (
    <div className={`pool-check ${tone}`} style={{ marginTop: 10 }}>
      <div className="pc-head">
        <Badge tone={tone}>
          <T zh="额度检查" en="quota check" />
        </Badge>
        <span className="sub">
          <T
            zh={`${pool.accounts.admits}/${pool.accounts.total} 条凭据接这个池`}
            en={`${pool.accounts.admits}/${pool.accounts.total} credentials admit this pool`}
          />
        </span>
        {pool.recent && (
          <span className="sub">
            <T
              zh={`近 ${pool.recent.requests} 单 · 挂 ${pool.recent.errors}`}
              en={`last ${pool.recent.requests} · ${pool.recent.errors} failed`}
            />
          </span>
        )}
        {pool.fits === false && (
          <Badge tone="bad">
            <T zh="这一单装不下" en="won't fit" />
          </Badge>
        )}
      </div>
      <p className="pc-line">
        <T zh={v.zh} en={v.en} />
      </p>
      {pool.tightest && (
        <p className="pc-note">
          <T
            zh={`最紧的一跳是 ${pool.tightest.account} 的 ${pool.tightest.window}，只剩 ${units(
              pool.tightest.left,
            )} · ${untilIn(pool.tightest.resetAt)} 后重置`}
            en={`Tightest hop: ${pool.tightest.account} ${pool.tightest.window} with ${units(
              pool.tightest.left,
            )} left · resets in ${untilIn(pool.tightest.resetAt)}`}
          />
        </p>
      )}
    </div>
  );
}

/** token / 次数的紧凑写法：99992 → 100k，没人想在界面上数零 */
function units(n: number) {
  if (n >= 100_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1_000) return `${(n / 1000).toFixed(1)}k`;
  return `${Math.round(n)}`;
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
          <TierBadge
            tier={hop.tier}
            tone={hop.tier === 'A' ? 'accent' : hop.tier === 'B' ? 'info' : 'gray'}
          />
          <Badge tone={healthTone[hop.health] ?? 'gray'}>
            <T zh={healthLabel(hop.health, 'zh')} en={healthLabel(hop.health, 'en')} />
          </Badge>
        </div>
        <div className="route-fac">
          <span title="档位">tier {f.tier.toFixed(2)}</span>
          <i>×</i>
          <span title="余量">headroom {f.headroom.toFixed(2)}</span>
          <i>×</i>
          {/* 启用三维权重时第三因子是合成评分，健康值只是它的一个输入 ——
              两个都标出来，用户才看得出「为什么慢了反而排前面」。
              可靠性后验单独进 title：链上一行塞五个数就没人看了 */}
          {typeof f.blend === 'number' ? (
            <span
              title={
                f.reliability == null
                  ? '三维合成评分 · 可靠性样本不足，按健康值兜底'
                  : `三维合成评分 · 可靠性后验 ${f.reliability.toFixed(2)}`
              }
            >
              score {f.blend.toFixed(2)}
            </span>
          ) : (
            <span title="健康">health {f.health.toFixed(2)}</span>
          )}
          {/* 请求要的工具/视觉/JSON 这个模型没实测支持：仍可用，但权重打折 */}
          {typeof f.caps === 'number' && f.caps < 0.999 && (
            <>
              <i>×</i>
              <span title="能力未实测，权重打折">caps {f.caps.toFixed(2)}</span>
            </>
          )}
          <span className="eq">= {hop.weight.toFixed(3)}</span>
        </div>
        {/* 底部量条行：左额度条与右权重条同基线（sc 列同样底部对齐），
            并各带一枚微型标签——之前两条裸条悬在公式和数字下面，看起来像画歪的一根线 */}
        <div className="bar-row">
          <span className="bar-lb">
            <T zh="剩余额度" en="Headroom" />
          </span>
          <span className="axis r" title="剩余额度">
            <i style={{ width: `${Math.round(hop.remaining * 100)}%` }} />
          </span>
        </div>
      </div>
      <div className="sc">
        <RingProgress value={hop.weight / max} />
        <span className="bar-lb">
          <T zh="权重" en="Weight" />
        </span>
      </div>
    </div>
  );
}

/**
 * 权重圆环：value = 本跳权重 / 榜首权重，环画比例、心里填百分数。
 * 横条在链路行里太「细」不容易比对，圆环一眼能看出谁大谁小。
 */
function RingProgress({ value }: { value: number }) {
  const v = Math.max(0, Math.min(1, value));
  const r = 11;
  const c = 2 * Math.PI * r;
  return (
    <svg className="ring" width="30" height="30" viewBox="0 0 30 30" role="img" aria-label={`${Math.round(v * 100)}%`}>
      <circle cx="15" cy="15" r={r} fill="none" stroke="var(--panel-3)" strokeWidth="3.5" />
      <circle
        cx="15"
        cy="15"
        r={r}
        fill="none"
        stroke="var(--accent)"
        strokeWidth="3.5"
        strokeLinecap="round"
        strokeDasharray={`${(v * c).toFixed(1)} ${c.toFixed(1)}`}
        transform="rotate(-90 15 15)"
      />
      <text x="15" y="15.5" textAnchor="middle" dominantBaseline="central" className="ring-num">
        {Math.round(v * 100)}
      </text>
    </svg>
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

/* ── 策略与命名链 ───────────────────────────────
   策略决定「排完之后怎么选」，命名链决定「先看哪一段」。
   两件都是纯配置，改完立刻生效，不需要重启网关。 */

const STRATEGIES: { id: RouteStrategy; zh: string; en: string; why_zh: string; why_en: string }[] = [
  {
    id: 'balanced',
    zh: '均衡',
    en: 'Balanced',
    why_zh: '按档位 × 余量 × 健康加权，平滑轮转，不会连着打同一家',
    why_en: 'Weighted by tier × headroom × health, rotating smoothly',
  },
  {
    id: 'order',
    zh: '严格按链',
    en: 'Strict order',
    why_zh: '永远先用链上第一个，挂了才下一个。适合「我就想先用这家」',
    why_en: 'Always the first in the chain; fall through only on failure',
  },
  {
    id: 'rotate',
    zh: '轮流',
    en: 'Rotate',
    why_zh: '每次请求换一个起点，不看权重。适合想把额度摊平的免费号池',
    why_en: 'Round-robin ignoring weights — good for spreading free quota',
  },
  {
    id: 'usage',
    zh: '先花用得少的',
    en: 'Least used',
    why_zh: '今天用得最少的先上，尽量让每个号都被用到',
    why_en: 'Prefer the account used least today',
  },
  {
    id: 'pace',
    zh: '先花经得起耗的',
    en: 'Pace',
    why_zh: '把剩余额度按「到重置还有多久」摊平，越能持续越先上',
    why_en: 'Remaining quota spread over time-to-reset; the most sustainable first',
  },
  {
    id: 'fastest',
    zh: '最快',
    en: 'Fastest',
    why_zh: '按探针实测延迟排，最快的先上；没测过的排在测过的后面',
    why_en: 'Sort by measured probe latency; unprobed models yield to measured ones',
  },
  {
    id: 'stablest',
    zh: '最稳定',
    en: 'Stablest',
    why_zh: '健康的先上，其次没测过的，再次慢机；同档按连续失败次数细分',
    why_en: 'Healthy first, then unprobed, then degraded; ties broken by fail streak',
  },
  {
    id: 'smartest',
    zh: '最智能',
    en: 'Smartest',
    why_zh: '实测支持推理的优先，其次工具、视觉，最后按上下文长度近似排序',
    why_en: 'Measured reasoning first, then tools, vision, then context length',
  },
];

/** 三维权重的三个轴。文案与后端 `blend_score` 的口径一一对应。 */
const W_AXES = [
  {
    k: 'reliability' as const,
    zh: '可靠性',
    en: 'Reliability',
    hint_zh: '健康状态与连续失败次数',
    hint_en: 'health status and fail streak',
  },
  {
    k: 'speed' as const,
    zh: '速度',
    en: 'Speed',
    hint_zh: '探针实测延迟，400ms 得半分',
    hint_en: 'probe latency, 400ms scores half',
  },
  {
    k: 'intelligence' as const,
    zh: '智能',
    en: 'Intelligence',
    hint_zh: '实测推理 / 工具 / 视觉 + 上下文长度',
    hint_en: 'measured reasoning / tools / vision + context',
  },
];

const W_FALLBACK: GwWeights = { enabled: false, reliability: 70, speed: 15, intelligence: 15 };

/**
 * 三维权重编辑。开关即时提交；滑杆松手提交，数字框失焦提交——
 * 拖动过程中每像素写一次盘没有意义，还会让配置写入节流把真改动挡掉。
 */
function WeightsRow({
  cur,
  disabled,
  onSave,
}: {
  cur?: GwWeights;
  disabled: boolean;
  onSave: (w: GwWeights) => void;
}) {
  const lang = useLang();
  const live = cur ?? W_FALLBACK;
  const [draft, setDraft] = useState<GwWeights>(live);
  // 后端回写（或另一个窗口改了配置）时以服务端值为准
  useEffect(() => {
    setDraft(live);
  }, [live.enabled, live.reliability, live.speed, live.intelligence]);

  const total = draft.reliability + draft.speed + draft.intelligence;
  const pct = (n: number) => (total > 0 ? Math.round((n / total) * 100) : 0);
  const clamp = (n: number) => Math.max(0, Math.min(100, Number.isFinite(n) ? n : 0));

  return (
    <div className={`weights ${draft.enabled ? 'on' : ''}`}>
      <div className="weights-head">
        <Switch
          on={draft.enabled}
          disabled={disabled}
          label={<b>{lang === 'zh' ? '三维权重' : 'Weighted scoring'}</b>}
          onChange={(v) => {
            setDraft({ ...draft, enabled: v });
            onSave({ ...draft, enabled: v });
          }}
        />
        <span className="sub">
          {draft.enabled
            ? lang === 'zh'
              ? `可靠性 ${pct(draft.reliability)}% · 速度 ${pct(draft.speed)}% · 智能 ${pct(draft.intelligence)}%`
              : `reliability ${pct(draft.reliability)}% · speed ${pct(draft.speed)}% · intelligence ${pct(draft.intelligence)}%`
            : lang === 'zh'
              ? '关闭时只看健康（旧公式）'
              : 'off = health only (legacy formula)'}
        </span>
      </div>

      {draft.enabled && (
        <div className="weights-grid">
          {W_AXES.map((a) => (
            <div className="weights-row" key={a.k}>
              <span className="wl" title={lang === 'zh' ? a.hint_zh : a.hint_en}>
                {lang === 'zh' ? a.zh : a.en}
              </span>
              <input
                type="range"
                min={0}
                max={100}
                step={5}
                value={draft[a.k]}
                disabled={disabled}
                onChange={(e) => setDraft({ ...draft, [a.k]: clamp(Number(e.target.value)) })}
                onMouseUp={() => onSave(draft)}
                onTouchEnd={() => onSave(draft)}
                onKeyUp={() => onSave(draft)}
              />
              <input
                className="wn"
                type="number"
                min={0}
                max={100}
                value={draft[a.k]}
                disabled={disabled}
                onChange={(e) => setDraft({ ...draft, [a.k]: clamp(Number(e.target.value)) })}
                onBlur={() => onSave(draft)}
              />
              <span className="wp">{pct(draft[a.k])}%</span>
            </div>
          ))}
        </div>
      )}

      <div className="hint-line">
        <T
          zh="只对「均衡」策略生效——极值策略（最快 / 最稳定 / 最智能）按定义只看单一维度。三项按总和归一，不必凑够 100；改完右侧候选链的 score 会立刻跟着变。"
          en="Balanced only — the extreme strategies are single-dimension by definition. Values are normalised by their sum, so they need not add up to 100. The score on the chain updates immediately."
        />
      </div>
    </div>
  );
}

function StrategyCard() {
  const lang = useLang();
  const routing = useGateway((s) => s.routing);
  const saveRouting = useGateway((s) => s.saveRouting);
  const [busy, setBusy] = useState(false);
  /** 新加的一行还没保存，用本地草稿撑着，否则输入框会在重新渲染时丢掉半截 */
  const [draft, setDraft] = useState<{ name: string; chain: string } | null>(null);

  const chains = routing?.chains ?? {};
  const purposeChain = routing?.purposeChain ?? {};

  const write = async (patch: Parameters<typeof saveRouting>[0]) => {
    setBusy(true);
    try {
      await saveRouting(patch);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const putChain = (name: string, chain: string) => {
    const next = {
      ...chains,
      [name]: chain
        .split(',')
        .map((x) => x.trim())
        .filter(Boolean),
    };
    void write({ chains: next });
  };

  const dropChain = (name: string) => {
    const next = { ...chains };
    delete next[name];
    // 用途还指着它的话一起解绑：留一条指向不存在链的映射，
    // 表现是「设了却没生效」，比没有更难查
    const pc = { ...purposeChain };
    for (const [k, v] of Object.entries(pc)) if (v === name) delete pc[k];
    void write({ chains: next, purposeChain: pc });
  };

  return (
    <div className="card">
      <div className="card-head">
        <h3>
          <T zh="策略与命名链" en="Strategy & named chains" />
        </h3>
        <span className="sub">{routing?.strategy ?? 'balanced'}</span>
      </div>
      <div className="card-body">
        <div className="strategy-pick">
          {STRATEGIES.map((s0) => (
            <button
              key={s0.id}
              type="button"
              className={`strategy-opt ${routing?.strategy === s0.id ? 'on' : ''}`}
              disabled={busy}
              title={s0.why_zh}
              onClick={() => void write({ strategy: s0.id })}
            >
              <b>{lang === 'zh' ? s0.zh : s0.en}</b>
              <span>{lang === 'zh' ? s0.why_zh : s0.why_en}</span>
            </button>
          ))}
        </div>

        <WeightsRow
          cur={routing?.weights}
          disabled={busy}
          onSave={(w) => void write({ weights: w })}
        />

        <div className="hint-line" style={{ marginTop: 12 }}>
          <T
            zh="命名链让一个请求先看指定的一段：客户端写 auto:coding 就只先过 coding 这条链，都没命中再回落到全局链和其余候选。链是「优先」不是「独占」。"
            en="A named chain narrows what a request looks at first: auto:coding tries that chain before falling back to the global chain and the rest. It is a priority, not an exclusive set."
          />
        </div>

        <div className="chain-list">
          {Object.entries(chains).map(([name, list]) => (
            <div className="chain-row" key={name}>
              <code>auto:{name}</code>
              <input
                defaultValue={list.join(', ')}
                spellCheck={false}
                placeholder="groq:llama-3.3-70b, cerebras:llama3.1-8b"
                onBlur={(e) => {
                  const v = e.target.value.trim();
                  if (v !== list.join(', ')) putChain(name, v);
                }}
                onKeyDown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
              />
              <button className="btn ghost sm" disabled={busy} onClick={() => dropChain(name)}>
                <Trash2 size={11} />
              </button>
            </div>
          ))}

          {draft && (
            <div className="chain-row">
              <input
                autoFocus
                value={draft.name}
                placeholder="链名，例如 coding"
                spellCheck={false}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              />
              <input
                value={draft.chain}
                placeholder="provider:model, provider:model"
                spellCheck={false}
                onChange={(e) => setDraft({ ...draft, chain: e.target.value })}
                onKeyDown={(e) => {
                  if (e.key !== 'Enter' || !draft.name.trim()) return;
                  putChain(draft.name.trim(), draft.chain);
                  setDraft(null);
                }}
              />
              <button
                className="btn ghost sm"
                disabled={busy || !draft.name.trim()}
                onClick={() => {
                  putChain(draft.name.trim(), draft.chain);
                  setDraft(null);
                }}
              >
                <Plus size={11} />
              </button>
            </div>
          )}
        </div>

        <div style={{ display: 'flex', gap: 8, marginTop: 10 }}>
          <button
            className="btn ghost sm"
            disabled={busy || !!draft}
            onClick={() => setDraft({ name: '', chain: '' })}
          >
            <Plus size={11} />
            <T zh="新建链" en="New chain" />
          </button>
        </div>

        <ProfilesEditor />

        {/* 用途 → 链：只在有链可选时才露出，空列表摆一排下拉没有意义 */}
        {Object.keys(chains).length > 0 && (
          <div className="purpose-map">
            <div className="hint-line" style={{ marginBottom: 6 }}>
              <T zh="用途映射：这类请求默认先走哪条链" en="Purpose mapping: which chain this kind of request tries first" />
            </div>
            {ROUTE_PURPOSES.map((p) => (
              <label key={p} className="purpose-map-row">
                <span>{p}</span>
                <select
                  value={purposeChain[p] ?? ''}
                  disabled={busy}
                  onChange={(e) => {
                    const pc = { ...purposeChain };
                    if (e.target.value) pc[p] = e.target.value;
                    else delete pc[p];
                    void write({ purposeChain: pc });
                  }}
                >
                  <option value="">—</option>
                  {Object.keys(chains).map((n) => (
                    <option key={n} value={n}>
                      {n}
                    </option>
                  ))}
                </select>
              </label>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/* ── 策略档（Pro）──────────────────────────────
   按 key 前缀或用途绑定策略：第一条命中的生效，其余不看。
   和分池一样是「收窄」不是「切分」——只覆盖策略选择，不改 Gate。 */

const PROFILE_MAX = 8;

function ProfilesEditor() {
  const lang = useLang();
  const zh = lang === 'zh';
  const routing = useGateway((s) => s.routing);
  const setProfiles = useGateway((s) => s.setProfiles);

  const profiles = routing?.profiles ?? [];
  const [busy, setBusy] = useState(false);

  const commit = async (next: StrategyProfile[]) => {
    setBusy(true);
    try {
      await setProfiles(next);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  /** 输入框 blur 提交整批；后端整批校验，非法档会被整批拒绝并给出原因 */
  const patchRow = (id: string, part: Partial<StrategyProfile>) => {
    void commit(profiles.map((p) => (p.id === id ? { ...p, ...part } : p)));
  };

  const addRow = () => {
    const id = `prof-${Date.now().toString(36)}`;
    void commit([...profiles, { id, name: '', keyPrefix: '', purpose: null, strategy: 'balanced' }]);
  };

  const dropRow = (id: string) => {
    void commit(profiles.filter((p) => p.id !== id));
  };

  const inputStyle: React.CSSProperties = {
    flex: 1,
    minWidth: 0,
    height: 28,
    padding: '0 8px',
    border: '1px solid var(--line)',
    borderRadius: 7,
    background: 'var(--panel-2)',
    color: 'var(--text)',
    fontSize: 11.5,
  };

  return (
    <div className="purpose-map" style={{ marginTop: 12 }}>
      <div className="hint-line" style={{ marginBottom: 6, display: 'flex', alignItems: 'center', gap: 6 }}>
        <span>
          <T
            zh="策略档：某把虚拟 Key 或某类用途，固定走某个策略。按列表顺序取第一条命中的。"
            en="Profiles: pin a strategy to a virtual key or a purpose pool. First match in list order wins."
          />
        </span>
      </div>

      <>
          {profiles.map((p) => (
            <div className="chain-row" key={p.id} style={{ flexWrap: 'wrap', gap: 6 }}>
              <input
                defaultValue={p.name}
                key={`n-${p.id}`}
                placeholder={zh ? '名称（可空）' : 'name (optional)'}
                spellCheck={false}
                disabled={busy}
                onBlur={(e) => {
                  if (e.target.value !== p.name) patchRow(p.id, { name: e.target.value });
                }}
                style={{ ...inputStyle, flex: '0 1 110px' }}
              />
              <input
                defaultValue={p.keyPrefix ?? ''}
                key={`k-${p.id}`}
                placeholder="agent:claude-code"
                spellCheck={false}
                disabled={busy}
                onBlur={(e) => {
                  if ((e.target.value || null) !== (p.keyPrefix ?? null))
                    patchRow(p.id, { keyPrefix: e.target.value || null });
                }}
                style={inputStyle}
              />
              <select
                className="select"
                value={p.purpose ?? ''}
                disabled={busy}
                onChange={(e) => patchRow(p.id, { purpose: e.target.value || null })}
                style={{ flex: 'none', width: 92 }}
              >
                <option value="">{zh ? '用途不限' : 'any purpose'}</option>
                {ROUTE_PURPOSES.map((u) => (
                  <option key={u} value={u}>
                    {PURPOSE_LABEL[u].zh}
                  </option>
                ))}
              </select>
              <select
                className="select"
                value={p.strategy}
                disabled={busy}
                onChange={(e) => patchRow(p.id, { strategy: e.target.value })}
                style={{ flex: 'none', width: 110 }}
              >
                {STRATEGIES.map((s0) => (
                  <option key={s0.id} value={s0.id}>
                    {zh ? s0.zh : s0.id}
                  </option>
                ))}
              </select>
              <button className="btn ghost sm" disabled={busy} onClick={() => dropRow(p.id)}>
                <Trash2 size={11} />
              </button>
            </div>
          ))}
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button
              className="btn ghost sm"
              disabled={busy || profiles.length >= PROFILE_MAX}
              title={
                profiles.length >= PROFILE_MAX
                  ? zh
                    ? `最多 ${PROFILE_MAX} 条`
                    : `max ${PROFILE_MAX}`
                  : undefined
              }
              onClick={addRow}
            >
              <Plus size={11} />
              <T zh="新建策略档" en="New profile" />
            </button>
            {profiles.length === 0 && (
              <span className="faint" style={{ alignSelf: 'center', fontSize: 11 }}>
                <T
                  zh="例：keyPrefix 填 agent:codex、策略选「最快」——Codex 的请求从此永远先打最快的通道。"
                  en="e.g. keyPrefix agent:codex + strategy 'fastest' — Codex traffic always tries the fastest channel first."
                />
              </span>
            )}
          </div>
          {profiles.length > 0 && (
            <div className="hint-line" style={{ marginTop: 6 }}>
              <T
                zh="keyPrefix 与用途至少要绑一个，后端才会收下这一条；都不绑的那行会在保存时被整批拒绝。"
                en="Each profile must bind at least a keyPrefix or a purpose — rows binding neither are rejected as a whole on save."
              />
            </div>
          )}
        </>
    </div>
  );
}
