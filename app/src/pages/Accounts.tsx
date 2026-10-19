import { Plus, ShieldCheck, TriangleAlert } from 'lucide-react';
import { T } from '../lib/i18n';
import { accounts, providers } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';
import { useApp } from '../lib/store';

export function Accounts() {
  const { setPage } = useApp();
  const needAction = accounts.filter((a) => a.status === 'error' || a.status === 'expired');

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="账号" en="Accounts" />
          </h1>
          <p>
            <T
              zh="一个厂商可以有多个账号。额度按账号独立记账，轮转时只影响该账号。"
              en="A provider can have several accounts. Quota is tracked per account; rotation never touches the others."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => setPage('providers')}>
            <T zh="按厂商看" en="By provider" />
          </button>
          <button className="btn primary" onClick={() => toast('添加账号：粘贴 Key / OAuth / 内置窗口登录')}>
            <Plus />
            <T zh="添加账号" en="Add account" />
          </button>
        </div>
      </div>

      {needAction.length > 0 && (
        <div className="risk-note" style={{ marginBottom: 12 }}>
          <TriangleAlert />
          <p>
            <b>
              <T zh={`${needAction.length} 个账号需要处理`} en={`${needAction.length} accounts need attention`} />
            </b>
            {' — '}
            <T
              zh="ModelScope 的 key 已失效（401），OpenCode Zen 的网页会话 3 天前过期。修复前这两个厂商不参与路由。"
              en="ModelScope key is invalid (401); OpenCode Zen session expired 3 days ago. Both are excluded from routing until fixed."
            />
          </p>
        </div>
      )}

      <div className="card">
        <div className="card-head">
          <h3>
            <T zh="账号池" en="Account pool" />
          </h3>
          <span className="sub">
            <T zh="四窗口记账 · 请求前预留、响应后修正" en="four windows · reserve before, settle after" />
          </span>
        </div>

        {accounts.map((a) => {
          const prov = providers.find((p) => p.id === a.provider);
          return (
            <div className="acc-row" key={a.id}>
              <div className="acc-ic">{prov?.short ?? a.provider.slice(0, 2).toUpperCase()}</div>
              <div className="acc-main">
                <div className="acc-name">
                  {prov?.name ?? a.provider}
                  <span className="badge gray">{a.label}</span>
                  {a.kind === 'cookie' && (
                    <Badge tone="warn">
                      <T zh="网页会话" en="web session" />
                    </Badge>
                  )}
                  {a.kind === 'oauth' && <Badge tone="accent">OAuth</Badge>}
                </div>
                <div className="acc-sub">
                  {a.masked} · {a.detail}
                </div>
              </div>

              <div className="acc-bars">
                <Bar label="RPM" v={a.windows.rpm} />
                <Bar label="RPD" v={a.windows.rpd} />
                <Bar label="TPM" v={a.windows.tpm} />
                <Bar label="TPD" v={a.windows.tpd} />
              </div>

              <div style={{ width: 92, flex: 'none' }}>
                {a.status === 'active' && <Status tone="ok"><T zh="正常" en="active" /></Status>}
                {a.status === 'cooldown' && <Status tone="warn"><T zh="限流中" en="cooling" /></Status>}
                {a.status === 'expired' && <Status tone="idle"><T zh="已过期" en="expired" /></Status>}
                {a.status === 'error' && <Status tone="bad"><T zh="失效" en="invalid" /></Status>}
              </div>

              <div style={{ width: 60, flex: 'none', textAlign: 'right' }}>
                <button
                  className="btn ghost sm"
                  onClick={() =>
                    toast(
                      a.status === 'active'
                        ? '已重新校验 / Re-validated'
                        : '打开修复流程 / Open fix flow',
                    )
                  }
                >
                  {a.status === 'active' ? (
                    <T zh="校验" en="Check" />
                  ) : (
                    <T zh="修复" en="Fix" />
                  )}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      <div className="two-col" style={{ marginTop: 12 }}>
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="凭据生命周期" en="Credential lifecycle" />
            </h3>
          </div>
          <div className="card-body">
            <div className="kv">
              <span className="k">
                <T zh="存储" en="Storage" />
              </span>
              <span className="v">Keychain / DPAPI</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="OAuth 刷新提前量" en="OAuth refresh skew" />
              </span>
              <span className="v">24h</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="refresh_token 保活" en="refresh_token keepalive" />
              </span>
              <span className="v">3d</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="刷新失败退避" en="refresh backoff" />
              </span>
              <span className="v">5m</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="新号失效宽限" en="new account grace" />
              </span>
              <span className="v">10m / 30s</span>
            </div>
            <div className="hint-line">
              <T
                zh="参数取自 chat2api 的实战经验值：提前量太短会撞上过期瞬间，太长会被上游判定为异常刷新。"
                en="Values come from chat2api field experience: too short hits expiry, too long looks like abusive refreshing."
              />
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="轮转策略" en="Rotation policy" />
            </h3>
          </div>
          <div className="card-body">
            <div className="kv">
              <span className="k">
                <T zh="选择依据" en="Selection" />
              </span>
              <span className="v">
                <T zh="余量优先 + 轮转" en="headroom first, then round-robin" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="单账号 429 影响" en="429 blast radius" />
              </span>
              <span className="v">
                <T zh="仅该账号" en="that account only" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="并发预留" en="Concurrent lease" />
              </span>
              <span className="v">est. tokens</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="日窗口重置" en="Daily reset" />
              </span>
              <span className="v">UTC 00:00</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="探测标记" en="Probe header" />
              </span>
              <span className="v">X-Freeport-Probe</span>
            </div>
            <div className="hint-line">
              <T
                zh="探针请求单独计数，不混进用户用量。巡检预算耗尽时只探路由链前 20 个。"
                en="Probe traffic is counted separately. When the probe budget runs dry only the top 20 of the chain is checked."
              />
            </div>
          </div>
        </div>
      </div>

      <div className="hint-line">
        <ShieldCheck size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T
          zh="Freeport 不代注册账号、不转售额度、不按 token 计费。所有免费额度归上游，随时可能变化。"
          en="Freeport never registers accounts, resells quota, or bills per token. Free tiers belong to upstream and can change anytime."
        />
      </div>
    </>
  );
}

function Bar({ label, v }: { label: string; v: number }) {
  return (
    <div className="acc-bar">
      <span>{label}</span>
      <div className="track">
        <i className={v > 0.85 ? 'bad' : v > 0.6 ? 'warn' : ''} style={{ width: `${Math.max(v * 100, 1)}%` }} />
      </div>
    </div>
  );
}
