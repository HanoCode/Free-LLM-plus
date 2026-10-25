import { useEffect, useState } from 'react';
import { BarChart3, Download, Radar, Zap } from 'lucide-react';
import { T, healthLabel, useLang } from '../lib/i18n';
import { useGateway } from '../lib/gateway';
import { Badge, Empty, Status, fmt, toast } from '../components/ui';
import type { GwBucket, GwDayStat, GwModelIntel, GwModelStat } from '../lib/bridge';

/**
 * 报表页（Pro）：用量报表 + 本机额度情报。
 *
 * 数据全部来自本机：报表基于落盘的 JSONL 日志（最多 90 天），
 * 情报基于内存日志环 + 账本 + model_health 聚合。没有任何云端参与。
 * 非 Pro 不渲染假数据——`pro:false` 时后端直接回空 models，这里进引导卡。
 */

const RANGES = [7, 30, 90] as const;

const PURPOSE_LABEL: Record<string, { zh: string; en: string }> = {
  chat: { zh: '对话', en: 'chat' },
  coding: { zh: '编码', en: 'coding' },
  vision: { zh: '视觉', en: 'vision' },
  probe: { zh: '巡检', en: 'probe' },
};

const HEALTH_TONE: Record<string, 'ok' | 'warn' | 'bad' | 'gray'> = {
  healthy: 'ok',
  degraded: 'warn',
  cooling: 'warn',
  unavailable: 'bad',
  retired: 'bad',
  error: 'bad',
  unknown: 'gray',
};

function tokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return n.toLocaleString();
}

function shortDay(date: string): string {
  // 2026-10-24 → 10-24；解析失败就原样给
  return date.length >= 10 ? date.slice(5) : date;
}

