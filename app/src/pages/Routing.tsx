import { useEffect, useMemo, useRef, useState } from 'react';
import { Play, RotateCcw, Save, ShieldQuestion } from 'lucide-react';
import { T } from '../lib/i18n';
import { models } from '../lib/data';
import { Badge, toast } from '../components/ui';

type Preset = 'balanced' | 'coding' | 'fastest' | 'smartest' | 'reliable';

const presets: Record<Preset, { r: number; s: number; i: number; zh: string; en: string }> = {
  balanced: { r: 50, s: 25, i: 25, zh: '均衡', en: 'Balanced' },
  coding: { r: 40, s: 20, i: 40, zh: '编码', en: 'Coding' },
  fastest: { r: 10, s: 80, i: 10, zh: '最快', en: 'Fastest' },
  smartest: { r: 10, s: 10, i: 80, zh: '最聪明', en: 'Smartest' },
  reliable: { r: 80, s: 10, i: 10, zh: '最稳', en: 'Reliable' },
};

/** 余量守卫：used 越高，乘子越低；0.8 以下不打折 */
function headroom(used: number) {
  if (used <= 0.8) return 1;
  return Math.max(0.1, 1 - ((used - 0.8) / 0.2) * 0.9);
}

interface Scored {
  id: string;
  name: string;
  provider: string;
  base: number;
  score: number;
  r: number;
  s: number;
  i: number;
  gate: number;
  cooling: boolean;
}

interface HopView {
  n: number;
  model: string;
  provider: string;
  state: 'run' | 'fail' | 'ok';
  code: string;
  reason_zh: string;
  reason_en: string;
  ms: number;
  scope: string;
}

