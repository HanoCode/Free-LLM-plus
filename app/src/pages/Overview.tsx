import { Activity, ArrowRight, Gauge, Radar, ShieldCheck, Zap } from 'lucide-react';
import { T } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { Badge, CopyBtn, Status, fmt, toast } from '../components/ui';
import { QuotaPanel } from '../components/QuotaPanel';

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

export function Overview() {
  const { setPage } = useApp();
  const { status, providers, models, logs, patrol, quota, runPatrol } = useGateway();

  const connected = providers.filter((p) => p.hasKey);
  // 凭据是真正的记账单位：同一家两把 key 算两条通道
  const credentials = providers.reduce(
    (n, p) => n + p.accounts.filter((a) => a.hasKey).length,
    0,
  );
  const rows = models.filter((m) => m.freeport?.upstreamModel);
  const ready = rows.filter((m) => m.freeport?.health === 'healthy').length;
  const addr = `http://${status?.bind ?? '127.0.0.1'}:${status?.port ?? 8787}/v1`;

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
          <CopyBtn value={addr} label="" />
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

      <div className="card" style={{ marginBottom: 14 }}>
        <div className="card-head">
          <h3>
            <T zh="巡检" en="Patrol" />
          </h3>
          <span className="sub">
            {patrol?.enabled ? (
              <T
                zh={`热 ${patrol.hotHours}h / 冷 ${patrol.coldHours}h · 每日 ${patrol.sweepAt} 全量`}
                en={`hot ${patrol.hotHours}h / cold ${patrol.coldHours}h · full sweep ${patrol.sweepAt}`}
              />
            ) : (
              <T zh="已关闭" en="disabled" />
            )}
          </span>
          <button
            className="btn ghost sm"
            style={{ marginLeft: 'auto' }}
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
              zh="有真实请求在途时巡检暂停；预算按日配额 1% 计算，下限 60 上限 800"
              en="Patrol pauses while real requests are in flight; budget is 1% of daily quota, clamped to 60–800"
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
              <th>MODEL</th>
              <th style={{ width: 110 }}>PROVIDER</th>
              <th style={{ width: 86 }}>
                <T zh="状态" en="STATUS" />
              </th>
              <th style={{ width: 80 }} className="right">
                TOKENS
              </th>
              <th style={{ width: 76 }} className="right">
                TTFB
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
