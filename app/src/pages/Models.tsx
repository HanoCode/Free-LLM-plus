import { useMemo, useState } from 'react';
import { Search, SlidersHorizontal, RefreshCw, Sparkles } from 'lucide-react';
import { T } from '../lib/i18n';
import { models as allModels, type Model } from '../lib/data';
import { Status, Switch, fmt, toast } from '../components/ui';
import { useApp } from '../lib/store';

type Filter = 'all' | 'tools' | 'vision' | 'json' | 'cooling';

export function Models() {
  const [q, setQ] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [onlyReady, setOnlyReady] = useState(false);
  const { setPage } = useApp();

  const list = useMemo(() => {
    let r = allModels;
    if (q.trim()) {
      const s = q.toLowerCase();
      r = r.filter((m) => m.id.toLowerCase().includes(s) || m.provider.includes(s));
    }
    if (filter === 'tools') r = r.filter((m) => m.caps.tools);
    if (filter === 'vision') r = r.filter((m) => m.caps.vision);
    if (filter === 'json') r = r.filter((m) => m.caps.json);
    if (filter === 'cooling') r = r.filter((m) => m.status !== 'healthy');
    if (onlyReady) r = r.filter((m) => m.status === 'healthy');
    return r;
  }, [q, filter, onlyReady]);

  const count = (f: Filter) => {
    if (f === 'all') return allModels.length;
    if (f === 'tools') return allModels.filter((m) => m.caps.tools).length;
    if (f === 'vision') return allModels.filter((m) => m.caps.vision).length;
    if (f === 'json') return allModels.filter((m) => m.caps.json).length;
    return allModels.filter((m) => m.status !== 'healthy').length;
  };

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="模型" en="Models" />
          </h1>
          <p>
            <T
              zh="目录里所有模型。能力标签来自实测探针，不是厂商自称。"
              en="Every model in the catalog. Capability tags come from live probes, not vendor claims."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => toast('已开始重探 3 个异常模型 / Re-probing 3 models')}>
            <RefreshCw />
            <T zh="重探异常" en="Re-probe" />
          </button>
          <button className="btn primary" onClick={() => setPage('discover')}>
            <Sparkles />
            <T zh="添加模型" en="Add models" />
          </button>
        </div>
      </div>

      <div className="toolbar">
        <div className="search">
          <Search />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="搜索模型或厂商… / Search model or provider"
          />
        </div>
        <button className={`filter ${filter === 'all' ? 'on' : ''}`} onClick={() => setFilter('all')}>
          <T zh="全部" en="All" />
          <span className="n">{count('all')}</span>
        </button>
        <button className={`filter ${filter === 'tools' ? 'on' : ''}`} onClick={() => setFilter('tools')}>
          <T zh="工具" en="Tools" />
          <span className="n">{count('tools')}</span>
        </button>
        <button className={`filter ${filter === 'vision' ? 'on' : ''}`} onClick={() => setFilter('vision')}>
          <T zh="视觉" en="Vision" />
          <span className="n">{count('vision')}</span>
        </button>
        <button className={`filter ${filter === 'json' ? 'on' : ''}`} onClick={() => setFilter('json')}>
          JSON
          <span className="n">{count('json')}</span>
        </button>
        <button className={`filter ${filter === 'cooling' ? 'on' : ''}`} onClick={() => setFilter('cooling')}>
          <T zh="异常" en="Issues" />
          <span className="n">{count('cooling')}</span>
        </button>
        <div className="spacer" />
        <Switch on={onlyReady} onChange={setOnlyReady} label={<T zh="只看可用" en="Ready only" />} />
        <button className="icon-btn" title="列设置">
          <SlidersHorizontal />
        </button>
      </div>

      <div className="card">
        <table>
          <thead>
            <tr>
              <th>MODEL</th>
              <th style={{ width: 120 }}>PROVIDER</th>
              <th style={{ width: 80 }} className="right">
                CONTEXT
              </th>
              <th style={{ width: 92 }}>
                <T zh="能力" en="CAPS" />
              </th>
              <th style={{ width: 78 }} className="right">
                TTFB
              </th>
              <th style={{ width: 118 }}>
                <T zh="可靠性" en="RELIABILITY" />
              </th>
              <th style={{ width: 86 }}>
                <T zh="额度" en="QUOTA" />
              </th>
              <th style={{ width: 96 }}>
                <T zh="状态" en="STATUS" />
              </th>
            </tr>
          </thead>
          <tbody>
            {list.map((m) => (
              <Row key={m.id} m={m} />
            ))}
            {list.length === 0 && (
              <tr>
                <td colSpan={8}>
                  <div className="empty">
                    <Search />
                    <p>
                      <T zh="没有匹配的模型" en="No models match" />
                    </p>
                  </div>
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div className="hint-line">
        <T
          zh="能力徽章：T = 工具调用 · V = 视觉 · J = JSON 模式。灰色表示实测不支持，路由时会按请求类型过滤。"
          en="Badges: T = tools · V = vision · J = JSON mode. Grey means probe failed; routing filters by request type."
        />
      </div>
    </>
  );
}

function Row({ m }: { m: Model }) {
  return (
    <tr>
      <td>
        <div className="t-main">{m.name}</div>
        <div className="t-sub">
          {m.meta ?? <T zh="免费额度" en="free tier" />}
        </div>
      </td>
      <td className="num">{m.provider}</td>
      <td className="num right">{fmt.ctx(m.ctx)}</td>
      <td>
        <div className="cap">
          <i className={m.caps.tools ? 'on' : ''}>T</i>
          <i className={m.caps.vision ? 'on' : ''}>V</i>
          <i className={m.caps.json ? 'on' : ''}>J</i>
        </div>
      </td>
      <td className="num right">{fmt.ms(m.latency)}</td>
      <td>
        <span className="mini-track">
          <i
            className={m.reliability < 0.85 ? 'bad' : m.reliability < 0.92 ? 'warn' : ''}
            style={{ width: `${m.reliability * 100}%` }}
          />
        </span>
        <span className="num" style={{ marginLeft: 6 }}>
          {fmt.pct(m.reliability)}
        </span>
      </td>
      <td>
        <span className="mini-track">
          <i className={m.quota > 0.7 ? 'warn' : ''} style={{ width: `${m.quota * 100}%` }} />
        </span>
        <div className="t-sub">{fmt.pct(m.quota)}</div>
      </td>
      <td>
        {m.status === 'healthy' && <Status tone="ok"><T zh="可用" en="ready" /></Status>}
        {m.status === 'cooling' && <Status tone="warn"><T zh="冷却" en="cooling" /></Status>}
        {m.status === 'degraded' && <Status tone="bad"><T zh="降级" en="degraded" /></Status>}
      </td>
    </tr>
  );
}
