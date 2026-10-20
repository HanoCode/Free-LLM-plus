import { useEffect, useMemo, useRef, useState } from 'react';
import { Cookie, KeyRound, LogIn, Plus, RefreshCw, ShieldCheck, Trash2, TriangleAlert } from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, Status, Switch, toast } from '../components/ui';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import type {
  GwAccount,
  GwCooldown,
  GwLoginStart,
  GwLoginState,
  GwProvider,
  GwQuota,
  ProviderQuotaInput,
} from '../lib/bridge';

/**
 * 凭据视图（一厂商多账号）。
 *
 * 粒度是**凭据**而不是厂商：同一家接两把 key，在上游就是两本独立的账。
 * 所以这一页按厂商分组、按凭据出行，每行有自己的四个窗口、自己的冷却、
 * 自己的开关。路由候选、失败跳过、账本三处都按同一个粒度算，漏一处
 * 多接的 key 就只是界面上多一行字。
 */

type RowState = 'active' | 'cooling' | 'exhausted' | 'missing' | 'invalid' | 'disabled';

interface Row {
  p: GwProvider;
  a: GwAccount;
  /** 同一厂商里的序号，界面上区分「第一把 / 第二把」 */
  index: number;
  quota?: GwQuota;
  cooldown?: GwCooldown;
  state: RowState;
  /** 状态说明：恢复时刻、冷却原因等 */
  detail: string;
}

interface Group {
  p: GwProvider;
  rows: Row[];
}

const REASON: Record<string, { zh: string; en: string }> = {
  rate_limit: { zh: '触发限流', en: 'rate limited' },
  no_credit: { zh: '余额不足', en: 'no credit' },
  quota: { zh: '额度用尽', en: 'quota exhausted' },
  quota_exhausted: { zh: '免费额度用完', en: 'free quota exhausted' },
  no_free_tier: { zh: '不提供免费额度', en: 'no free tier' },
  invalid_key: { zh: '密钥失效', en: 'invalid key' },
  forbidden: { zh: '被拒绝', en: 'forbidden' },
  upstream: { zh: '上游异常', en: 'upstream error' },
};

function reasonText(reason: string): { zh: string; en: string } {
  return REASON[reason] ?? { zh: reason, en: reason };
}

