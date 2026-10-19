import { Fragment, useState } from 'react';
import { ChevronDown, ChevronRight, Download, Search } from 'lucide-react';
import { T } from '../lib/i18n';
import { logs } from '../lib/data';
import { Status, fmt, toast } from '../components/ui';

export function Logs() {
  const [open, setOpen] = useState<string | null>('l2');
  const [q, setQ] = useState('');

  const list = q.trim()
    ? logs.filter((l) => (l.model + l.provider).toLowerCase().includes(q.toLowerCase()))
    : logs;

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
          <button className="btn ghost" onClick={() => toast('已导出近 7 天日志（脱敏） / Exported 7d')}>
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
          <T zh="保留 7 天 · 共 8,412 条" en="7 days · 8,412 rows" />
        </span>
      </div>

      <div className="card">
        <table>
          <thead>
            <tr>
              <th style={{ width: 34 }} />
              <th style={{ width: 78 }}>
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
                  <td className="num right">{l.tokens.toLocaleString()}</td>
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
                        <div className="trail-item" style={{ marginTop: 4, paddingTop: 8, borderTop: '1px dashed var(--line)' }}>
                          <span className="n">Σ</span>
                          <span className="msg faint">
                            <T
                              zh={`总耗时 ${l.ms}ms · 预算 45s · 余 ${(45 - l.ms / 1000).toFixed(1)}s · trail 已签名`}
                              en={`total ${l.ms}ms · budget 45s · ${(45 - l.ms / 1000).toFixed(1)}s left · trail signed`}
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

      <div className="hint-line">
        <T
          zh="响应头 X-Freeport-Trail 带每跳的模型、错误分类、耗时与总预算，出问题直接贴给上游就能对齐。"
          en="The X-Freeport-Trail response header carries per-hop model, error class, latency and remaining budget."
        />
      </div>
    </>
  );
}
