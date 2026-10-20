import { useCallback, useEffect, useState } from 'react';
import { Plus, Trash2 } from 'lucide-react';
import { T } from '../lib/i18n';
import { CopyBtn, Switch, toast } from './ui';
import type { GwKey } from '../lib/bridge';

/**
 * 虚拟 key 管理。
 *
 * 明文只在创建成功时出现一次，之后只能看到前 8 位——这是刻意的：
 * 落盘只存 sha256，泄露配置文件不等于泄露 key。
 */
export function ApiKeys({ desktop }: { desktop: boolean }) {
  const [keys, setKeys] = useState<GwKey[]>([]);
  const [name, setName] = useState('');
  const [rpm, setRpm] = useState('');
  const [daily, setDaily] = useState('');
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
      const created = await api.createKey({
        name: n,
        rpmLimit: rpm ? Number(rpm) : undefined,
        dailyLimit: daily ? Number(daily) : undefined,
      });
      setFresh({ name: created.key.name, secret: created.secret });
      setName('');
      setRpm('');
      setDaily('');
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
                    zh={`今日 ${k.usedToday}${k.dailyLimit ? ` / ${k.dailyLimit}` : ''} 次 · 最近 ${new Date(k.lastUsedAt).toLocaleTimeString()}`}
                    en={`${k.usedToday}${k.dailyLimit ? ` / ${k.dailyLimit}` : ''} today · last ${new Date(k.lastUsedAt).toLocaleTimeString()}`}
                  />
                ) : (
                  <T zh="尚未使用" en="never used" />
                )}
              </div>
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
          </div>
          <div className="d" style={{ marginTop: 8 }}>
            <T
              zh="留空表示不限额。限额只作用在这个 key 上，不影响其他 agent。"
              en="Leave blank for unlimited. Limits apply to this key only."
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