function clockOf(ms: number): string {
  const d = new Date(ms);
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

function stateOf(p: GwProvider, a: GwAccount, quota: GwQuota | undefined, cd: GwCooldown | undefined) {
  if (!a.enabled) {
    return { state: 'disabled' as RowState, detail: '已停用，不参与路由' };
  }
  if (!a.hasKey) {
    return { state: 'missing' as RowState, detail: '还没有填 Key' };
  }
  if (cd) {
    const r = reasonText(cd.reason);
    return {
      state: 'cooling' as RowState,
      detail: `${r.zh} · ${clockOf(cd.until)} 恢复`,
    };
  }
  const dry = quota?.windows.find((w) => w.exhausted);
  if (dry) {
    const at = quota?.resetAt ?? dry.resetAt;
    return {
      state: 'exhausted' as RowState,
      detail: `${dry.kind.toUpperCase()} 用尽 · ${clockOf(at)} 恢复`,
    };
  }
  // 凭据自己的校验结果优先。没有就退回厂商级（老数据、或只跑过巡检）。
  // 厂商级现在是「还有一条好的就算好」的汇总，所以不会因为另一把 key 挂了被连坐。
  const h = a.health ?? p.health;
  if (h.status === 'unavailable' || h.status === 'error') {
    return { state: 'invalid' as RowState, detail: h.error ?? '这条凭据最近一次校验失败' };
  }
  return {
    state: 'active' as RowState,
    detail: h.latencyMs ? `${h.latencyMs}ms` : '待探测',
  };
}

function buildGroups(
  providers: GwProvider[],
  quota: GwQuota[],
  cooldowns: GwCooldown[],
): Group[] {
  const now = Date.now();
  return providers
    .map((p) => {
      const rows: Row[] = p.accounts.map((a, index) => {
        const q = quota.find((x) => x.accountId === a.id);
        // 厂商级冷却（5xx / 连不上）会连坐这一家的每条凭据，key 级只挡自己
        const cd = cooldowns.find(
          (c) => (c.key === `k:${a.id}` || c.key === `p:${p.id}`) && c.until > now,
        );
        return { p, a, index, quota: q, cooldown: cd, ...stateOf(p, a, q, cd) };
      });
      return { p, rows };
    })
    .filter((g) => g.rows.length > 0);
}

const HINTS = ['rpm', 'rpd', 'tpm', 'tpd'] as const;

function limitDigits(q: ProviderQuotaInput | undefined): Record<string, string> {
  return {
    rpm: q?.rpm ? String(q.rpm) : '',
    rpd: q?.rpd ? String(q.rpd) : '',
    tpm: q?.tpm ? String(q.tpm) : '',
    tpd: q?.tpd ? String(q.tpd) : '',
  };
}

export function Accounts() {
  const { setPage } = useApp();
  const providers = useGateway((s) => s.providers);
  const quota = useGateway((s) => s.quota);
  const status = useGateway((s) => s.status);
  const probeAccount = useGateway((s) => s.probeAccount);
  const removeAccount = useGateway((s) => s.removeAccount);
  const patchAccount = useGateway((s) => s.patchAccount);
  const available = useGateway((s) => s.available);

  /** 追加凭据的弹窗：只对已有厂商开放（新厂商走「接入厂商」） */
  const [adding, setAdding] = useState<GwProvider | null>(null);
  /** 编辑凭据：改名字、限额、启停 */
  const [editing, setEditing] = useState<{ p: GwProvider; a: GwAccount } | null>(null);
  /** 设备码登录弹窗 */
  const [logging, setLogging] = useState<GwProvider | null>(null);
  /** 粘贴 web 会话的弹窗。`a` 有值表示是给已有凭据换会话。 */
  const [cookies, setCookies] = useState<{ p: GwProvider; a?: GwAccount } | null>(null);

  const groups = useMemo(
    () => buildGroups(providers, quota ?? [], status?.cooldowns ?? []),
    [providers, quota, status],
  );

  const allRows = groups.flatMap((g) => g.rows);
  const total = allRows.length;
  const ready = allRows.filter((r) => r.state === 'active').length;
  const needAction = allRows.filter((r) => r.state !== 'active' && r.state !== 'disabled');

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="账号" en="Accounts" />
          </h1>
          <p>
            <T
              zh="额度按凭据算，不按厂商算。同一家接两把 key 就是两条独立通道：各自记账、各自冷却，一条 429 不影响另一条。"
              en="Quota is tracked per credential, not per provider. Two keys on the same vendor are two independent lanes — separate ledgers, separate cooldowns."
            />
          </p>
        </div>
        <div className="head-actions">
          <Badge tone={ready > 0 ? 'ok' : 'gray'}>
            <T zh={`${ready}/${total} 条可用`} en={`${ready}/${total} usable`} />
          </Badge>
          <button className="btn primary" onClick={() => setPage('providers')}>
            <Plus />
            <T zh="接入厂商" en="Add provider" />
          </button>
        </div>
      </div>

      {needAction.length > 0 && (
        <div className="risk-note" style={{ marginBottom: 12 }}>
          <TriangleAlert />
          <p>
            <b>
              <T
                zh={`${needAction.length} 条凭据需要处理`}
                en={`${needAction.length} credentials need attention`}
              />
            </b>
            {' — '}
            {needAction.map((r) => `${r.p.name} / ${r.a.name}`).join('、')}
            <T
              zh="。修复前它们不参与路由，请求会自动落到下一跳。"
              en=". They are excluded from routing until fixed; traffic falls through to the next hop."
            />
          </p>
        </div>
      )}

      {groups.length === 0 ? (
        <div className="card">
          <div className="card-body" style={{ color: 'var(--text-2)', fontSize: 12.5 }}>
            <T
              zh="还没有任何凭据。先去「厂商」页粘一个官方免费 Key。"
              en="No credentials yet. Paste an official free key on the Providers page."
            />
          </div>
        </div>
      ) : (
        groups.map((g) => (
          <div className="card" key={g.p.id} style={{ marginBottom: 12 }}>
            <div className="card-head">
              <h3 style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                {g.p.name}
                <Badge tone={g.p.tier === 'A' ? 'ok' : g.p.tier === 'B' ? 'accent' : 'warn'}>
                  Tier {g.p.tier}
                </Badge>
                {g.p.freeTier && (
                  <Badge tone="accent">
                    <T zh="免费额度" en="free tier" />
                  </Badge>
                )}
              </h3>
              <span className="sub">
                <T
                  zh={`${g.rows.length} 条凭据 · ${g.p.models} 个模型`}
                  en={`${g.rows.length} credentials · ${g.p.models} models`}
                />
              </span>
              <span style={{ display: 'flex', gap: 6, marginLeft: 8 }}>
                <button
                  className="btn ghost sm"
                  disabled={!available}
                  title="用每条凭据各打一次 /models"
                  onClick={async () => {
                    // 逐条校验：一条 key 的结果只写它自己，成功几条报几条
                    let ok = 0;
                    let tried = 0;
                    for (const r of g.rows) {
                      if (!r.a.hasKey) continue;
                      tried += 1;
                      const out = await probeAccount(g.p.id, r.a.id);
                      if (out.ok) ok += 1;
                    }
                    toast(
                      tried === 0
                        ? `${g.p.name} 还没有任何 Key`
                        : `${g.p.name}：${ok}/${tried} 条凭据可用`,
                    );
                  }}
                >
                  <RefreshCw size={11} />
                  <T zh="校验全部" en="Check all" />
                </button>
                {g.p.tier === 'C' && (
                  <button
                    className="btn ghost sm"
                    disabled={!available}
                    title="在自己的浏览器里登录，不用手填 key"
                    onClick={() => setLogging(g.p)}
                  >
                    <LogIn size={11} />
                    <T zh="登录" en="Sign in" />
                  </button>
                )}
                {COOKIE_PROVIDERS.has(g.p.id) && (
                  <button
                    className="btn ghost sm"
                    disabled={!available}
                    title="粘贴浏览器里的会话，适用于没有公开 API 的站点"
                    onClick={() => setCookies({ p: g.p })}
                  >
                    <Cookie size={11} />
                    <T zh="会话" en="Session" />
                  </button>
                )}
                <button
                  className="btn ghost sm"
                  disabled={!available}
                  onClick={() => setAdding(g.p)}
                >
                  <Plus size={11} />
                  <T zh="追加凭据" en="Add key" />
                </button>
              </span>
            </div>

            {g.rows.map((r) => (
              <div className="acc-row" key={r.a.id}>
                <div className="acc-ic">{(r.p.name.slice(0, 2) || '??').toUpperCase()}</div>

                <div className="acc-main">
                  <div className="acc-name">
                    {r.a.name}
                    {r.index === 0 && (
                      <span className="badge gray">
                        <T zh="主" en="primary" />
                      </span>
                    )}
                    {!r.a.enabled && (
                      <span className="badge gray">
                        <T zh="已停用" en="off" />
                      </span>
                    )}
                    {r.a.auth === 'cookie' ? (
                      r.a.hasKey ? (
                        <span className="badge ok" title="web 会话可用">
                          Web 会话
                        </span>
                      ) : (
                        <button
                          className="badge warn"
                          title="会话已过期或全部失效，点这里重新粘贴"
                          onClick={() => setCookies({ p: g.p, a: r.a })}
                        >
                          <T zh="会话已过期" en="Session expired" />
                        </button>
                      )
                    ) : (
                      r.a.auth && (
                        <span className="badge ok" title={expiryHint(r.a.expiresAt)}>
                          {r.a.auth === 'copilot' ? 'GitHub 登录' : 'OAuth'}
                        </span>
                      )
                    )}
                    {r.a.quota && (
                      <span className="badge gray">
                        <T zh="已设限额" en="limited" />
                      </span>
                    )}
                  </div>
                  <div className="acc-sub">
                    {r.a.auth === 'cookie'
                      ? r.a.hasKey
                        ? 'web 会话'
                        : '会话已失效'
                      : r.a.hasKey
                        ? r.a.keyMask
                        : '（未配置）'}{' '}
                    · {r.detail}
                  </div>
                </div>

                <div className="acc-bars">
                  {HINTS.map((k) => {
                    const w = r.quota?.windows.find((x) => x.kind === k);
                    // 没配限额时 remaining 恒为 1，进度条留空并标「—」
                    const used = w && w.limit > 0 ? Math.min(1, 1 - w.remaining) : 0;
                    return (
                      <Bar
                        key={k}
                        label={k.toUpperCase()}
                        v={used}
                        muted={!w || w.limit <= 0}
                        probe={w?.usedProbe ?? 0}
                      />
                    );
                  })}
                </div>

                <div style={{ width: 96, flex: 'none' }}>
                  {r.state === 'active' && (
                    <Status tone="ok">
                      <T zh="正常" en="active" />
                    </Status>
                  )}
                  {r.state === 'cooling' && (
                    <Status tone="warn">
                      <T zh="冷却中" en="cooling" />
                    </Status>
                  )}
                  {r.state === 'exhausted' && (
                    <Status tone="warn">
                      <T zh="额度耗尽" en="exhausted" />
                    </Status>
                  )}
                  {r.state === 'missing' && (
                    <Status tone="idle">
                      <T zh="未配置" en="no key" />
                    </Status>
                  )}
                  {r.state === 'invalid' && (
                    <Status tone="bad">
                      <T zh="失效" en="invalid" />
                    </Status>
                  )}
                  {r.state === 'disabled' && (
                    <Status tone="idle">
                      <T zh="已停用" en="off" />
                    </Status>
                  )}
                </div>

                <div
                  style={{
                    width: 186,
                    flex: 'none',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'flex-end',
                    gap: 4,
                  }}
                >
                  <Switch
                    on={r.a.enabled}
                    onChange={(v) => {
                      void patchAccount(r.p.id, r.a.id, { enabled: v }).then(() =>
                        toast(v ? `${r.a.name} 已启用` : `${r.a.name} 已停用，不再参与路由`),
                      );
                    }}
                  />
                  <button
                    className="btn ghost sm"
                    title={`用 ${r.a.name} 这把 key 校验`}
                    disabled={!r.a.hasKey}
                    onClick={async () => {
                      const out = await probeAccount(r.p.id, r.a.id);
                      toast(
                        out.ok
                          ? `${r.a.name}：${out.models ?? 0} 个模型 · ${out.latencyMs ?? 0}ms`
                          : `${r.a.name} 校验失败：${out.error ?? '未知错误'}`,
                      );
                    }}
                  >
                    <RefreshCw size={11} />
                  </button>
                  <button className="btn ghost sm" onClick={() => setEditing({ p: r.p, a: r.a })}>
                    <T zh="编辑" en="Edit" />
                  </button>
                  <button
                    className="btn ghost sm"
                    title="删除这条凭据"
                    onClick={async () => {
                      if (r.index === 0 && g.rows.length > 1) {
                        toast('这条是主凭据，删掉后路由会改用下一条；确认无误再点一次');
                      }
                      await removeAccount(r.p.id, r.a.id);
                      toast(`已删除 ${r.p.name} / ${r.a.name}，密文与账本一并清除`);
                    }}
                  >
                    <Trash2 size={11} />
                  </button>
                </div>
              </div>
            ))}
          </div>
        ))
      )}

      <div className="two-col" style={{ marginTop: 12 }}>
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="凭据存储" en="Credential storage" />
            </h3>
          </div>
          <div className="card-body">
            <div className="kv">
              <span className="k">
                <T zh="加密" en="Encryption" />
              </span>
              <span className="v">AES-256-GCM</span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="存放位置" en="Location" />
              </span>
              <span className="v">
                <T zh="本机应用目录（不进系统钥匙串）" en="app data dir, not the OS keychain" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="界面与日志" en="UI and logs" />
              </span>
              <span className="v">
                <T zh="只出现掩码" en="masked only" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="删除凭据" en="Deleting a credential" />
              </span>
              <span className="v">
                <T zh="密文与账本同时清" en="ciphertext and ledger cleared" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="注入给 Agent" en="Injected to agents" />
              </span>
              <span className="v">
                <T zh="每个 Agent 一把独立 key" en="one key per agent" />
              </span>
            </div>
            <div className="hint-line">
              <T
                zh="主密钥是数据目录里 32 字节的随机文件（权限 0600）。拿走 freeport.json 看不到任何 Key。"
                en="The master key is a 32-byte random file (mode 0600) in the data dir. freeport.json alone leaks nothing."
              />
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="隔离与轮转" en="Isolation and rotation" />
            </h3>
          </div>
          <div className="card-body">
            <div className="kv">
              <span className="k">
                <T zh="选路" en="Selection" />
              </span>
              <span className="v">
                <T zh="Gate → 平滑加权 → 最早刷新" en="Gate → smooth weighted → earliest reset" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="候选粒度" en="Candidate unit" />
              </span>
              <span className="v">
                <T zh="凭据 × 模型" en="credential × model" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="失败作用域" en="Failure scope" />
              </span>
              <span className="v">
                <T zh="key / 模型 / 厂商 三级" en="key, model or provider" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="并发预留" en="Concurrent reserve" />
              </span>
              <span className="v">
                <T zh="输入估算 + max_tokens" en="input estimate + max_tokens" />
              </span>
            </div>
            <div className="kv">
              <span className="k">
                <T zh="探针用量" en="Probe usage" />
              </span>
              <span className="v">
                <T zh="与用户请求分列" en="counted separately" />
              </span>
            </div>
            <div className="hint-line">
              <T
                zh="限流、欠费、key 失效只挡这一条凭据；5xx 和连不上才连坐整家。同一个模型的两条 key 也会各自冷却，互不拖累。"
                en="Rate limits, credit and key errors only bench that one credential; 5xx and unreachable hosts bench the whole vendor."
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

      {adding && (
        <AccountDialog
          provider={adding}
          onClose={() => setAdding(null)}
        />
      )}
      {editing && (
        <AccountDialog
          provider={editing.p}
          account={editing.a}
          onClose={() => setEditing(null)}
        />
      )}
      {logging && <LoginDialog provider={logging} onClose={() => setLogging(null)} />}
      {cookies && (
        <CookieDialog
          provider={cookies.p}
          account={cookies.a}
          onClose={() => setCookies(null)}
        />
      )}
    </>
  );
}

