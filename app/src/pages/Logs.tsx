import { Fragment, useState } from 'react';
import { ChevronDown, ChevronRight, Download, Search, Unplug } from 'lucide-react';
import { T } from '../lib/i18n';
import { Empty, Status, fmt, toast } from '../components/ui';
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
  /** 从哪个协议入口进来的 */
  path: string;
  /** 哪个 key 打进来的（当次名字快照） */
  key: string;
  /** 最终答它的是哪条凭据（同样是快照） */
  account: string;
  hops: {
    n: number;
    provider: string;
    /** 这一跳用的哪条凭据；多账号时才知道是哪个 key 挂的 */
    account: string;
    model: string;
    code: string;
    reason: string;
    scope: string;
    ms: number;
  }[];
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
    path: l.path || '/v1/chat/completions',
    key: l.keyName || '—',
    account: l.accountName || '—',
    hops: l.attempts.map((a, i) => ({
      n: i,
      provider: a.provider,
      account: a.account || '—',
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

  // 只有真日志。拿不到网关就是空表——日志是网关唯一的证据链，
  // 用示例行填满等于把证据也一起伪造了。
  const list: Row[] = gwLogs.map(toRow).filter((l) =>
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
            disabled={!desktop || gwLogs.length === 0}
            onClick={() => {
              const blob = JSON.stringify(gwLogs, null, 2);
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
            <T zh="未连接网关" en="gateway offline" />
          )}
        </span>
      </div>

      {!desktop ? (
        <div className="card">
          <Empty
            icon={<Unplug />}
            title={<T zh="没有连接到网关" en="No gateway connection" />}
            hint={
              <T
                zh="日志是网关在内存里留的，前端不存也不编。桌面版启动时网关自动运行；浏览器预览连不上本机进程，所以这里一条都没有。"
                en="Logs live in gateway memory — the UI neither stores nor invents them. The desktop build runs the gateway automatically; a browser preview cannot reach the local process, so the table is empty."
              />
            }
          />
        </div>
      ) : list.length === 0 ? (
        <div className="card">
          <Empty
            icon={<Search />}
            title={
              q.trim() ? (
                <T zh="没有匹配的请求" en="No matching requests" />
              ) : (
                <T zh="还没有请求进来" en="No requests yet" />
              )
            }
            hint={
              q.trim() ? (
                <T zh="换个模型名或厂商名再搜。" en="Try another model or provider name." />
              ) : (
                <T
                  zh="在终端跑一次下面这条命令，这里就会出现真实的链路与耗时。"
                  en="Run the command below once — the real trail will show up here."
                />
              )
            }
          />
          {!q.trim() && (
            <pre
              className="mono"
              style={{
                margin: '0 auto 22px',
                padding: '12px 14px',
                background: 'var(--panel-2)',
                border: '1px solid var(--line)',
                borderRadius: 8,
                display: 'block',
                width: 'fit-content',
                textAlign: 'left',
                lineHeight: 1.7,
              }}
            >
              curl http://127.0.0.1:{useGateway.getState().status?.port ?? 8787}/v1/chat/completions \
              {'\n  '}-H 'content-type: application/json' \
              {'\n  '}-d '{`{"model":"auto","messages":[{"role":"user","content":"hi"}]}`}'
            </pre>
          )}
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
                            <span className="num" style={{ marginLeft: 'auto', display: 'flex', gap: 10, alignItems: 'center' }}>
                              <span className="mono faint" title="协议入口">
                                {l.path || '/v1/chat/completions'}
                              </span>
                              <span className="mono faint" title="最终答它的凭据">
                                {l.account}
                              </span>
                              <span className="mono" title="发起这次请求的 key">
                                {l.key || '—'}
                              </span>
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
