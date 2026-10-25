import { useMemo, useState } from 'react';
import { Activity, Search, RefreshCw, Sparkles } from 'lucide-react';
import { T, healthLabel, tx, useLang } from '../lib/i18n';
import { Status, Switch, fmt, toast } from '../components/ui';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import type { GwModel, HealthStatus } from '../lib/bridge';

type Filter = 'all' | 'free' | 'tools' | 'vision' | 'issues';

/** 一行模型 = 一个 provider × 一个上游模型。虚拟模型（freeport/auto）不在这里。 */
interface Row {
  key: string;
  provider: string;
  providerName: string;
  model: string;
  context?: number;
  tools: boolean;
  vision: boolean;
  json: boolean;
  probed: boolean;
  free: boolean;
  freeTier: boolean;
  latency?: number;
  status: HealthStatus;
  /** feed 新加、还在 canary 里 */
  candidate: boolean;
  canary: number;
  /** 模型组：同名模型还有哪几家也能跑 */
  alsoOn?: string[];
}

const toneOf = (s: HealthStatus): 'ok' | 'warn' | 'bad' | 'idle' => {
  if (s === 'healthy') return 'ok';
  if (s === 'degraded' || s === 'cooling' || s === 'probing') return 'warn';
  if (s === 'unavailable' || s === 'error' || s === 'retired') return 'bad';
  return 'idle';
};

/* 状态说法统一收在 lib/i18n 的 healthLabel 里：路由候选链、报表、模型页共用一套词 */
const labelOf = (s: HealthStatus) => ({
  zh: healthLabel(s, 'zh'),
  en: healthLabel(s, 'en'),
});

/** 能力徽章：hover 显示含义与实测结论 */
function CapBadge({
  k,
  zh,
  en,
  on,
  lang,
}: {
  k: string;
  zh: string;
  en: string;
  on: boolean;
  lang: 'zh' | 'en';
}) {
  return (
    <i
      className={on ? 'on' : ''}
      data-tip={tx(
        lang,
        `${zh}（${k}）· ${on ? '支持' : '不支持'}`,
        `${en} (${k}) · ${on ? 'yes' : 'no'}`,
      )}
    >
      {k}
    </i>
  );
}