/* ── 追加 / 编辑凭据 ─────────────────────────── */
function AccountDialog({
  provider,
  account,
  onClose,
}: {
  provider: GwProvider;
  account?: GwAccount;
  onClose: () => void;
}) {
  const addAccount = useGateway((s) => s.addAccount);
  const patchAccount = useGateway((s) => s.patchAccount);
  const edit = !!account;

  const [name, setName] = useState(account?.name ?? '');
  const [apiKey, setApiKey] = useState('');
  const [limits, setLimits] = useState<Record<string, string>>(limitDigits(account?.quota));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  /** 空字符串 = 这个维度不拦，所以只在填了数字时才进 quota */
  const quotaOf = (): ProviderQuotaInput | undefined => {
    const q: ProviderQuotaInput = {};
    for (const k of HINTS) {
      const raw = limits[k]?.trim();
      if (!raw) continue;
      const n = Number(raw);
      if (!Number.isFinite(n) || n <= 0) return undefined;
      q[k] = Math.round(n);
    }
    return Object.keys(q).length ? q : undefined;
  };

  const submit = async () => {
    setErr('');
    if (!edit && !apiKey.trim()) return setErr('请填入 API Key');

    const q = quotaOf();
    if (Object.keys(limits).some((k) => limits[k]?.trim()) && !q) {
      return setErr('限额要填正整数，留空表示该维度不拦');
    }

    setBusy(true);
    try {
      if (edit && account) {
        await patchAccount(provider.id, account.id, {
          name: name.trim() || undefined,
          // 编辑时把空限额显式清掉，否则「删掉数字」这个动作不会生效
          quota: q,
          clearQuota: !q,
        });
        toast(`已更新 ${name.trim() || account.name}`);
      } else {
        await addAccount({
          providerId: provider.id,
          apiKey: apiKey.trim(),
          name: name.trim() || undefined,
          quota: q,
        });
        toast(`已追加一条凭据，额度独立记账`);
      }
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <h3>
          {edit ? (
            <T zh={`编辑 ${account?.name}`} en={`Edit ${account?.name}`} />
          ) : (
            <T zh={`给 ${provider.name} 追加凭据`} en={`Add a key to ${provider.name}`} />
          )}
        </h3>
        <p className="sub">
          {edit ? (
            <T
              zh="限额只对这一条凭据生效。留空表示该维度只记账、不拦截。"
              en="Limits apply to this credential only. Blank means record but never block."
            />
          ) : (
            <T
              zh="新凭据会用真实 Key 拉一次 /models 校验，通过才入库。它的额度、冷却、账本都从零开始。"
              en="A /models call validates the key before saving. Its quota, cooldowns and ledger start from zero."
            />
          )}
        </p>

        <div className="field">
          <label>NAME</label>
          <input
            value={name}
            placeholder={`key-${provider.accounts.length + 1}`}
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        {!edit && (
          <div className="field">
            <label>API KEY</label>
            <input
              type="password"
              autoFocus
              value={apiKey}
              placeholder="sk-..."
              onChange={(e) => setApiKey(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && !busy && void submit()}
            />
          </div>
        )}

        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10 }}>
          {HINTS.map((k) => (
            <div className="field" key={k}>
              <label>{k === 'tpm' || k === 'tpd' ? `${k.toUpperCase()} (TOKENS)` : k.toUpperCase()}</label>
              <input
                inputMode="numeric"
                value={limits[k] ?? ''}
                placeholder="不限"
                onChange={(e) => setLimits((s) => ({ ...s, [k]: e.target.value }))}
              />
            </div>
          ))}
        </div>

        {err && <div className="modal-err">{err}</div>}

        <div className="modal-actions">
          {edit && account && (
            <button
              className="btn ghost"
              style={{ marginRight: 'auto' }}
              onClick={async () => {
                await patchAccount(provider.id, account.id, { clearQuota: true });
                setLimits(limitDigits(undefined));
                toast('已清空限额，这条凭据回到只记账');
              }}
            >
              <T zh="清空限额" en="Clear limits" />
            </button>
          )}
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            <T zh="取消" en="Cancel" />
          </button>
          <button className="btn primary" onClick={() => void submit()} disabled={busy}>
            {busy ? (
              <T zh="处理中…" en="Working…" />
            ) : edit ? (
              <T zh="保存" en="Save" />
            ) : (
              <T zh="追加并校验" en="Add key" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

function Bar({
  label,
  v,
  muted,
  probe,
}: {
  label: string;
  v: number;
  muted: boolean;
  probe: number;
}) {
  return (
    <div className="acc-bar" title={probe > 0 ? `巡检 ${probe}` : undefined}>
      <span>
        {label}
        {!muted && ` ${Math.round(v * 100)}%`}
      </span>
      <div className="track">
        {!muted && (
          <i
            className={v > 0.85 ? 'bad' : v > 0.6 ? 'warn' : ''}
            style={{ width: `${Math.max(v * 100, 1)}%` }}
          />
        )}
      </div>
    </div>
  );
}

/* ── 设备码登录 ───────────────────────────────
   用户在自己的浏览器里授权自己的账号：我们不收密码、不读 Cookie、
   不内嵌 client_secret。界面只负责把 code 说清楚、把轮询节奏守住。 */

function expiryHint(expiresAt?: number): string | undefined {
  if (!expiresAt) return undefined;
  const mins = Math.round((expiresAt * 1000 - Date.now()) / 60000);
  return mins <= 0 ? '授权即将到期，下一次请求会自动续期' : `授权还剩约 ${mins} 分钟，快到期会自动续期`;
}

function LoginDialog({ provider, onClose }: { provider: GwProvider; onClose: () => void }) {
  const loginStart = useGateway((s) => s.loginStart);
  const loginPoll = useGateway((s) => s.loginPoll);
  const [start, setStart] = useState<GwLoginStart | null>(null);
  const [state, setState] = useState<GwLoginState | null>(null);
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(true);
  const timer = useRef<number | null>(null);

  const stop = () => {
    if (timer.current !== null) {
      window.clearInterval(timer.current);
      timer.current = null;
    }
  };

  useEffect(() => {
    let alive = true;

    const poll = async (id: string) => {
      try {
        const out = await loginPoll(id);
        if (!alive) return;
        setState(out);
        if (out.status === 'pending') return;
        stop();
        if (out.status === 'ready') {
          toast(`已登录${out.login ? `为 ${out.login}` : ''}，凭据已加入 ${provider.name}`);
          onClose();
        }
      } catch (e) {
        stop();
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      }
    };

    void (async () => {
      try {
        const s = await loginStart(provider.id);
        if (!alive) return;
        setStart(s);
        // 按上游给的 interval 轮询：自己拍一个更快的会被限流
        timer.current = window.setInterval(() => void poll(s.loginId), Math.max(1, s.interval) * 1000);
      } catch (e) {
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      } finally {
        if (alive) setBusy(false);
      }
    })();

    return () => {
      alive = false;
      stop();
    };
    // 只在打开时跑一次：provider 变了就是另一个弹窗了
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider.id]);

  const open = (url?: string) => {
    if (!url) return;
    window.open(url, '_blank', 'noopener,noreferrer');
  };

  const terminal = state && state.status !== 'pending';

  return (
    <div className="modal-mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <h3>
          <T zh={`登录 ${provider.name}`} en={`Sign in to ${provider.name}`} />
        </h3>

        <div className="modal-body">
          <p className="modal-note">
            <T
              zh="在你的浏览器里登录你自己的账号。Freeport 不接触密码，也不读取系统浏览器的 Cookie。"
              en="Sign in with your own account in your browser. Freeport never sees your password or reads system browser cookies."
            />
          </p>

          {busy && (
            <div className="login-code">
              <T zh="正在申请设备码…" en="Requesting a device code…" />
            </div>
          )}

          {start && (
            <>
              <div className="login-code" aria-label="device code">
                <KeyRound size={14} />
                <span className="login-code-value">{start.userCode}</span>
                <button
                  className="btn ghost sm"
                  onClick={() => {
                    void navigator.clipboard?.writeText(start.userCode);
                    toast('设备码已复制');
                  }}
                >
                  <T zh="复制" en="Copy" />
                </button>
              </div>
              <p className="modal-note">{start.instruction}</p>
              <div style={{ display: 'flex', gap: 8, marginTop: 4 }}>
                <button
                  className="btn primary"
                  onClick={() => open(start.verificationUriComplete ?? start.verificationUri)}
                >
                  <LogIn size={12} />
                  <T zh="打开授权页面" en="Open authorization page" />
                </button>
              </div>
              {state?.status === 'pending' && (
                <p className="modal-note">
                  <T zh="等待你在浏览器里确认…" en="Waiting for you to approve in the browser…" />
                  {state.message ? ` ${state.message}` : ''}
                </p>
              )}
            </>
          )}

          {terminal && state?.status !== 'ready' && (
            <div className="modal-err">{state?.message ?? '登录未完成'}</div>
          )}
        </div>

        {err && <div className="modal-err">{err}</div>}

        <div className="modal-actions">
          <button className="btn ghost" onClick={onClose}>
            <T zh="关闭" en="Close" />
          </button>
        </div>
      </div>
    </div>
  );
}

/* ── web 会话（Cookie 形态） ─────────────────────
   只给没有公开 API、只认浏览器会话的站点用。我们只保存用户主动交出来的
   这几条 cookie：不读系统浏览器库、不模拟登录、会话失效也不自动续——那是在
   绕人家的风控。失效就如实说「请重新粘贴」。 */

const COOKIE_PROVIDERS = new Set(['opencode']);

function CookieDialog({
  provider,
  account,
  onClose,
}: {
  provider: GwProvider;
  account?: GwAccount;
  onClose: () => void;
}) {
  const addCookieAccount = useGateway((s) => s.addCookieAccount);
  const setAccountCookie = useGateway((s) => s.setAccountCookie);
  const [raw, setRaw] = useState('');
  const [name, setName] = useState('');
  const [err, setErr] = useState('');
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (!raw.trim()) {
      setErr('先粘贴会话内容');
      return;
    }
    setBusy(true);
    setErr('');
    try {
      if (account) {
        await setAccountCookie(provider.id, account.id, raw);
        toast(`已更新 ${provider.name} 的会话`);
      } else {
        await addCookieAccount(provider.id, name.trim() || undefined, raw);
        toast(`会话已加入 ${provider.name}`);
      }
      onClose();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <h3>
          <Cookie size={14} />
          <T
            zh={account ? `更新 ${account.name} 的会话` : `为 ${provider.name} 添加会话`}
            en={account ? `Refresh session for ${account.name}` : `Add session to ${provider.name}`}
          />
        </h3>

        <div className="modal-body">
          <p className="modal-note">
            <T
              zh="在浏览器里登录后，从开发者工具 → Application → Cookies 复制，或直接粘贴 Cookie 请求头。只保存你交出来的这几条，不会去读系统浏览器的 Cookie。"
              en="Sign in with your browser, then copy from DevTools → Application → Cookies, or paste the Cookie request header. Only what you paste is stored — we never read system browser cookies."
            />
          </p>

          {!account && (
            <input
              className="input"
              placeholder="凭据名称（可选）"
              value={name}
              onChange={(e) => setName(e.target.value)}
              spellCheck={false}
            />
          )}

          <textarea
            className="cookie-input"
            placeholder={'session=...; other=...\n或粘贴 DevTools 导出的 JSON 数组'}
            value={raw}
            onChange={(e) => setRaw(e.target.value)}
            spellCheck={false}
            rows={5}
          />

          <p className="modal-note">
            <T
              zh="会话失效后不会自动续期，界面会提示重新粘贴——自动续期需要模拟浏览行为，那属于绕风控，我们不做。"
              en="Sessions are never auto-renewed. The UI will ask you to paste again — renewing would mean simulating browser behaviour, which we refuse to do."
            />
          </p>
        </div>

        {err && <div className="modal-err">{err}</div>}

        <div className="modal-actions">
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            <T zh="取消" en="Cancel" />
          </button>
          <button className="btn primary" onClick={() => void submit()} disabled={busy}>
            {busy ? <T zh="校验中…" en="Checking…" /> : <T zh="保存并校验" en="Save & verify" />}
          </button>
        </div>
      </div>
    </div>
  );
}