export function Routing() {
  const [preset, setPreset] = useState<Preset>('balanced');
  const [w, setW] = useState({ r: 50, s: 25, i: 25 });
  const [custom, setCustom] = useState(false);
  const [hops, setHops] = useState<HopView[]>([]);
  const [running, setRunning] = useState(false);
  const [done, setDone] = useState<null | 'ok' | 'fail'>(null);
  const timers = useRef<number[]>([]);

  const total = w.r + w.s + w.i || 1;
  const norm = { r: w.r / total, s: w.s / total, i: w.i / total };

  const chain: Scored[] = useMemo(() => {
    return models
      .map((m) => {
        const base = norm.r * m.reliability + norm.s * m.speed + norm.i * m.smart;
        const gate = headroom(m.quota);
        // 429 惩罚最多压到 40%，不判死
        const rate = m.quota > 0.85 ? 0.4 : 1;
        const score = base * gate * rate;
        return {
          id: m.id,
          name: m.name,
          provider: m.provider,
          base,
          score,
          r: m.reliability,
          s: m.speed,
          i: m.smart,
          gate,
          cooling: m.status !== 'healthy',
        };
      })
      .sort((a, b) => b.score - a.score);
  }, [norm.r, norm.s, norm.i]);

  const maxScore = chain[0]?.score ?? 1;

  useEffect(() => () => timers.current.forEach(clearTimeout), []);

  const pick = (p: Preset) => {
    setPreset(p);
    setCustom(false);
    setW({ r: presets[p].r, s: presets[p].s, i: presets[p].i });
  };

  const slide = (k: 'r' | 's' | 'i', v: number) => {
    setCustom(true);
    setW((prev) => ({ ...prev, [k]: v }));
  };

  const simulate = () => {
    timers.current.forEach(clearTimeout);
    timers.current = [];
    setRunning(true);
    setDone(null);
    setHops([]);

    const top = chain.slice(0, 3);
    const script: Omit<HopView, 'state'>[] = [
      {
        n: 0,
        model: top[0].name,
        provider: top[0].provider,
        code: '429',
        reason_zh: 'RPD 已耗尽，读到 Retry-After=1840s',
        reason_en: 'RPD exhausted, Retry-After=1840s',
        ms: 210,
        scope: '模型 + key → 至重置',
      },
      {
        n: 1,
        model: top[1].name,
        provider: top[1].provider,
        code: '402',
        reason_zh: '额度不足，跳过该平台全部模型',
        reason_en: 'Quota exhausted, skip the whole provider',
        ms: 160,
        scope: '平台 → 24h',
      },
      {
        n: 2,
        model: top[2].name,
        provider: top[2].provider,
        code: '200',
        reason_zh: '首字节 312ms，流式转发中',
        reason_en: 'First byte 312ms, streaming',
        ms: 312,
        scope: '—',
      },
    ];

    script.forEach((h, idx) => {
      timers.current.push(
        window.setTimeout(() => {
          setHops((prev) => [
            ...prev.map((p) => (p.state === 'run' ? { ...p, state: 'fail' as const } : p)),
            { ...h, state: idx === script.length - 1 ? 'ok' : 'run' },
          ]);
          if (idx === script.length - 1) {
            setRunning(false);
            setDone('ok');
          }
        }, 700 + idx * 900),
      );
    });
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
              zh="权重决定「谁先上」，守卫乘子决定「谁不能上」。拖动滑杆，右侧链路会真的重算。"
              en="Weights decide who goes first; guard multipliers decide who is not allowed. Drag a slider — the chain recomputes."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => pick('balanced')}>
            <RotateCcw />
            <T zh="重置" en="Reset" />
          </button>
          <button className="btn primary" onClick={() => toast('已保存路由配置 / Routing profile saved')}>
            <Save />
            <T zh="保存" en="Save" />
          </button>
        </div>
      </div>

      <div className="routing-grid">
        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="打分权重" en="Scoring weights" />
              </h3>
              <span className="sub">
                {custom ? (
                  <T zh="自定义（不随预设漂移）" en="custom (no drift)" />
                ) : (
                  <T zh="预设" en="preset" />
                )}
              </span>
            </div>
            <div className="card-body">
              <div style={{ display: 'flex', gap: 6, marginBottom: 16, flexWrap: 'wrap' }}>
                {(Object.keys(presets) as Preset[]).map((p) => (
                  <button
                    key={p}
                    className={`filter ${preset === p && !custom ? 'on' : ''}`}
                    onClick={() => pick(p)}
                  >
                    <T zh={presets[p].zh} en={presets[p].en} />
                  </button>
                ))}
              </div>

              <Slider
                name_zh="可靠性 R"
                name_en="Reliability R"
                hint_zh="EWMA 成功率，Beta 后验采样"
                hint_en="EWMA success, Beta posterior"
                value={w.r}
                onChange={(v) => slide('r', v)}
                color="var(--ok)"
              />
              <Slider
                name_zh="速度 S"
                name_en="Speed S"
                hint_zh="吞吐 × 0.6 + TTFB 线性 × 0.4"
                hint_en="throughput 0.6 + TTFB 0.4"
                value={w.s}
                onChange={(v) => slide('s', v)}
                color="var(--info)"
              />
              <Slider
                name_zh="智能 I"
                name_en="Intelligence I"
                hint_zh="档位分层，档内排名可编辑"
                hint_en="tier stratified, rank editable"
                value={w.i}
                onChange={(v) => slide('i', v)}
                color="var(--accent)"
              />

              <div className="formula">
                base = <b>{norm.r.toFixed(2)}</b>·R + <b>{norm.s.toFixed(2)}</b>·S +{' '}
                <b>{norm.i.toFixed(2)}</b>·I
                <br />
                score = base × headroom × rateLimit × override
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="守卫参数" en="Guard parameters" />
              </h3>
              <span className="sub">
                <T zh="乘子，不是轴" en="multipliers, not axes" />
              </span>
            </div>
            <div className="card-body">
              <div className="params">
                <div className="param">
                  <span className="k">
                    <T zh="余量起降点" en="Headroom floor at" />
                  </span>
                  <input defaultValue="80" />
                  <span className="u">%</span>
                </div>
                <div className="param">
                  <span className="k">
                    <T zh="保底权重" en="Floor weight" />
                  </span>
                  <input defaultValue="0.1" />
                  <span className="u" />
                </div>
                <div className="param">
                  <span className="k">
                    <T zh="429 惩罚" en="429 penalty" />
                  </span>
                  <input defaultValue="0.4" />
                  <span className="u" />
                </div>
                <div className="param">
                  <span className="k">
                    <T zh="探索率" en="Exploration" />
                  </span>
                  <input defaultValue="8" />
                  <span className="u">%</span>
                </div>
                <div className="param">
                  <span className="k">
                    <T zh="总预算" en="Wall budget" />
                  </span>
                  <input defaultValue="45" />
                  <span className="u">s</span>
                </div>
                <div className="param">
                  <span className="k">
                    <T zh="粘性会话" en="Sticky session" />
                  </span>
                  <input defaultValue="30" />
                  <span className="u">m</span>
                </div>
              </div>
              <div className="hint-line">
                <T
                  zh="余量做成乘子而不是轴：它是危险度，不是质量。否则「用了 80% 额度的好模型」会输给慢模型。"
                  en="Headroom is a multiplier, not an axis: it measures risk, not quality. Otherwise a good model at 80% usage loses to a slow one."
                />
              </div>
            </div>
          </div>
        </div>

        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="当前链路" en="Current chain" />
              </h3>
              <span className="sub">
                <T zh={`${chain.length} 个候选 · 实时重算`} en={`${chain.length} candidates · live`} />
              </span>
            </div>
            <div className="card-body" style={{ paddingTop: 6, paddingBottom: 8 }}>
              <div className="chain">
                {chain.slice(0, 9).map((c, i) => (
                  <div className={`chain-row ${c.cooling ? 'cool' : ''}`} key={c.id}>
                    <span className="idx">{i + 1}</span>
                    <div className="body">
                      <div className="top">
                        <span className="mname">{c.name}</span>
                        <span className="prov">{c.provider}</span>
                        {c.cooling && (
                          <Badge tone="warn">
                            <T zh="冷却" en="cool" />
                          </Badge>
                        )}
                      </div>
                      <div className="axes">
                        <span className="axis r">
                          <i style={{ width: `${c.r * 100}%` }} />
                        </span>
                        <span className="axis s">
                          <i style={{ width: `${c.s * 100}%` }} />
                        </span>
                        <span className="axis i">
                          <i style={{ width: `${c.i * 100}%` }} />
                        </span>
                      </div>
                    </div>
                    <div className="sc">
                      <b>{c.score.toFixed(3)}</b>
                      <div className="sc-bar">
                        <i style={{ width: `${(c.score / maxScore) * 100}%` }} />
                      </div>
                    </div>
                  </div>
                ))}
              </div>
              <div className="hint-line" style={{ padding: '0 4px 4px' }}>
                <T zh="三条细线依次是 R / S / I；右下角是乘过守卫后的最终分。" en="Three bars are R / S / I; the right number is the final guarded score." />
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="模拟一次请求" en="Simulate a request" />
              </h3>
              <button
                className="btn primary sm"
                style={{ marginLeft: 'auto' }}
                onClick={simulate}
                disabled={running}
              >
                <Play />
                {running ? <T zh="运行中…" en="Running…" /> : <T zh="开始" en="Run" />}
              </button>
            </div>
            <div className="card-body">
              {hops.length === 0 && (
                <div className="empty" style={{ padding: '18px 10px' }}>
                  <ShieldQuestion />
                  <p>
                    <T
                      zh="会按当前链路模拟：第 1 跳 429、第 2 跳 402、第 3 跳成功。"
                      en="Simulates hop 1 → 429, hop 2 → 402, hop 3 → success."
                    />
                  </p>
                </div>
              )}

              <div className="timeline">
                {hops.map((h) => (
                  <div className={`hop ${h.state}`} key={h.n}>
                    <div className="node">{h.state === 'ok' ? '✓' : h.state === 'fail' ? '✕' : h.n + 1}</div>
                    <div className="info">
                      <div className="l1">
                        {h.model}
                        <span className="prov">· {h.provider}</span>
                        <span className="time">{h.ms}ms</span>
                      </div>
                      <div className="l2">
                        <code>{h.code}</code> <T zh={h.reason_zh} en={h.reason_en} />
                        <br />
                        <span className="faint">
                          <T zh="跳过范围" en="skip scope" />: {h.scope}
                        </span>
                      </div>
                    </div>
                  </div>
                ))}
              </div>

              {done === 'ok' && (
                <div className="result-bar" style={{ marginTop: 10 }}>
                  <b>
                    <T zh="成功" en="Success" />
                  </b>
                  <T zh="3 跳 · 总耗时 682ms · 预算剩 44.3s" en="3 hops · 682ms · 44.3s budget left" />
                  <span className="right">X-Freeport-Trail</span>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      <div className="hint-line">
        <T
          zh="错误作用域：401 只跳该 key；402 跳整个平台 24h；403/404 跳该模型的全部 key；5xx 只影响本请求并升阶梯冷却。"
          en="Scopes: 401 → that key; 402 → whole provider 24h; 403/404 → that model across keys; 5xx → this request only, escalating cooldown."
        />
      </div>
    </>
  );
}

function Slider({
  name_zh,
  name_en,
  hint_zh,
  hint_en,
  value,
  onChange,
  color,
}: {
  name_zh: string;
  name_en: string;
  hint_zh: string;
  hint_en: string;
  value: number;
  onChange: (v: number) => void;
  color: string;
}) {
  return (
    <div className="slider-row">
      <div className="slider-head">
        <span className="nm">
          <T zh={name_zh} en={name_en} />
        </span>
        <span className="hint">
          <T zh={hint_zh} en={hint_en} />
        </span>
        <span className="pct" style={{ color }}>
          {value}
        </span>
      </div>
      <input
        type="range"
        min={0}
        max={100}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{
          background: `linear-gradient(90deg, ${color} ${value}%, var(--panel-3) ${value}%)`,
        }}
      />
    </div>
  );
}
