import { Activity, ArrowRight, Gauge, ShieldCheck, Zap } from 'lucide-react';
import { T } from '../lib/i18n';
import { useApp } from '../lib/store';
import { Badge, CopyBtn, Status, fmt, toast } from '../components/ui';
import { catalogEvents, logs, quotaWindows, traffic24h } from '../lib/data';

export function Overview() {
  const { setPage } = useApp();
  const max = Math.max(...traffic24h);
  const peak = traffic24h.indexOf(max);

  return (
    <>
      <div className="hero">
        <div className="hero-ico">
          <ShieldCheck />
        </div>
        <div>
          <h2>
            <T zh="网关运行中 · 一切正常" en="Gateway healthy" />
          </h2>
          <p>
            <T
              zh="11 个厂商、14 个模型可用；最近一次全量审计 8 小时前。"
              en="11 providers, 14 models ready. Last full audit 8h ago."
            />
          </p>
        </div>
        <div className="addr-pill">
          <span>http://127.0.0.1:8787/v1</span>
          <CopyBtn value="http://127.0.0.1:8787/v1" label="" />
        </div>
        <div className="hero-stats">
          <div className="hero-stat">
            <b>14</b>
            <span>
              <T zh="可用模型" en="MODELS" />
            </span>
          </div>
          <div className="hero-stat">
            <b>312ms</b>
            <span>
              <T zh="P50 首字节" en="P50 TTFB" />
            </span>
          </div>
          <div className="hero-stat">
            <b>4</b>
            <span>
              <T zh="已注入 Agent" en="AGENTS" />
            </span>
          </div>
        </div>
      </div>

      <div className="metrics">
        <div className="card metric">
          <div className="k">
            <Activity size={12} />
            <T zh="今日请求" en="Requests today" />
          </div>
          <div className="v">
            1,284 <span className="delta up">+12%</span>
          </div>
          <div className="t-sub">
            <T zh="昨日同期 1,146" en="1,146 yesterday" />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <Zap size={12} />
            <T zh="Token 消耗" en="Tokens" />
          </div>
          <div className="v">
            1.84M
            <small>
              <T zh="免费" en="free" />
            </small>
          </div>
          <div className="t-sub">
            <T zh="折合 API 价约 ¥41" en="≈ $5.7 at list price" />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <Gauge size={12} />
            <T zh="路由成功率" en="Success rate" />
          </div>
          <div className="v">
            97.4%
            <span className="delta up">+0.6</span>
          </div>
          <div className="t-sub">
            <T zh="relay 2.6% · 主要 429" en="2.6% relayed · mostly 429" />
          </div>
        </div>
        <div className="card metric">
          <div className="k">
            <ArrowRight size={12} />
            <T zh="平均跳数" en="Avg. hops" />
          </div>
          <div className="v">
            1.18
            <small>
              <T zh="次/请求" en="per req" />
            </small>
          </div>
          <div className="t-sub">
            <T zh="接力开销 340ms/日累计" en="340ms/day relay cost" />
          </div>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 14 }}>
        <div className="card-head">
          <h3>
            <T zh="今日变更" en="Changes today" />
          </h3>
          <span className="sub">
            <T zh="昨天 03:00 审计 · 探针 137 / 300" en="audit 03:00 · probes 137 / 300" />
          </span>
          <button
            className="btn ghost sm"
            style={{ marginLeft: 10 }}
            onClick={() => toast('已暂停自动应用，等待确认 / Auto-apply paused')}
          >
            <T zh="暂停自动应用" en="Pause auto-apply" />
          </button>
          <button className="btn ghost sm" onClick={() => setPage('discover')}>
            <T zh="全部" en="All" />
            <ArrowRight size={12} />
          </button>
        </div>
        {catalogEvents.map((e, i) => (
          <div className="ev-row" key={i}>
            <div className={`ev-ico ${e.kind}`}>
              {e.kind === 'add' ? '+3' : e.kind === 'del' ? '−1' : e.kind === 'back' ? '↺' : '%'}
            </div>
            <div className="ev-body">
              <div className="ev-t">
                <T zh={e.label_zh} en={e.label_en} />
              </div>
              <div className="ev-s">{e.detail}</div>
            </div>
            <Badge tone={e.badge.tone as 'ok'}>
              <T zh={e.badge.zh} en={e.badge.en} />
            </Badge>
            <span className="ev-time">{e.time}</span>
          </div>
        ))}
      </div>

      <div className="cols-2">
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="请求量 · 最近 24 小时" en="Requests · last 24h" />
            </h3>
            <span className="sub">
              <T zh="峰值 11:00 · 52 次/30min" en="peak 11:00 · 52 per 30m" />
            </span>
          </div>
          <div className="card-body">
            <div className="chart">
              {traffic24h.map((v, i) => (
                <div
                  key={i}
                  className={`bar ${i === peak ? 'hi' : ''}`}
                  style={{ height: `${(v / max) * 100}%` }}
                  title={`${v} req`}
                />
              ))}
            </div>
            <div className="chart-x">
              <span>00:00</span>
              <span>06:00</span>
              <span>12:00</span>
              <span>18:00</span>
              <span>now</span>
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="额度余量" en="Quota headroom" />
            </h3>
            <span className="sub">
              <T zh="最低四窗口" en="lowest window" />
            </span>
          </div>
          <div className="card-body">
            {quotaWindows.map((q) => (
              <div className="quota-row" key={q.name}>
                <span className="nm">{q.name}</span>
                <div className="track">
                  <i className={q.pct > 0.7 ? 'warn' : ''} style={{ width: `${q.pct * 100}%` }} />
                </div>
                <span className="pc">{fmt.pct(q.pct)}</span>
              </div>
            ))}
            <div className="hint-line">
              <T
                zh="余量低于 20% 开始线性降权，不会被直接判死。"
                en="Below 20% a model is down-weighted linearly, never killed."
              />
            </div>
          </div>
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
            {logs.slice(0, 5).map((l) => (
              <tr key={l.id} className="log-row" onClick={() => setPage('logs')}>
                <td className="num">{l.time}</td>
                <td>
                  <div className="t-main">{l.model}</div>
                  <div className="t-sub">
                    {l.hops.length > 1 ? (
                      <T zh={`${l.hops.length} 跳接力`} en={`${l.hops.length} hops`} />
                    ) : (
                      <T zh="直连" en="direct" />
                    )}
                  </div>
                </td>
                <td className="num">{l.provider}</td>
                <td>
                  {l.status === 'ok' && <Status tone="ok">200</Status>}
                  {l.status === 'retry' && <Status tone="warn">{l.code}</Status>}
                  {l.status === 'fail' && <Status tone="bad">{l.code}</Status>}
                </td>
                <td className="num right">{l.tokens.toLocaleString()}</td>
                <td className="num right">{fmt.ms(l.ms)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </>
  );
}
