import { ExternalLink, RefreshCw, Radar } from 'lucide-react';
import { T } from '../lib/i18n';
import { candidates, discoverSources } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';

export function Discover() {
  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="发现" en="Discover" />
          </h1>
          <p>
            <T
              zh="扫描 5 个来源，只有实测通过的才进目录。候选池里的东西不进路由。"
              en="Five sources, verified before listed. Candidates never enter routing."
            />
          </p>
        </div>
        <div className="head-actions">
          <span className="badge info">
            <T zh="上次扫描 14 分钟前" en="scanned 14m ago" />
          </span>
          <button className="btn ghost" onClick={() => toast('已开始扫描 5 个来源 / Scanning 5 sources')}>
            <RefreshCw />
            <T zh="立即扫描" en="Scan now" />
          </button>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 6 }}>
        <div className="card-head">
          <h3>
            <T zh="数据来源" en="Sources" />
          </h3>
          <span className="sub">
            <T zh="Free 6h / Pro 5min · 条件请求，无变更返回 304" en="free 6h / pro 5min · conditional GET" />
          </span>
        </div>
        {discoverSources.map((s) => (
          <div className="src-row" key={s.id}>
            <span className="nm">
              {s.id} · {s.name}
            </span>
            <span className="meta">
              <T zh={s.zh} en={s.en} />
            </span>
            <span className="right">
              {s.ok ? (
                <Status tone={s.id === 'S4' ? 'warn' : 'ok'}>
                  {s.id === 'S5' ? <T zh="低可信" en="low trust" /> : s.code}
                </Status>
              ) : (
                <Status tone="warn">{s.code}</Status>
              )}
              <span className="ev-time">{s.time}</span>
            </span>
          </div>
        ))}
      </div>

      <div className="sec-title">
        <T zh="可直接启用 · 无需 Key" en="Ready to enable · no key" />
        <span className="ln" />
      </div>
      <div className="grid-3">
        <DiscCard
          name="Pollinations"
          tier="B"
          meta_zh="6 个模型 · 匿名端点&#10;验证 2 分钟前 · 延迟 840ms"
          meta_en="6 models · anonymous&#10;verified 2m ago · 840ms"
          action="enable"
          foot="probe 3/3 ✓"
        />
        <DiscCard
          name="LLM7"
          tier="B"
          meta_zh="3 个模型 · 公共端点&#10;验证 18 分钟前 · 延迟 1.2s"
          meta_en="3 models · public endpoint&#10;verified 18m ago · 1.2s"
          action="enable"
          foot="probe 2/3 · no tools"
        />
        <DiscCard
          name="Cloudflare Workers AI"
          tier="A"
          meta_zh="9 个模型 · 10K neurons/天&#10;已收录，待添加凭据"
          meta_en="9 models · 10K neurons/day&#10;listed, needs credentials"
          action="key"
          foot="listed"
        />
      </div>

      <div className="sec-title">
        <T zh="需要 Key · 有免费额度" en="Needs a key · free tier" />
        <span className="ln" />
      </div>
      <div className="grid-3">
        <DiscCard
          name="Together AI"
          tier="A"
          meta_zh="$1 试用额度 · 需绑卡 ⚠&#10;来源：S5 社区，未实测"
          meta_en="$1 trial · card required ⚠&#10;source S5, unverified"
          action="paste"
          foot="trust: low"
        />
        <DiscCard
          name="Hyperbolic"
          tier="A"
          meta_zh="5 个模型 · 无需绑卡&#10;来源：S2 OpenRouter 交叉验证"
          meta_en="5 models · no card&#10;cross-checked via S2"
          action="key"
          foot="trust: high"
        />
        <DiscCard
          name="NVIDIA NIM"
          tier="A"
          meta_zh="已连接 · 2 个新模型待确认&#10;来源：S1 上游 /models"
          meta_en="connected · 2 new models&#10;source S1 /models"
          action="review"
          foot="diff"
        />
      </div>

      <div className="sec-title">
        <T zh="候选池 · 验证中，不进路由" en="Candidate pool · not routed" />
        <span className="ln" />
      </div>
      <div className="card">
        <table>
          <thead>
            <tr>
              <th>MODEL</th>
              <th style={{ width: 150 }}>SOURCE</th>
              <th style={{ width: 150 }}>
                <T zh="状态" en="STATE" />
              </th>
              <th style={{ width: 120 }}>
                <T zh="上次探测" en="LAST PROBE" />
              </th>
              <th style={{ width: 90 }} className="right">
                <T zh="操作" en="ACTION" />
              </th>
            </tr>
          </thead>
          <tbody>
            {candidates.map((c) => (
              <tr key={c.model}>
                <td>
                  <div className="t-main">{c.model}</div>
                  <div className="t-sub">{c.provider}</div>
                </td>
                <td className="t-sub">{c.source}</td>
                <td>
                  <Badge tone={c.state.tone as 'warn'}>
                    <T zh={c.state.zh} en={c.state.en} />
                  </Badge>
                </td>
                <td className="num">{c.probe}</td>
                <td className="right">
                  <button
                    className="btn ghost sm"
                    onClick={() =>
                      toast(
                        c.action === 'retry'
                          ? '已加入重试队列 / Queued for retry'
                          : c.action === 'accept'
                            ? '已按 chat-only 收录 / Listed as chat-only'
                            : '请先添加 Fireworks 凭据 / Add Fireworks credentials',
                      )
                    }
                  >
                    {c.action === 'retry' ? (
                      <T zh="重试" en="Retry" />
                    ) : c.action === 'accept' ? (
                      <T zh="收录" en="Accept" />
                    ) : (
                      <T zh="添加" en="Add" />
                    )}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="hint-line">
        <Radar size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T
          zh="候选池的新模型先以 0.3 倍权重进路由，连续 20 次成功才转正；单次删除超过 15% 会自动熔断并转人工确认。"
          en="New models enter routing at 0.3× weight, promoted after 20 consecutive successes. A single removal above 15% trips the circuit breaker."
        />
      </div>
    </>
  );
}

function DiscCard({
  name,
  tier,
  meta_zh,
  meta_en,
  action,
  foot,
}: {
  name: string;
  tier: 'A' | 'B';
  meta_zh: string;
  meta_en: string;
  action: 'enable' | 'key' | 'paste' | 'review';
  foot: string;
}) {
  return (
    <div className="card disc">
      <div className="disc-top">
        <div className="disc-name">{name}</div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={tier === 'A' ? 'ok' : 'accent'}>Tier {tier}</Badge>
        </span>
      </div>
      <div className="disc-meta" style={{ whiteSpace: 'pre-line' }}>
        <T zh={meta_zh} en={meta_en} />
      </div>
      <div className="disc-foot">
        {action === 'enable' && (
          <button className="btn primary sm" onClick={() => toast(`已启用 ${name} / Enabled`)}>
            <T zh="启用" en="Enable" />
          </button>
        )}
        {action === 'key' && (
          <button className="btn sm" onClick={() => toast(`打开 ${name} 控制台`)}>
            <T zh="获取 Key" en="Get key" />
            <ExternalLink size={11} />
          </button>
        )}
        {action === 'paste' && (
          <>
            <button className="btn sm" onClick={() => toast(`打开 ${name} 控制台`)}>
              <T zh="获取 Key" en="Get key" />
              <ExternalLink size={11} />
            </button>
            <button className="btn ghost sm" onClick={() => toast('粘贴 Key 后自动探测 / Paste to auto-probe')}>
              <T zh="已粘贴" en="Paste" />
            </button>
          </>
        )}
        {action === 'review' && (
          <button className="btn sm" onClick={() => toast('已把 2 个模型加入候选池 / 2 models queued')}>
            <T zh="查看 2 个" en="Review 2" />
          </button>
        )}
        <span className="time">{foot}</span>
      </div>
    </div>
  );
}
