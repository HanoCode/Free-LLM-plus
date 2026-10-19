import { Fragment, useState } from 'react';
import { ChevronDown, ChevronRight, Download, Search } from 'lucide-react';
import { T } from '../lib/i18n';
import { logs as mockLogs } from '../lib/data';
import { Status, fmt, toast } from '../components/ui';
import { useGateway } from '../lib/gateway';
import type { GwLog } from '../lib/bridge';

interface Row {
  id: string;
  time: string;
  model: string;
  provider: string;
  status: 'ok' | 'retry' | 'fail';
  code: string;
  tokens: number;
  ms: number;
  hops: { n: number; provider: string; model: string; code: string; reason: string; scope: string; ms: number }[];
  stream: boolean;
}

function toRow(l: GwLog): Row {
  return {
    id: l.id,
    time: new Date(l.at).toLocaleTimeString('en-GB', { hour12: false }),
    model: l.model || l.requestedModel,
    provider: l.provider || '—',
    status: l.ok ? 'ok' : l.attempts.length > 1 ? 'retry' : 'fail',
    code: String(l.status || 'ERR'),
    tokens: l.usage?.total_tokens || 0,
    ms: l.ms,
    stream: l.stream,
    hops: l.attempts.map((a, i) => ({
      n: i,
      provider: a.provider,
      model: a.model,
      code: a.status ? String(a.status) : 'ERR',
      reason: a.kind,
      scope: a.scope || (a.kind === 'ok' ? 'success' : '—'),
      ms: a.ms,
    })),
  };
}

