import { useState } from 'react';
import {
  Activity,
  ArrowRight,
  Check,
  ChevronDown,
  Gauge,
  KeyRound,
  MessageSquare,
  Plug,
  Radar,
  ShieldCheck,
  Zap,
} from 'lucide-react';
import { T } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { Badge, CopyBtn, Status, fmt, toast } from '../components/ui';
import { QuotaPanel } from '../components/QuotaPanel';

const POOL_LABEL: Record<string, { zh: string; en: string }> = {
  chat: { zh: '对话', en: 'chat' },
  coding: { zh: '编码', en: 'coding' },
  vision: { zh: '视觉', en: 'vision' },
  probe: { zh: '巡检', en: 'probe' },
};

function hhmm(iso?: string | null): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function ago(iso?: string | null): string {
  if (!iso) return '从未';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '—';
  const min = Math.round((Date.now() - t) / 60000);
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const h = Math.round(min / 60);
  if (h < 24) return `${h} 小时前`;
  return `${Math.round(h / 24)} 天前`;
}

/** 美元金额：小额保留四位，否则两位——0.0006 显示成 0.00 等于说谎 */
function money(usd: number): string {
  if (!Number.isFinite(usd) || usd <= 0) return '0';
  return usd < 0.01 ? usd.toFixed(4) : usd.toFixed(2);
}

/** 三步任务卡的收起状态。用户明确收起后就不再打扰，存本地。 */
const TASKS_HIDDEN_KEY = 'fp.tasks.hidden';

