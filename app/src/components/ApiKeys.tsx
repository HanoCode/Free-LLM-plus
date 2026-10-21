import { useCallback, useEffect, useState } from 'react';
import { ChevronDown, ChevronUp, Plus, Trash2 } from 'lucide-react';
import { T, useLang } from '../lib/i18n';
import { CopyBtn, Switch, toast } from './ui';
import type { GwKey } from '../lib/bridge';

/**
 * 虚拟 key 管理。
 *
 * 明文只在创建成功时出现一次，之后只能看到前 8 位——这是刻意的：
 * 落盘只存 sha256，泄露配置文件不等于泄露 key。
 */
/** 大数字用 k/M 显示：13 万 token 写成 130000 在列表里读不出量级。 */
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

function expired(at: string): boolean {
  const t = Date.parse(at);
  return Number.isFinite(t) && t < Date.now();
}

export function ApiKeys({ desktop }: { desktop: boolean }) {
  const lang = useLang();
  const [keys, setKeys] = useState<GwKey[]>([]);
  const [name, setName] = useState('');
  const [rpm, setRpm] = useState('');
  const [daily, setDaily] = useState('');
  const [dayTokens, setDayTokens] = useState('');
  const [allow, setAllow] = useState('');
  const [expires, setExpires] = useState('');
  /** 高级限制默认收起：大部分人只想要「起个名 + 一个次数上限」 */
  const [more, setMore] = useState(false);
  const [fresh, setFresh] = useState<{ name: string; secret: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const api = window.freeport?.gateway;

  const reload = useCallback(async () => {
    if (!api) return;
    try {
      setKeys(await api.keys());
    } catch {
      /* 网关还没起来时静默，下一次轮询会补上 */
    }
  }, [api]);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function create() {
    if (!api) return;
    const n = name.trim();
    if (!n) {
      toast('先给这个 key 起个名字');
      return;
    }
    setBusy(true);
    try {
      // 白名单按逗号/空格切。写 `*` 等于不限，和「空」不是一个意思：
      // 空是「没配」，`*` 是「我明确允许全部」——后者写下来以后改默认值不会影响它
      const models = allow
        .split(/[,，\s]+/)
        .map((x) => x.trim())
        .filter(Boolean);
      const created = await api.createKey({
        name: n,
        rpmLimit: rpm ? Number(rpm) : undefined,
        dailyLimit: daily ? Number(daily) : undefined,
        dailyTokenLimit: dayTokens ? Number(dayTokens) : undefined,
        allowedModels: models.length ? models : undefined,
        // 日期选择器给的是 YYYY-MM-DD，补成当天 23:59:59 的本地时间再转 UTC：
        // 选「到 10-20」的用户期望的是 20 号全天可用，不是 20 号零点就断
        expiresAt: expires
          ? new Date(`${expires}T23:59:59`).toISOString()
          : undefined,
      });
      setFresh({ name: created.key.name, secret: created.secret });
      setName('');
      setRpm('');
      setDaily('');
      setDayTokens('');
      setAllow('');
      setExpires('');
      setKeys(await api.keys());
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <div className="card-head">
        <h3>
          <T zh="API Keys" en="API Keys" />
        </h3>
        <span className="badge gray">
          <T zh="按 key 记额度" en="per-key quota" />
        </span>
      </div>

      {!desktop && (
        <div className="set-row">
          <div>
            <div className="k">
              <T zh="需要桌面端" en="Desktop only" />
            </div>
            <div className="d">
              <T zh="key 存在本机数据目录，浏览器预览里读不到" en="keys live in the local data dir" />
            </div>
          </div>
        </div>
      )}

      {desktop &&
        keys.map((k) => (
          <div className="set-row" key={k.id}>
            <div style={{ minWidth: 0 }}>
              <div className="k" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <span>{k.name}</span>
                <span className="mono" style={{ fontSize: 11, opacity: 0.7 }}>
                  {k.prefix}…
                </span>
                {!k.enabled && (
                  <span className="badge bad">
                    <T zh="已停用" en="disabled" />
                  </span>
                )}
              </div>
              <div className="d">
                {k.lastUsedAt ? (
                  <T
                    zh={`今日 ${k.usedToday}${k.dailyLimit ? ` / ${k.dailyLimit}` : ''} 次${
                      k.dailyTokenLimit
                        ? ` · ${fmtTokens(k.usedTokensToday)} / ${fmtTokens(k.dailyTokenLimit)} token`
                        : k.usedTokensToday
                          ? ` · ${fmtTokens(k.usedTokensToday)} token`
                          : ''
                    } · 最近 ${new Date(k.lastUsedAt).toLocaleTimeString()}`}
                    en={`${k.usedToday}${k.dailyLimit ? ` / ${k.dailyLimit}` : ''} today${
                      k.dailyTokenLimit
                        ? ` · ${fmtTokens(k.usedTokensToday)} / ${fmtTokens(k.dailyTokenLimit)} tokens`
                        : k.usedTokensToday
                          ? ` · ${fmtTokens(k.usedTokensToday)} tokens`
                          : ''
                    } · last ${new Date(k.lastUsedAt).toLocaleTimeString()}`}
                  />
                ) : (
                  <T zh="尚未使用" en="never used" />
                )}
              </div>
              {(k.allowedModels.length > 0 || k.expiresAt) && (
                <div className="d" style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginTop: 3 }}>
                  {k.allowedModels.length > 0 && (
                    <span className="badge gray" title={k.allowedModels.join(', ')}>
                      {k.allowedModels.length === 1 && k.allowedModels[0] === '*'
                        ? lang === 'zh'
                          ? '不限模型'
                          : 'all models'
                        : `${k.allowedModels.length} 个模型`}
                    </span>
                  )}
                  {k.expiresAt && (
                    <span className={`badge ${expired(k.expiresAt) ? 'bad' : 'gray'}`}>
                      {expired(k.expiresAt)
                        ? lang === 'zh'
                          ? '已过期'
                          : 'expired'
                        : new Date(k.expiresAt).toLocaleDateString()}
                    </span>
                  )}
                </div>
              )}
            </div>
            <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <Switch
                on={k.enabled}
                onChange={(v) => {
                  void api
                    ?.toggleKey(k.id, v)
                    .then(setKeys)
                    .catch((e) => toast(String(e)));
                }}
              />
              <button
                className="copy-mini"
                title="删除"
                onClick={() => {
                  void api
                    ?.deleteKey(k.id)
                    .then(setKeys)
                    .catch((e) => toast(String(e)));
                }}
              >
                <Trash2 size={11} />
              </button>
            </div>
          </div>
        ))}

      {desktop && (
        <div style={{ padding: '12px 16px', borderTop: '1px solid var(--line)' }}>
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <input
              className="select"
              style={{ flex: '1 1 140px' }}
              placeholder="名称，比如 claude-code"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            <input
              className="select"
              style={{ width: 110 }}
              placeholder="次/分钟"
              inputMode="numeric"
              value={rpm}
              onChange={(e) => setRpm(e.target.value.replace(/\D/g, ''))}
            />
            <input
              className="select"
              style={{ width: 110 }}
              placeholder="次/天"
              inputMode="numeric"
              value={daily}
              onChange={(e) => setDaily(e.target.value.replace(/\D/g, ''))}
            />
            <button className="copy-mini" disabled={busy} onClick={() => void create()}>
              <Plus size={11} />
              <T zh="新建" en="Create" />
            </button>
            <button className="copy-mini" onClick={() => setMore((v) => !v)}>
              {more ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
              <T zh="更多限制" en="More limits" />
            </button>
          </div>

          {more && (
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 8 }}>
              <input
                className="select"
                style={{ width: 130 }}
                placeholder="token/天"
                inputMode="numeric"
                value={dayTokens}
                onChange={(e) => setDayTokens(e.target.value.replace(/\D/g, ''))}
              />
              <input
                className="select"
                style={{ flex: '1 1 220px' }}
                placeholder="模型白名单：groq:*, gpt-4o-mini"
                value={allow}
                spellCheck={false}
                onChange={(e) => setAllow(e.target.value)}
              />
              <input
                className="select"
                style={{ width: 150 }}
                type="date"
                value={expires}
                onChange={(e) => setExpires(e.target.value)}
              />
            </div>
          )}

          <div className="d" style={{ marginTop: 8 }}>
            <T
              zh="留空表示不限额。限额只作用在这个 key 上，不影响其他 agent。白名单只对显式点名的模型生效，写 auto 时按路由结果再筛。"
              en="Leave blank for unlimited. Limits apply to this key only. The allowlist applies to explicitly named models; with auto it filters the routed result instead."
            />
          </div>
        </div>
      )}

      {fresh && (
        <div
          style={{
            padding: '12px 16px',
            borderTop: '1px solid var(--line)',
            background: 'var(--ok-weak, rgba(45,160,90,.08))',
          }}
        >
          <div className="k">
            <T zh={`「${fresh.name}」的完整 key（只显示这一次）`} en={`Full key for "${fresh.name}" (shown once)`} />
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 6 }}>
            <code className="mono" style={{ fontSize: 12, wordBreak: 'break-all', flex: 1 }}>
              {fresh.secret}
            </code>
            <CopyBtn value={fresh.secret} label="" />
            <button className="copy-mini" onClick={() => setFresh(null)}>
              <T zh="知道了" en="Done" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