export function Reports() {
  const lang = useLang();
  const zh = lang === 'zh';
  const reports = useGateway((s) => s.reports);
  const intel = useGateway((s) => s.intel);
  const loadReports = useGateway((s) => s.loadReports);
  const loadIntel = useGateway((s) => s.loadIntel);

  const [days, setDays] = useState<number>(7);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void loadReports(days);
    void loadIntel();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [days]);

  const exportReport = async () => {
    const api = window.freeport?.gateway;
    if (!api) return toast(zh ? '导出需要桌面端' : 'Export needs the desktop app');
    setBusy(true);
    try {
      const path = await api.reportsExport();
      if (path) toast(zh ? `已导出到 ${path}` : `Exported to ${path}`);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const daily = reports?.daily ?? [];
  const maxDay = daily.reduce((m, d) => Math.max(m, d.requests), 0) || 1;

  const success =
    reports && reports.requests > 0 ? 1 - reports.errors / reports.requests : null;

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="报表" en="Reports" />
          </h1>
          <p>
            <T
              zh="本机用量报表与额度情报：数据只留在这台机器上，最多 90 天，随时可导出。"
              en="Local usage reports and quota intel: data stays on this machine, up to 90 days, exportable anytime."
            />
          </p>
        </div>
        <div className="head-actions">
          <div className="pool-pick">
            {RANGES.map((r) => (
              <button
                key={r}
                type="button"
                className={`badge ${days === r ? 'accent' : 'gray'}`}
                aria-pressed={days === r}
                onClick={() => setDays(r)}
              >
                {r} <T zh="天" en="d" />
              </button>
            ))}
          </div>
          <button className="btn ghost" disabled={busy || !reports?.pro} onClick={() => void exportReport()}>
            <Download size={13} />
            <T zh="导出日志" en="Export logs" />
          </button>
        </div>
      </div>

      {reports && reports.days === 0 ? (
        <div className="card" style={{ marginBottom: 14 }}>
          <Empty
            icon={<BarChart3 />}
            title={<T zh="还没有留存日志" en="No persisted logs yet" />}
            hint={
              <T
                zh="报表从现在开始记：每条请求按天落盘。发几单请求再回来看，柱状图就有了。"
                en="The archive starts now: each request is written per day. Send a few requests and come back."
              />
            }
          />
        </div>
      ) : reports ? (
        <>
          <div className="metrics">
            <div className="card metric">
              <div className="k">
                <BarChart3 size={12} />
                <T zh="总请求" en="Requests" />
              </div>
              <div className="v">{reports.requests.toLocaleString()}</div>
              <div className="t-sub">
                <T zh={`失败 ${reports.errors} 次 · 留存 ${reports.days} 天`} en={`${reports.errors} failed · ${reports.days} days kept`} />
              </div>
            </div>
            <div className="card metric">
              <div className="k">
                <Zap size={12} />
                <T zh="Token" en="Tokens" />
              </div>
              <div className="v">{tokens(reports.tokensIn + reports.tokensOut)}</div>
              <div className="t-sub">
                <T
                  zh={`入 ${tokens(reports.tokensIn)} · 出 ${tokens(reports.tokensOut)}`}
                  en={`in ${tokens(reports.tokensIn)} · out ${tokens(reports.tokensOut)}`}
                />
              </div>
            </div>
            <div className="card metric">
              <div className="k">
                <Status tone={success === null ? 'idle' : success > 0.9 ? 'ok' : 'warn'}>·</Status>
                <T zh="成功率" en="Success rate" />
              </div>
              <div className="v">{success === null ? '—' : fmt.pct(success)}</div>
              <div className="t-sub">
                <T zh="近 90 天落盘记录" en="from persisted records" />
              </div>
            </div>
          </div>

          {daily.length > 0 && (
            <div className="card" style={{ marginBottom: 14 }}>
              <div className="card-head">
                <h3>
                  <T zh="逐天请求" en="Daily requests" />
                </h3>
                <span className="sub">
                  <T
                    zh={`最近 ${Math.min(days, daily.length)} 天 · 峰值 ${maxDay} 单/天`}
                    en={`last ${Math.min(days, daily.length)} days · peak ${maxDay}/day`}
                  />
                </span>
              </div>
              <div className="card-body">
                <DailyBars daily={daily} max={maxDay} />
              </div>
            </div>
          )}

          {reports.topModels.length > 0 && (
            <div className="card" style={{ marginBottom: 14 }}>
              <div className="card-head">
                <h3>
                  <T zh="最常用模型" en="Top models" />
                </h3>
                <span className="sub">
                  <T zh="按请求数" en="by requests" />
                </span>
              </div>
              <table>
                <thead>
                  <tr>
                    <th>
                      <T zh="模型" en="MODEL" />
                    </th>
                    <th style={{ width: 110 }}>
                      <T zh="厂商" en="PROVIDER" />
                    </th>
                    <th style={{ width: 84 }} className="right">
                      <T zh="请求" en="REQS" />
                    </th>
                    <th style={{ width: 70 }} className="right">
                      <T zh="失败" en="ERR" />
                    </th>
                    <th style={{ width: 96 }} className="right">
                      <T zh="Token" en="TOKENS" />
                    </th>
                    <th style={{ width: 76 }} className="right">
                      <T zh="均耗时" en="AVG" />
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {reports.topModels.map((m) => (
                    <ModelRow key={`${m.provider}:${m.model}`} m={m} />
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {(reports.byPurpose.length > 0 || reports.byKey.length > 0) && (
            <div className="card" style={{ marginBottom: 14 }}>
              <div className="card-head">
                <h3>
                  <T zh="按用途与凭据" en="By purpose & key" />
                </h3>
              </div>
              <div className="card-body" style={{ display: 'flex', flexWrap: 'wrap', gap: 18 }}>
                {reports.byPurpose.length > 0 && (
                  <div style={{ minWidth: 220 }}>
                    <div className="t-sub" style={{ marginBottom: 8 }}>
                      <T zh="按用途池" en="By purpose pool" />
                    </div>
                    <BucketRows buckets={reports.byPurpose} labelMap={PURPOSE_LABEL} />
                  </div>
                )}
                {reports.byKey.length > 0 && (
                  <div style={{ minWidth: 220, flex: 1 }}>
                    <div className="t-sub" style={{ marginBottom: 8 }}>
                      <T zh="按虚拟 Key" en="By virtual key" />
                    </div>
                    <BucketRows buckets={reports.byKey} labelMap={{}} />
                  </div>
                )}
              </div>
            </div>
          )}
        </>
      ) : (
        <div className="card" style={{ marginBottom: 14 }}>
          <Empty icon={<BarChart3 />} title={<T zh="加载中…" en="Loading…" />} />
        </div>
      )}

      {/* ── 本机额度情报：最近请求 + 账本 + 健康度的交叉视角 ── */}
      <div className="card">
        <div className="card-head">
          <h3>
            <T zh="额度情报" en="Quota intel" />
          </h3>
          <span className="sub">
            {intel ? (
              <T
                zh={`最近 ${intel.scanned} 条请求 × 账本 × 健康度`}
                en={`last ${intel.scanned} requests × ledger × health`}
              />
            ) : (
              <T zh="内存日志环，实时聚合" en="aggregated from the in-memory log ring" />
            )}
          </span>
        </div>
        {!intel || intel.models.length === 0 ? (
          <div className="card-body">
            <Empty
              icon={<Radar />}
              title={<T zh="还没有可聚合的请求" en="Nothing to aggregate yet" />}
              hint={<T zh="发几单请求，这里会给出每条通道的真实表现。" en="Send a few requests and each channel's real behaviour shows up here." />}
            />
          </div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>
                  <T zh="模型" en="MODEL" />
                </th>
                <th style={{ width: 96 }} className="right">
                  <T zh="请求" en="REQS" />
                </th>
                <th style={{ width: 72 }} className="right">
                  <T zh="成功率" en="OK" />
                </th>
                <th style={{ width: 128 }} className="right">
                  <T zh="延迟 p50 / p95" en="p50 / p95" />
                </th>
                <th style={{ width: 130 }}>
                  <T zh="剩余额度" en="Headroom" />
                </th>
                <th style={{ width: 92 }}>
                  <T zh="健康" en="HEALTH" />
                </th>
              </tr>
            </thead>
            <tbody>
              {intel.models.map((m) => (
                <IntelRow key={`${m.provider}:${m.model}`} m={m} />
              ))}
            </tbody>
          </table>
        )}
        <div className="hint-line" style={{ padding: '0 16px 14px' }}>
          <T
            zh="情报来自内存日志环（最近 300 条），与报表的落盘档案不是一个口径：这边看「此刻谁在挂、谁快没额度」，报表看「过去几天用了多少」。"
            en="Intel comes from the in-memory ring (last 300) and reads differently from reports: this is 'who is failing and running low right now', reports are 'how much was used over days'."
          />
        </div>
      </div>
    </>
  );
}

/* ── 片段 ─────────────────────────────────── */

function DailyBars({ daily, max }: { daily: GwDayStat[]; max: number }) {
  const step = Math.max(1, Math.ceil(daily.length / 14));
  return (
    <div style={{ display: 'flex', alignItems: 'flex-end', gap: 3, height: 120 }}>
      {daily.map((d, i) => {
        const h = d.requests === 0 ? 2 : Math.max(4, Math.round((d.requests / max) * 104));
        const errRate = d.requests > 0 ? d.errors / d.requests : 0;
        const tone = errRate === 0 ? 'var(--accent, #4b7bec)' : errRate > 0.3 ? 'var(--bad, #d64545)' : 'var(--warn, #c98a2e)';
        const tip = `${d.date} · ${d.requests} req · ${d.errors} err · ${tokens(d.tokensIn + d.tokensOut)} tok`;
        return (
          <div key={d.date} style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 4, minWidth: 0 }}>
            <i
              title={tip}
              style={{ display: 'block', width: '100%', maxWidth: 22, height: h, background: tone, borderRadius: 3, opacity: 0.85 }}
            />
            {(i % step === 0 || i === daily.length - 1) && (
              <span className="faint" style={{ fontSize: 9.5, whiteSpace: 'nowrap' }}>
                {shortDay(d.date)}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

function ModelRow({ m }: { m: GwModelStat }) {
  return (
    <tr>
      <td>
        <div className="t-main">{m.model}</div>
        {m.errors > 0 && (
          <div className="t-sub">
            <T zh={`失败率 ${fmt.pct(m.errors / m.requests)}`} en={`${fmt.pct(m.errors / m.requests)} failed`} />
          </div>
        )}
      </td>
      <td className="num">{m.provider}</td>
      <td className="num right">{m.requests.toLocaleString()}</td>
      <td className="num right">{m.errors ? <span style={{ color: 'var(--bad)' }}>{m.errors}</span> : '0'}</td>
      <td className="num right">{tokens(m.tokensIn + m.tokensOut)}</td>
      <td className="num right">{m.avgOkMs > 0 ? fmt.ms(m.avgOkMs) : '—'}</td>
    </tr>
  );
}

function IntelRow({ m }: { m: GwModelIntel }) {
  return (
    <tr title={m.lastError ? `last error: ${m.lastError}` : undefined}>
      <td>
        <div className="t-main">{m.model}</div>
        <div className="t-sub">{m.provider}</div>
      </td>
      <td className="num right">
        {m.requests.toLocaleString()}
        {m.errors > 0 && <span style={{ color: 'var(--bad)' }}> · {m.errors}err</span>}
      </td>
      <td className="num right">{m.requests > 0 ? fmt.pct(m.okRate) : '—'}</td>
      <td className="num right">
        {fmt.ms(m.p50Ms)} / {fmt.ms(m.p95Ms)}
      </td>
      <td>
        {m.remaining === null ? (
          <span className="faint">—</span>
        ) : (
          <span className="axis r" title={`${Math.round(m.remaining * 100)}%`}>
            <i
              style={{
                width: `${Math.round(m.remaining * 100)}%`,
                background: m.remaining < 0.15 ? 'var(--bad)' : m.remaining < 0.4 ? 'var(--warn)' : 'var(--accent, #4b7bec)',
              }}
            />
          </span>
        )}
      </td>
      <td>
        <Badge tone={HEALTH_TONE[m.health ?? 'unknown'] ?? 'gray'}>
          <T
            zh={healthLabel(m.health ?? 'unknown', 'zh')}
            en={healthLabel(m.health ?? 'unknown', 'en')}
          />
        </Badge>
      </td>
    </tr>
  );
}

function BucketRows({ buckets, labelMap }: { buckets: GwBucket[]; labelMap: Record<string, { zh: string; en: string }> }) {
  const max = buckets.reduce((m, b) => Math.max(m, b.requests), 0) || 1;
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
      {buckets.map((b) => {
        const label = labelMap[b.key] ?? { zh: b.key, en: b.key };
        return (
          <div key={b.key} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span className="sub" style={{ width: 96, flex: 'none', textAlign: 'right' }}>
              <T zh={label.zh} en={label.en} />
            </span>
            <span className="axis r" style={{ flex: 1 }} title={`${b.requests}`}>
              <i style={{ width: `${Math.round((b.requests / max) * 100)}%` }} />
            </span>
            <b style={{ fontSize: 11.5, width: 52, textAlign: 'right' }}>{b.requests.toLocaleString()}</b>
          </div>
        );
      })}
    </div>
  );
}