export function Overview() {
  const { setPage } = useApp();
  const { status, providers, models, logs, patrol, quota, digest, observed, agents, runPatrol } =
    useGateway();

  const connected = providers.filter((p) => p.hasKey);
  // 凭据是真正的记账单位：同一家两把 key 算两条通道
  const credentials = providers.reduce(
    (n, p) => n + p.accounts.filter((a) => a.hasKey).length,
    0,
  );
  const rows = models.filter((m) => m.freeport?.upstreamModel);
  const ready = rows.filter((m) => m.freeport?.health === 'healthy').length;
  const addr = `http://${status?.bind ?? '127.0.0.1'}:${status?.port ?? 8787}/v1`;

  // ── 三步任务卡：只依据真实状态，不编进度 ──
  // 1) 接了模型源：有任一可用凭据
  // 2) 接上工具：有 Agent 已注入；或已经有请求进来（说明已经在用）
  // 3) 发出过请求：日志环里有记录
  const step1 = connected.length > 0;
  const injected = agents.filter((a) => a.state === 'injected').length;
  const step2 = injected > 0 || logs.length > 0;
  const step3 = logs.length > 0;
  const [tasksHidden, setTasksHidden] = useState(
    () => localStorage.getItem(TASKS_HIDDEN_KEY) === '1',
  );
  /** 巡检控制台默认折叠：它是运维工具，不是首屏内容 */
  const [patrolOpen, setPatrolOpen] = useState(
    () => localStorage.getItem('fp.patrol.open') === '1',
  );

  const requests = status?.requests24h ?? 0;
  const tokens = (status?.tokensIn ?? 0) + (status?.tokensOut ?? 0);
  const errors = status?.errors ?? 0;
  // 没有任何请求时成功率是「没有数据」，不是 100%——分母为 0 就别编一个好看的数
  const success = requests > 0 ? 1 - errors / requests : null;

  const probe = async (all: boolean) => {
    try {
      const r = await runPatrol(all);
      toast(
        r.skipped
          ? r.skipped
          : `巡检完成：探了 ${r.ran} 个 · 成功 ${r.ok} · 失败 ${r.failed}`,
      );
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <>
      <div className="hero">
        <div className="hero-ico">
          <ShieldCheck />
        </div>
        <div>
          <h2>
            {status?.running ? (
              <T zh="网关运行中" en="Gateway running" />
            ) : (
              <T zh="网关未运行" en="Gateway stopped" />
            )}
          </h2>
          <p>
            {connected.length > 0 ? (
              <T
                zh={`${connected.length} 个厂商、${credentials} 条凭据、${rows.length} 个模型；上次全量体检 ${ago(patrol?.lastSweepAt)}。`}
                en={`${connected.length} providers, ${credentials} credentials, ${rows.length} models. Last full audit ${ago(patrol?.lastSweepAt)}.`}
              />
            ) : (
              <T
                zh="还没有接入厂商。先去「厂商」页填一个 API Key。"
                en="No providers yet. Add an API key on the Providers page."
              />
            )}
          </p>
        </div>
        <div className="addr-pill">
          <span>{addr}</span>
          <CopyBtn value={addr} label="" tip="复制网关地址 / Copy address" />
        </div>
        <div className="hero-stats">
          <div className="hero-stat">
            <b>{ready}</b>
            <span>
              <T zh="可用模型" en="MODELS" />
            </span>
          </div>
          <div className="hero-stat">
            <b>{rows.length}</b>
            <span>
              <T zh="目录模型" en="LISTED" />
            </span>
          </div>
          <div className="hero-stat">
            <b>{patrol?.dueNow ?? 0}</b>
            <span>
              <T zh="待巡检" en="DUE" />
            </span>
          </div>
        </div>
      </div>

      {/* 三步任务卡：新用户的主路。三步全做完自动收成一行，也可手动收起 */}
      {(!step1 || !step2 || !step3) && !tasksHidden && (
        <div className="card tasks-card" style={{ marginBottom: 14 }}>
          <div className="card-head">
            <h3>
              <T zh="开始使用" en="Get started" />
            </h3>
            <span className="sub">
              <T zh="三步就能用起来" en="three steps to your first reply" />
            </span>
            <button
              className="btn ghost sm"
              style={{ marginLeft: 'auto' }}
              aria-label="collapse"
              onClick={() => {
                localStorage.setItem(TASKS_HIDDEN_KEY, '1');
                setTasksHidden(true);
              }}
            >
              <T zh="收起" en="Hide" />
              <ChevronDown size={12} />
            </button>
          </div>
          <div className="card-body" style={{ padding: '10px 16px' }}>
            <div className={`task-row ${step1 ? 'done' : ''}`}>
              <span className="task-dot">
                <Check size={11} />
              </span>
              <div className="task-txt">
                <b>
                  <T zh="接一个模型源" en="Connect a model source" />
                </b>
                <span>
                  {step1 ? (
                    <T
                      zh={`${connected.length} 个厂商 · ${credentials} 条凭据已就绪`}
                      en={`${connected.length} providers · ${credentials} credentials ready`}
                    />
                  ) : (
                    <T
                      zh="粘一个免费 Key，或直接启用免 Key 的匿名源"
                      en="paste a free API key, or enable a keyless source"
                    />
                  )}
                </span>
              </div>
              {!step1 && (
                <button className="btn primary sm" onClick={() => setPage('connect')}>
                  <KeyRound size={12} />
                  <T zh="去接入" en="Connect" />
                </button>
              )}
            </div>
            <div className={`task-row ${step2 ? 'done' : ''}`}>
              <span className="task-dot">
                <Check size={11} />
              </span>
              <div className="task-txt">
                <b>
                  <T zh="接到你正在用的工具" en="Plug into your tools" />
                </b>
                <span>
                  {step2 ? (
                    injected > 0 ? (
                      <T zh={`已注入 ${injected} 个 Agent`} en={`${injected} agents injected`} />
                    ) : (
                      <T zh="已有请求经网关转出" en="requests already flowing through" />
                    )
                  ) : (
                    <T
                      zh="一键注入 Claude Code / Codex 等，或复制网关地址手动接入"
                      en="inject Claude Code / Codex in one click, or copy the gateway address"
                    />
                  )}
                </span>
              </div>
              {!step2 && (
                <>
                  <CopyBtn value={addr} label="" tip="复制网关地址 / Copy address" />
                  <button className="btn primary sm" onClick={() => setPage('agents')}>
                    <Plug size={12} />
                    <T zh="一键注入" en="Inject" />
                  </button>
                </>
              )}
            </div>
            <div className={`task-row ${step3 ? 'done' : ''}`}>
              <span className="task-dot">
                <Check size={11} />
              </span>
              <div className="task-txt">
                <b>
                  <T zh="发第一条消息" en="Send your first message" />
                </b>
                <span>
                  {step3 ? (
                    <T zh="链路已通，对话会出现在下方" en="pipeline works — chats show up below" />
                  ) : (
                    <T
                      zh="在试验场直接对话，验证整条链路"
                      en="chat in the Playground to verify the pipeline"
                    />
                  )}
                </span>
              </div>
              {!step3 && (
                <button
                  className="btn primary sm"
                  onClick={() => setPage(step1 ? 'playground' : 'connect')}
                >
                  <MessageSquare size={12} />
                  <T zh="打开试验场" en="Open Playground" />
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {digest && (
        <div className="card" style={{ marginBottom: 14 }}>
          <div className="card-head">
            <h3>
              <T zh="今日摘要" en="Today" />
            </h3>
            <span className="sub">{digest.headline}</span>
            <button
              className="btn ghost sm"
              style={{ marginLeft: 'auto' }}
              onClick={() => setPage('settings')}
            >
              <T zh="巡检设置" en="Patrol settings" />
              <ArrowRight size={12} />
            </button>
          </div>
          <div className="src-row">
            <span className="nm">
              <T zh="变更" en="Changes" />
            </span>
            <span className="meta">
              {digest.changes.length === 0 ? (
                <T zh="24 小时内没有目录变更" en="No catalog changes in 24h" />
              ) : (
                digest.changes.map((c, i) => (
                  <span key={`${c.kind}-${i}`} style={{ marginRight: 10 }}>
                    <Badge tone={c.kind === 'removed' ? 'warn' : c.kind === 'added' ? 'ok' : 'bad'}>
                      {c.title}
                    </Badge>{' '}
                    {c.detail}
                  </span>
                ))
              )}
            </span>
            <span className="right">
              <Badge tone={(patrol?.remaining ?? 0) > 0 ? 'ok' : 'warn'}>
                {digest.probeUsed} / {digest.probeBudget}
              </Badge>
            </span>
          </div>
          <div className="src-row">
            <span className="nm">
              <T zh="健康" en="Health" />
            </span>
            <span className="meta">
              <T
                zh={`可用 ${digest.health.healthy} · 降级 ${digest.health.degraded} · 冷却 ${digest.health.cooling} · 不可用 ${digest.health.unavailable} · 退役 ${digest.health.retired}`}
                en={`${digest.health.healthy} ready · ${digest.health.degraded} degraded · ${digest.health.cooling} cooling · ${digest.health.unavailable} down · ${digest.health.retired} retired`}
              />
            </span>
            <span className="right">
              <span className="ev-time">{hhmm(digest.at)}</span>
            </span>
          </div>
          {digest.pools.length > 0 && (
            <div className="src-row">
              <span className="nm">
                <T zh="用途池" en="Pools" />
              </span>
              <span className="meta">
                <span className="faint" style={{ marginRight: 10 }}>
                  <T
                    zh={`最近 ${digest.logScanned} 条请求：`}
                    en={`last ${digest.logScanned} requests: `}
                  />
                </span>
                {digest.pools.map((p) => {
                  const ok = p.requests - p.errors;
                  // 有失败才染色：全绿挂一排绿标反而看不出哪个池在挂
                  const tone = p.errors === 0 ? 'ok' : ok * 2 >= p.requests ? 'warn' : 'bad';
                  const label = POOL_LABEL[p.purpose] ?? { zh: p.purpose, en: p.purpose };
                  return (
                    <span key={p.purpose} style={{ marginRight: 8 }}>
                      <Badge tone={tone}>
                        <T zh={label.zh} en={label.en} /> {ok}/{p.requests}
                      </Badge>
                    </span>
                  );
                })}
              </span>
              <span className="right" />
            </div>
          )}
          {digest.attention.length > 0 && (
            <div className="src-row">
              <span className="nm">
                <T zh="需要处理" en="Needs attention" />
              </span>
              <span className="meta">
                {digest.attention.map((a) => (
                  <span key={a} style={{ marginRight: 12 }}>
                    <Status tone="warn">{a}</Status>
                  </span>
                ))}
              </span>
              <span className="right" />
            </div>
          )}
        </div>
      )}

      {/* 近期观测：样本是内存日志环，与上面的累计计数不是一个口径，
          所以这块自己带「最近 N 单」的说法，不冒充全生命周期总量 */}
      {observed && observed.requests > 0 && (
        <div className="card" style={{ marginBottom: 14 }}>
          <div className="card-head">
            <h3>
              <T zh="近 24 小时" en="Last 24h" />
            </h3>
            <span className="sub">
              <T
                zh={`${observed.requests} 单 · 失败 ${observed.errors}`}
                en={`${observed.requests} requests · ${observed.errors} failed`}
              />
            </span>
          </div>
          <div className="src-row">
            <span className="nm">
              <T zh="延迟" en="Latency" />
            </span>
            <span className="meta">
              {/* 只统计成功的请求：一条 90s 超时会把均值带偏，分位数就是为了不被带偏 */}
              <T
                zh={`p50 ${observed.p50Ms === null ? '—' : fmt.ms(observed.p50Ms)} · p95 ${observed.p95Ms === null ? '—' : fmt.ms(observed.p95Ms)} · 最慢 ${observed.maxMs === null ? '—' : fmt.ms(observed.maxMs)}`}
                en={`p50 ${observed.p50Ms === null ? '—' : fmt.ms(observed.p50Ms)} · p95 ${observed.p95Ms === null ? '—' : fmt.ms(observed.p95Ms)} · worst ${observed.maxMs === null ? '—' : fmt.ms(observed.maxMs)}`}
              />
            </span>
            <span className="right">
              <span className="faint">
                <T zh="失败请求不计入" en="failures excluded" />
              </span>
            </span>
          </div>
          <div className="src-row">
            <span className="nm">
              <T zh="折算花费" en="List-price value" />
            </span>
            <span className="meta">
              <T
                zh={`约 $${money(observed.costUsd)} —— 按目录价折算，不是账单（免费额度实付 0）`}
                en={`≈ $${money(observed.costUsd)} at list price, not a bill (free tiers cost nothing)`}
              />
              {observed.costUnknown > 0 && (
                <span className="faint" style={{ marginLeft: 8 }}>
                  <T
                    zh={`· ${observed.costUnknown} 单目录里没有价格，未计入`}
                    en={`· ${observed.costUnknown} requests have no price and are excluded`}
                  />
                </span>
              )}
            </span>
            <span className="right">
              <span className="ev-time">
                {(observed.tokensIn + observed.tokensOut).toLocaleString()} tok
              </span>
            </span>
          </div>
          {observed.byPurpose.length > 1 && (
            <div className="src-row">
              <span className="nm">
                <T zh="按用途" en="By purpose" />
              </span>
              <span className="meta">
                {observed.byPurpose.map((b) => {
                  const label = POOL_LABEL[b.key] ?? { zh: b.key, en: b.key };
                  return (
                    <span key={b.key} style={{ marginRight: 8 }}>
                      <Badge tone="info">
                        <T zh={label.zh} en={label.en} /> {b.requests}
                      </Badge>
                    </span>
                  );
                })}
              </span>
              <span className="right" />
            </div>
          )}
        </div>
      )}

      <div className="metrics">
        <div className="card metric">
          <div className="k">
            <Activity size={12} />
            <T zh="累计请求" en="Requests" />
          </div>
          <div className="v">{requests.toLocaleString()}</div>
          <div className="t-sub">
            <T zh={`失败 ${errors} 次`} en={`${errors} failed`} />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <Zap size={12} />
            <T zh="Token 消耗" en="Tokens" />
          </div>
          <div className="v">
            {tokens > 1_000_000 ? `${(tokens / 1_000_000).toFixed(2)}M` : tokens.toLocaleString()}
            <small>
              <T zh="免费" en="free" />
            </small>
          </div>
          <div className="t-sub">
            <T
              zh={`入 ${status?.tokensIn.toLocaleString() ?? 0} · 出 ${status?.tokensOut.toLocaleString() ?? 0}`}
              en={`in ${status?.tokensIn ?? 0} · out ${status?.tokensOut ?? 0}`}
            />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <Gauge size={12} />
            <T zh="成功率" en="Success rate" />
          </div>
          <div className="v">{success === null ? '—' : fmt.pct(success)}</div>
          <div className="t-sub">
            <T
              zh={`冷却中 ${status?.cooldowns.length ?? 0} 条`}
              en={`${status?.cooldowns.length ?? 0} cooling`}
            />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <Radar size={12} />
            <T zh="探针预算" en="Probe budget" />
          </div>
          <div className="v">
            {patrol?.usedToday ?? 0}
            <small>/ {patrol?.budgetToday ?? 0}</small>
          </div>
          <div className="t-sub">
            <T
              zh={`累计 ${patrol?.probesTotal ?? 0} 次 · 成功 ${patrol?.probesOk ?? 0}`}
              en={`${patrol?.probesTotal ?? 0} total · ${patrol?.probesOk ?? 0} ok`}
            />
          </div>
        </div>
      </div>

      <QuotaPanel quota={quota} />

      {/* 巡检控制台：运维工具，默认折叠成一行；展开后才是完整控制台 */}
      <div className="card" style={{ marginBottom: 14 }}>
        <div
          className="card-head"
          style={{ cursor: 'pointer', userSelect: 'none' }}
          onClick={() => {
            const v = !patrolOpen;
            setPatrolOpen(v);
            localStorage.setItem('fp.patrol.open', v ? '1' : '0');
          }}
        >
          <h3>
            <T zh="巡检" en="Patrol" />
          </h3>
          <span className="sub">
            {patrol?.enabled
              ? patrol?.dueNow
                ? `${patrol.dueNow} 个模型待体检`
                : '自动巡检中，无需处理'
              : '已关闭'}
          </span>
          <span
            style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}
            onClick={(e) => e.stopPropagation()}
          >
            {patrolOpen && (
              <>
                <button
                  className="btn ghost sm"
                  onClick={() => void probe(false)}
                  disabled={(patrol?.remaining ?? 0) === 0}
                >
                  <T zh="立即巡检" en="Run now" />
                </button>
                <button className="btn ghost sm" onClick={() => void probe(true)}>
                  <T zh="全量体检" en="Full sweep" />
                </button>
                <button className="btn ghost sm" onClick={() => setPage('settings')}>
                  <T zh="设置" en="Settings" />
                  <ArrowRight size={12} />
                </button>
              </>
            )}
            <ChevronDown
              size={14}
              style={{
                alignSelf: 'center',
                transform: patrolOpen ? 'rotate(180deg)' : undefined,
                transition: 'transform .15s',
              }}
            />
          </span>
        </div>
        {patrolOpen && (
          <>
            <div className="src-row">
              <span className="nm">
                <T zh="节奏" en="Cadence" />
              </span>
              <span className="meta">
                {patrol?.enabled ? (
                  <T
                    zh={`热 ${patrol.hotHours}h / 冷 ${patrol.coldHours}h · 每日 ${patrol.sweepAt} 全量`}
                    en={`hot ${patrol.hotHours}h / cold ${patrol.coldHours}h · full sweep ${patrol.sweepAt}`}
                  />
                ) : (
                  <T zh="已关闭" en="disabled" />
                )}
              </span>
              <span className="right" />
            </div>
            <div className="src-row">
              <span className="nm">
                <T zh="到期队列" en="Queue" />
              </span>
              <span className="meta">
                <T
                  zh={`${patrol?.dueNow ?? 0} 个模型到期，今日还剩 ${patrol?.remaining ?? 0} 次预算`}
                  en={`${patrol?.dueNow ?? 0} models due, ${patrol?.remaining ?? 0} probes left today`}
                />
              </span>
              <span className="right">
                <Badge tone={(patrol?.remaining ?? 0) > 0 ? 'ok' : 'warn'}>
                  {patrol?.usedToday ?? 0} / {patrol?.budgetToday ?? 0}
                </Badge>
                <span className="ev-time">{hhmm(patrol?.lastRunAt)}</span>
              </span>
            </div>
            <div className="src-row">
              <span className="nm">
                <T zh="让路规则" en="Yielding" />
              </span>
              <span className="meta">
                <T
                  zh="有真实请求在途时巡检暂停；免费版每天 5 次探针，Pro 按日配额 1% 自动扩展至 2400"
                  en="Patrol pauses while real requests are in flight; 5 probes/day on free, Pro scales to 2400"
                />
              </span>
              <span className="right">
                <Status tone={(patrol?.inflight ?? 0) > 0 ? 'warn' : 'ok'}>
                  {(patrol?.inflight ?? 0) > 0
                    ? `${patrol?.inflight} 个请求在途`
                    : 'idle'}
                </Status>
              </span>
            </div>
          </>
        )}
      </div>

      <div className="card">
        <div className="card-head">
          <h3>
            <T zh="最近请求" en="Recent requests" />
          </h3>
          <span className="sub">
            <T zh="点击日志页看完整接力轨迹" en="open Logs for full trail" />
          </span>
          <button className="btn ghost sm" style={{ marginLeft: 'auto' }} onClick={() => setPage('logs')}>
            <T zh="查看全部" en="View all" />
            <ArrowRight size={12} />
          </button>
        </div>
        <table>
          <thead>
            <tr>
              <th style={{ width: 78 }}>
                <T zh="时间" en="TIME" />
              </th>
              <th>
                <T zh="模型" en="MODEL" />
              </th>
              <th style={{ width: 110 }}>
                <T zh="厂商" en="PROVIDER" />
              </th>
              <th style={{ width: 86 }}>
                <T zh="状态" en="STATUS" />
              </th>
              <th style={{ width: 80 }} className="right">
                <T zh="Token" en="TOKENS" />
              </th>
              <th style={{ width: 76 }} className="right">
                <T zh="首字延迟" en="TTFB" />
              </th>
            </tr>
          </thead>
          <tbody>
            {logs.slice(0, 6).map((l) => (
              <tr key={l.id} className="log-row" onClick={() => setPage('logs')}>
                <td className="num">{hhmm(l.at)}</td>
                <td>
                  <div className="t-main">{l.model ?? l.requestedModel}</div>
                  <div className="t-sub">
                    {l.attempts.length > 1 ? (
                      <T zh={`${l.attempts.length} 跳接力`} en={`${l.attempts.length} hops`} />
                    ) : (
                      <T zh="直连" en="direct" />
                    )}
                  </div>
                </td>
                <td className="num">{l.provider ?? '—'}</td>
                <td>
                  {l.ok ? (
                    <Status tone="ok">{l.status}</Status>
                  ) : (
                    <Status tone="bad">{l.status || 'ERR'}</Status>
                  )}
                </td>
                <td className="num right">
                  {(l.usage?.total_tokens ?? 0).toLocaleString()}
                </td>
                <td className="num right">{fmt.ms(l.ms)}</td>
              </tr>
            ))}
            {logs.length === 0 && (
              <tr>
                <td colSpan={6}>
                  <div className="empty">
                    <Activity />
                    <p>
                      <T
                        zh="还没有请求。把 Agent 指向上面的地址，或直接 curl 一次。"
                        en="No requests yet. Point an agent at the address above, or just curl it."
                      />
                    </p>
                  </div>
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
