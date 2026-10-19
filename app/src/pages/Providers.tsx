import { ExternalLink, Plus, RefreshCw, ShieldAlert } from 'lucide-react';
import { T } from '../lib/i18n';
import { providers, type Provider } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';
import { useApp } from '../lib/store';

export function Providers() {
  const { setPage } = useApp();
  const connected = providers.filter((p) => p.status === 'connected');
  const available = providers.filter((p) => p.status === 'available' || p.status === 'error');
  const tierA = available.filter((p) => p.tier === 'A' || p.tier === 'B');
  const tierC = available.filter((p) => p.tier === 'C');

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="厂商" en="Providers" />
          </h1>
          <p>
            <T
              zh="Tier A 是注册即得的官方免费额度；Tier C 借用客户端订阅，风险自担、默认关闭。"
              en="Tier A is official free tier. Tier C borrows client subscriptions — opt-in, at your own risk."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => setPage('discover')}>
            <RefreshCw />
            <T zh="发现更多" en="Discover" />
          </button>
          <button className="btn primary" onClick={() => toast('自定义端点：填 base URL + 协议即可 / Custom endpoint')}>
            <Plus />
            <T zh="自定义端点" en="Custom endpoint" />
          </button>
        </div>
      </div>

      <div className="group-title">
        <T zh="已连接 · 11" en="Connected · 11" />
        <span className="line" />
      </div>
      <div className="pgrid">
        {connected.map((p) => (
          <PCard key={p.id} p={p} />
        ))}
      </div>

      <div className="group-title">
        <T zh="可用 · 尚未添加凭据" en="Available · no credentials yet" />
        <span className="line" />
      </div>
      <div className="pgrid">
        {tierA.map((p) => (
          <PCard key={p.id} p={p} />
        ))}
      </div>

      {tierC.length > 0 && (
        <>
          <div className="group-title">
            <T zh="⚠ 客户端订阅 · 需自行评估风险" en="⚠ Client subscriptions · assess the risk yourself" />
            <span className="line" />
          </div>
          <div className="risk-note">
            <ShieldAlert />
            <p>
              <T
                zh="这类凭据借用你已登录的客户端会话，可能违反上游服务条款并导致账号受限。Freeport 只做本地转发，不代注册、不绕过付费、不共享凭据；随时可一键撤销。"
                en="These reuse your logged-in client session. It may violate upstream terms and get the account limited. Freeport only forwards locally: no signup, no paywall bypass, no credential sharing. Revocable anytime."
              />
            </p>
          </div>
          <div className="pgrid">
            {tierC.map((p) => (
              <PCard key={p.id} p={p} />
            ))}
          </div>
        </>
      )}

      <div className="hint-line">
        <T
          zh="所有凭据存进系统钥匙串（Keychain / DPAPI），数据库只存引用；日志与导出默认脱敏。"
          en="Credentials live in the OS keychain (Keychain / DPAPI); the database only stores references."
        />
      </div>
    </>
  );
}

function PCard({ p }: { p: Provider }) {
  const tone = p.tier === 'A' ? 'ok' : p.tier === 'B' ? 'accent' : 'warn';
  return (
    <div className="card pcard">
      <div className="pcard-top">
        <div className="plogo">{p.short}</div>
        <div style={{ minWidth: 0 }}>
          <h4>{p.name}</h4>
          <div className="meta">{p.note ?? p.homepage}</div>
        </div>
        <span style={{ marginLeft: 'auto' }}>
          <Badge tone={tone}>Tier {p.tier}</Badge>
        </span>
      </div>

      <div className="pcard-stats">
        <div>
          <b>{p.models || '—'}</b>
          <span>
            <T zh="模型" en="MODELS" />
          </span>
        </div>
        <div>
          <b>{p.latency ? `${p.latency}ms` : '—'}</b>
          <span>TTFB</span>
        </div>
        <div>
          <b>{p.kind === 'apikey' ? 'KEY' : p.kind === 'oauth' ? 'OAUTH' : p.kind === 'cookie' ? 'COOKIE' : 'ANON'}</b>
          <span>
            <T zh="凭据" en="AUTH" />
          </span>
        </div>
      </div>

      <div className="pcard-foot">
        {p.status === 'connected' && <Status tone="ok"><T zh="已连接" en="connected" /></Status>}
        {p.status === 'available' && <Status tone="idle"><T zh="未连接" en="not connected" /></Status>}
        {p.status === 'error' && <Status tone="bad"><T zh="凭据失效" en="invalid key" /></Status>}
        <span className="right">
          <button
            className="btn ghost sm"
            onClick={() => toast(p.status === 'connected' ? `打开 ${p.name} 设置` : `打开 ${p.name} 获取 Key`)}
          >
            {p.status === 'connected' ? (
              <T zh="管理" en="Manage" />
            ) : p.status === 'error' ? (
              <T zh="修复" en="Fix" />
            ) : (
              <>
                <T zh="获取 Key" en="Get key" />
                <ExternalLink size={10} />
              </>
            )}
          </button>
        </span>
      </div>
    </div>
  );
}