export function Logs() {
  const [open, setOpen] = useState<string | null>(null);
  const [q, setQ] = useState('');
  const desktop = useGateway((s) => s.available);
  const gwLogs = useGateway((s) => s.logs);

  const list: Row[] = (desktop ? gwLogs.map(toRow) : (mockLogs as unknown as Row[])).filter((l) =>
    q.trim() ? (l.model + l.provider).toLowerCase().includes(q.toLowerCase()) : true,
  );

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="日志" en="Logs" />
          </h1>
          <p>
            <T
              zh="每一跳都记下来：为什么跳、跳去哪、花了多久。这段日志是判断网关是否可信的唯一依据。"
              en="Every hop is recorded: why, where to, how long. This log is the only evidence the gateway can be trusted."
            />
          </p>
        </div>
        <div className="head-actions">
          <button
            className="btn ghost"
            onClick={() => {
              const blob = JSON.stringify(desktop ? gwLogs : mockLogs, null, 2);
              void navigator.clipboard?.writeText(blob);
              toast('已复制脱敏日志 JSON / Copied redacted JSON');
            }}
          >
            <Download />
            <T zh="导出" en="Export" />
          </button>
        </div>
      </div>

      <div className="toolbar">
        <div className="search">
          <Search />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="搜索模型 / 厂商… / Search"
          />
        </div>
        <div className="spacer" />
        <span className="badge gray">
          {desktop ? (
            <T zh={`内存保留最近 ${gwLogs.length} 条`} en={`last ${gwLogs.length} requests in memory`} />
          ) : (
            <T zh="演示数据 · 未连接桌面网关" en="demo data · gateway offline" />
          )}
        </span>
      </div>

      {desktop && list.length === 0 ? (
        <div className="card" style={{ padding: '36px 24px', textAlign: 'center' }}>
          <div style={{ fontSize: 13.5, fontWeight: 600 }}>
            <T zh="还没有请求进来" en="No requests yet" />
          </div>
          <div className="muted" style={{ marginTop: 6, fontSize: 12.5 }}>
            <T
              zh="在终端跑一次下面这条命令，这里就会出现真实的链路与耗时。"
              en="Run the command below once — the real trail will show up here."
            />
          </div>
          <pre
            className="mono"
            style={{
              margin: '14px auto 0',
              padding: '12px 14px',
              background: 'var(--panel-2)',
              border: '1px solid var(--line)',
              borderRadius: 8,
              display: 'inline-block',
              textAlign: 'left',
              lineHeight: 1.7,
            }}
          >
            curl http://127.0.0.1:{useGateway.getState().status?.port ?? 8787}/v1/chat/completions \
            {'\n  '}-H 'content-type: application/json' \
            {'\n  '}-d '{`{"model":"auto","messages":[{"role":"user","content":"hi"}]}`}'
          </pre>
        </div>
      ) : (
        <div className="card">
          <table>
            <thead>
              <tr>
                <th style={{ width: 34 }} />
                <th style={{ width: 86 }}>
                  <T zh="时间" en="TIME" />
                </th>
                <th>MODEL</th>
                <th style={{ width: 112 }}>PROVIDER</th>
                <th style={{ width: 88 }}>
                  <T zh="状态" en="STATUS" />
                </th>
                <th style={{ width: 74 }} className="right">
                  TOKENS
                </th>
                <th style={{ width: 78 }} className="right">
                  TTFB
                </th>
                <th style={{ width: 70 }} className="right">
                  <T zh="跳数" en="HOPS" />
                </th>
              </tr>
            </thead>
            <tbody>
              {list.map((l) => (
                <Fragment key={l.id}>
                  <tr className="log-row" onClick={() => setOpen(open === l.id ? null : l.id)}>
                    <td className="faint">{open === l.id ? <ChevronDown size={13} /> : <ChevronRight size={13} />}</td>
                    <td className="num">{l.time}</td>
                    <td>
                      <div className="t-main">{l.model}</div>
                      {l.hops.length > 1 && (
                        <div className="t-sub">
                          <T zh={`${l.hops.length} 跳接力`} en={`${l.hops.length} hops`} />
                        </div>
                      )}
                    </td>
                    <td className="num">{l.provider}</td>
                    <td>
                      {l.status === 'ok' && <Status tone="ok">{l.code}</Status>}
                      {l.status === 'retry' && <Status tone="warn">{l.code}</Status>}
                      {l.status === 'fail' && <Status tone="bad">{l.code}</Status>}
                    </td>
                    <td className="num right">{l.tokens ? l.tokens.toLocaleString() : '—'}</td>
                    <td className="num right">{fmt.ms(l.ms)}</td>
                    <td className="num right">{l.hops.length}</td>
                  </tr>
                  {open === l.id && (
                    <tr className="log-detail" key={l.id + '-d'}>
                      <td colSpan={8}>
                        <div className="trail">
                          {l.hops.map((h) => (
                            <div className="trail-item" key={h.n}>
                              <span className="n">{h.n + 1}</span>
                              <span className="m">
                                {h.provider}/{h.model}
                              </span>
                              <span className="badge gray">{h.code}</span>
                              <span className="msg">
                                {h.reason} · <span className="faint">{h.scope}</span>
                              </span>
                              <span className="num" style={{ marginLeft: 'auto' }}>
                                {h.ms}ms
                              </span>
                            </div>
                          ))}
                          <div
                            className="trail-item"
                            style={{ marginTop: 4, paddingTop: 8, borderTop: '1px dashed var(--line)' }}
                          >
                            <span className="n">Σ</span>
                            <span className="msg faint">
                              <T
                                zh={`总耗时 ${l.ms}ms · ${l.stream ? '流式' : '非流式'} · budget 45s`}
                                en={`total ${l.ms}ms · ${l.stream ? 'stream' : 'buffered'} · budget 45s`}
                              />
                            </span>
                            <span className="num" style={{ marginLeft: 'auto' }}>
                              X-Freeport-Trail
                            </span>
                          </div>
                        </div>
                      </td>
                    </tr>
                  )}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="hint-line">
        <T
          zh="响应头 X-Freeport-Trail 带每跳的模型、错误分类、耗时与总预算，出问题直接贴给上游就能对齐。"
          en="The X-Freeport-Trail response header carries per-hop model, error class, latency and remaining budget."
        />
      </div>
    </>
  );
}