export function Models() {
  const lang = useLang();
  const [q, setQ] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [onlyReady, setOnlyReady] = useState(false);
  // 只看免费：默认开（本产品的核心就是免费额度），用户关掉后记住选择
  const [freeOnly, setFreeOnly] = useState(() => localStorage.getItem('freeport.models.freeOnly') !== '0');
  const [busy, setBusy] = useState<string | null>(null);
  const { setPage } = useApp();
  const { models, providers, probeFailed, probeModel } = useGateway();

  const toggleFreeOnly = (on: boolean) => {
    setFreeOnly(on);
    localStorage.setItem('freeport.models.freeOnly', on ? '1' : '0');
  };

  const rows = useMemo<Row[]>(() => {
    const names = new Map(providers.map((p) => [p.id, p]));
    return models
      .filter((m: GwModel) => m.freeport?.upstreamModel)
      .map((m) => {
        const provider = m.freeport?.provider ?? m.owned_by;
        const p = names.get(provider);
        const caps = m.freeport?.caps;
        return {
          key: m.id,
          provider,
          providerName: p?.name ?? provider,
          model: m.freeport?.upstreamModel ?? m.id,
          context: m.context_length,
          tools: caps?.tools === true,
          vision: caps?.vision === true,
          json: caps?.json === true,
          probed: m.freeport?.capsProbed === true,
          free: m.freeport?.free === true,
          freeTier: p?.freeTier === true,
          latency: m.freeport?.latencyMs,
          status: (m.freeport?.health as HealthStatus) ?? 'unknown',
          candidate: m.freeport?.candidate === true,
          canary: m.freeport?.canary ?? 0,
          alsoOn: m.freeport?.alsoOn,
        };
      });
  }, [models, providers]);

  const list = useMemo(() => {
    let r = rows;
    if (q.trim()) {
      const s = q.toLowerCase();
      r = r.filter(
        (m) =>
          m.model.toLowerCase().includes(s) ||
          m.providerName.toLowerCase().includes(s) ||
          m.provider.includes(s),
      );
    }
    if (filter === 'free') r = r.filter((m) => m.free);
    if (filter === 'tools') r = r.filter((m) => m.tools);
    if (filter === 'vision') r = r.filter((m) => m.vision);
    if (filter === 'issues') r = r.filter((m) => !['healthy', 'unknown'].includes(m.status));
    if (onlyReady) r = r.filter((m) => m.status === 'healthy');
    // 只看免费 = 只留真正零价的模型（「额度」徽标仅代表厂商有免费档，不算免费）
    if (freeOnly) r = r.filter((m) => m.free);
    return [...r].sort((a, b) => {
      const rank = (m: Row) => (m.status === 'healthy' ? 0 : m.status === 'unknown' ? 1 : 2);
      return rank(a) - rank(b) || a.provider.localeCompare(b.provider) || a.model.localeCompare(b.model);
    });
  }, [rows, q, filter, onlyReady, freeOnly]);

  const count = (f: Filter) => {
    if (f === 'all') return rows.length;
    if (f === 'free') return rows.filter((m) => m.free).length;
    if (f === 'tools') return rows.filter((m) => m.tools).length;
    if (f === 'vision') return rows.filter((m) => m.vision).length;
    return rows.filter((m) => !['healthy', 'unknown'].includes(m.status)).length;
  };

  const reProbe = async () => {
    setBusy('failed');
    try {
      const r = await probeFailed();
      toast(
        r.skipped
          ? `${r.skipped} / ${r.skipped}`
          : `重探 ${r.ran} 个 · 恢复 ${r.ok} / 仍失败 ${r.failed}`,
      );
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  };

  const probeOne = async (row: Row) => {
    setBusy(row.key);
    try {
      const r = await probeModel(row.provider, row.model, false);
      toast(
        r.ok
          ? `${row.model} · ${r.latencyMs}ms`
          : `${row.model} 失败：${r.error ?? r.status}`,
      );
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
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
              zh="目录里所有可用模型。能力徽章来自实测探针，不是厂商自称。"
              en="Every model you can route to. Capability badges come from live probes, not vendor claims."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={reProbe} disabled={busy !== null}>
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
        <button className={`filter ${filter === 'free' ? 'on' : ''}`} onClick={() => setFilter('free')}>
          <T zh="免费" en="Free" />
          <span className="n">{count('free')}</span>
        </button>
        <button className={`filter ${filter === 'tools' ? 'on' : ''}`} onClick={() => setFilter('tools')}>
          <T zh="工具" en="Tools" />
          <span className="n">{count('tools')}</span>
        </button>
        <button className={`filter ${filter === 'vision' ? 'on' : ''}`} onClick={() => setFilter('vision')}>
          <T zh="视觉" en="Vision" />
          <span className="n">{count('vision')}</span>
        </button>
        <button className={`filter ${filter === 'issues' ? 'on' : ''}`} onClick={() => setFilter('issues')}>
          <T zh="异常" en="Issues" />
          <span className="n">{count('issues')}</span>
        </button>
        <div className="spacer" />
        <Switch on={freeOnly} onChange={toggleFreeOnly} label={<T zh="只看免费" en="Free only" />} />
        <Switch on={onlyReady} onChange={setOnlyReady} label={<T zh="只看可用" en="Ready only" />} />
      </div>

      <div className="card">
        <table>
          <thead>
            <tr>
              <th>
                <T zh="模型" en="MODEL" />
              </th>
              <th style={{ width: 120 }}>
                <T zh="厂商" en="PROVIDER" />
              </th>
              <th style={{ width: 80 }} className="right">
                <T zh="上下文" en="CONTEXT" />
              </th>
              <th style={{ width: 92 }}>
                <T zh="能力" en="CAPS" />
              </th>
              <th style={{ width: 78 }} className="right">
                <T zh="首字延迟" en="TTFB" />
              </th>
              <th style={{ width: 88 }}>
                <T zh="价格" en="PRICE" />
              </th>
              <th style={{ width: 96 }}>
                <T zh="状态" en="STATUS" />
              </th>
              <th style={{ width: 64 }} className="right">
                <T zh="操作" en="PROBE" />
              </th>
            </tr>
          </thead>
          <tbody>
            {list.map((m) => (
              <tr key={m.key}>
                <td>
                  <div className="t-main">
                    {m.model}
                    {m.candidate && (
                      <span className="badge warn canary-badge" title="目录新发现的模型：先小流量试用，连续成功 20 次后转正">
                        <T
                          zh={`新模型试用 ${Math.min(m.canary, 20)}/20`}
                          en={`trial ${Math.min(m.canary, 20)}/20`}
                        />
                      </span>
                    )}
                    {m.alsoOn && m.alsoOn.length > 0 && (
                      <span
                        className="badge info canary-badge"
                        title={`${m.alsoOn.join(' / ')} 也提供这个模型；裸模型名按组路由，任一家限流会自动换下一家`}
                      >
                        <T
                          zh={`${m.alsoOn.length + 1} 家`}
                          en={`${m.alsoOn.length + 1} providers`}
                        />
                      </span>
                    )}
                  </div>
                  <div className="t-sub">
                    {m.probed ? (
                      <T zh="能力已实测" en="caps probed" />
                    ) : (
                      <T zh="能力来自目录声明" en="caps from catalog" />
                    )}
                  </div>
                </td>
                <td className="num">{m.providerName}</td>
                <td className="num right">{m.context ? fmt.ctx(m.context) : '—'}</td>
                <td>
                  <div className="cap">
                    <CapBadge
                      k="T"
                      zh="工具调用"
                      en="Tools"
                      on={m.tools}
                      lang={lang}
                    />
                    <CapBadge
                      k="V"
                      zh="视觉"
                      en="Vision"
                      on={m.vision}
                      lang={lang}
                    />
                    <CapBadge
                      k="J"
                      zh="JSON 模式"
                      en="JSON mode"
                      on={m.json}
                      lang={lang}
                    />
                  </div>
                </td>
                <td className="num right">{fmt.ms(m.latency ?? null)}</td>
                <td>
                  {m.free ? (
                    <span className="badge ok">
                      <T zh="零价" en="free" />
                    </span>
                  ) : m.freeTier ? (
                    <span className="badge accent">
                      <T zh="额度" en="quota" />
                    </span>
                  ) : (
                    <span className="badge gray">
                      <T zh="计费" en="paid" />
                    </span>
                  )}
                </td>
                <td>
                  <Status tone={toneOf(m.status)}>
                    <T zh={labelOf(m.status).zh} en={labelOf(m.status).en} />
                  </Status>
                </td>
                <td className="right">
                  <button
                    className="icon-btn tip-right"
                    data-tip="探测这一条 / Probe this model"
                    disabled={busy !== null}
                    onClick={() => probeOne(m)}
                  >
                    {busy === m.key ? <RefreshCw className="spin" /> : <Activity />}
                  </button>
                </td>
              </tr>
            ))}
            {list.length === 0 && (
              <tr>
                <td colSpan={8}>
                  <div className="empty">
                    <Search />
                    <p>
                      {rows.length === 0 ? (
                        <T
                          zh="还没有接入任何厂商。先去「厂商」页填一个 API Key，模型会自动出现。"
                          en="No providers yet. Add an API key on the Providers page and models show up here."
                        />
                      ) : (
                        <T zh="没有匹配的模型" en="No models match" />
                      )}
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
