import { T } from '../lib/i18n';
import { Badge } from './ui';
import type { GwQuota, GwWindow } from '../lib/bridge';

/**
 * 额度账本。
 *
 * 两件必须让用户看见的事：
 * 1. 请求数和 token 是两个独立维度，任一打满都会 Gate；
 * 2. 巡检花的额度单列——否则「我只发了 3 次请求，额度怎么没了」没法解释。
 */

const UNIT: Record<GwWindow['kind'], { label: string; token: boolean }> = {
  rpm: { label: '次/分', token: false },
  rpd: { label: '次/天', token: false },
  tpm: { label: 'tok/分', token: true },
  tpd: { label: 'tok/天', token: true },
};

function fmtNum(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(Math.round(n));
}

function hhmmOf(ms?: number): string | null {
  if (!ms) return null;
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return null;
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function Bar({ w }: { w: GwWindow }) {
  const used = w.usedUser + w.usedProbe;
  const pct = w.limit > 0 ? Math.min(100, (used / w.limit) * 100) : 0;
  const color = w.exhausted ? 'var(--bad)' : pct > 80 ? 'var(--warn, #c98a2e)' : 'var(--ok, #2da05a)';
  return (
    <div
      style={{ flex: 1, height: 5, borderRadius: 3, background: 'var(--line)', overflow: 'hidden' }}
    >
      <div style={{ width: `${pct}%`, height: '100%', background: color }} />
    </div>
  );
}

export function QuotaPanel({ quota }: { quota: GwQuota[] }) {
  if (!quota.length) return null;

  return (
    <div className="card" style={{ marginBottom: 14 }}>
      <div className="card-head">
        <h3>
          <T zh="额度记录" en="Quota" />
        </h3>
        <span className="sub">
          <T
            zh="请求数与 token 双记，任一耗尽即出局；巡检单独计数"
            en="requests and tokens tracked separately — either one can gate; probes counted apart"
          />
        </span>
      </div>

      {quota.map((q) => {
        const tracked = q.windows.filter((w) => w.limit > 0);
        const reset = hhmmOf(q.resetAt);
        // 同一厂商可能有多条凭据：显示凭据名才分得清是哪本账
        const multi = quota.filter((x) => x.provider === q.provider).length > 1;
        return (
          <div
            key={q.accountId}
            style={{ padding: '10px 16px', borderTop: '1px solid var(--line)' }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
              <span style={{ fontWeight: 600, fontSize: 13 }}>{q.name}</span>
              {multi && (
                <span className="mono" style={{ fontSize: 11, opacity: 0.6 }}>
                  {q.accountName}
                </span>
              )}
              {reset && (
                <Badge tone="warn">
                  <T zh={`预计 ${reset} 恢复`} en={`back around ${reset}`} />
                </Badge>
              )}
              <span
                className="mono"
                style={{ marginLeft: 'auto', fontSize: 11, opacity: 0.7 }}
              >
                {tracked.length === 0 ? (
                  <T zh="仅记账" en="track only" />
                ) : (
                  <T zh={`剩余 ${Math.round(q.headroom * 100)}%`} en={`${Math.round(q.headroom * 100)}% left`} />
                )}
              </span>
            </div>

            {tracked.length === 0 ? (
              <div className="d">
                <T
                  zh="没配限额，只记账不拦截。填了 quota 才会在耗尽时把这家排除出候选链。"
                  en="No limits configured — usage is recorded but never blocks routing."
                />
              </div>
            ) : (
              tracked.map((w) => {
                const used = w.usedUser + w.usedProbe;
                const u = UNIT[w.kind];
                return (
                  <div
                    key={w.kind}
                    style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 4 }}
                  >
                    <span className="mono" style={{ width: 52, fontSize: 11, opacity: 0.65 }}>
                      {u.label}
                    </span>
                    <Bar w={w} />
                    <span className="mono" style={{ fontSize: 11, minWidth: 96, textAlign: 'right' }}>
                      {fmtNum(used)} / {fmtNum(w.limit)}
                    </span>
                    {w.usedProbe > 0 && (
                      <span className="mono" style={{ fontSize: 10, opacity: 0.55 }}>
                        <T
                          zh={`含巡检 ${fmtNum(w.usedProbe)}`}
                          en={`${fmtNum(w.usedProbe)} probe`}
                        />
                      </span>
                    )}
                  </div>
                );
              })
            )}
          </div>
        );
      })}
    </div>
  );
}
