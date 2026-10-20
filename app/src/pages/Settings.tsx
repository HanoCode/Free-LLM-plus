import { useEffect, useState } from 'react';
import { ExternalLink } from 'lucide-react';
import { T, tx } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { CopyBtn, Switch, toast } from '../components/ui';
import { ApiKeys } from '../components/ApiKeys';
import { Transfer } from '../components/Transfer';
import { License } from '../components/License';
import type { UpdateCheck } from '../lib/bridge';

const REPO = 'https://github.com/HanoCode/Free-LLM-plus';

export function Settings() {
  const { lang, setLang, theme, setTheme } = useApp();
  const desktop = useGateway((s) => s.available);
  const gw = useGateway((s) => s.status);
  const setRequireAuth = useGateway((s) => s.setRequireAuth);
  const setBudget = useGateway((s) => s.setBudget);
  const clearCooldowns = useGateway((s) => s.clearCooldowns);
  const patrol = useGateway((s) => s.patrol);
  const setPatrol = useGateway((s) => s.setPatrol);
  const feed = useGateway((s) => s.feed);
  const setFeed = useGateway((s) => s.setFeed);

  const [version, setVersion] = useState('—');
  const [checking, setChecking] = useState(false);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);

  useEffect(() => {
    const known = document.documentElement.dataset.appVersion;
    if (known) {
      setVersion(known);
      return;
    }
    void window.freeport?.app.info().then((i) => {
      if (typeof i.version === 'string') setVersion(i.version);
    });
  }, []);

  const addr = `http://127.0.0.1:${gw?.port ?? 8787}`;
  const zh = lang === 'zh';

  /** 真去 GitHub 查，不弹「已是最新」的假提示 */
  const checkUpdate = async () => {
    const api = window.freeport?.app;
    if (!api) return toast(zh ? '检查更新需要桌面端' : 'Update check needs the desktop app');
    setChecking(true);
    try {
      const r = await api.checkUpdate();
      setUpdate(r);
      if (r.updateAvailable) {
        toast(zh ? `有新版本 v${r.latest}` : `v${r.latest} is available`);
      } else {
        toast(zh ? `已是最新（v${r.current}）` : `Up to date (v${r.current})`);
      }
    } catch (e) {
      toast(String(e));
    } finally {
      setChecking(false);
    }
  };

  const saveDiagnostics = async () => {
    const api = window.freeport?.app;
    if (!api) return toast(zh ? '诊断包需要桌面端' : 'Diagnostics need the desktop app');
    try {
      const path = await api.saveDiagnostics();
      if (path) toast(tx(lang, `已保存到 ${path}`, `Saved to ${path}`));
    } catch (e) {
      toast(String(e));
    }
  };

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="设置" en="Settings" />
          </h1>
          <p>
            <T
              zh="所有设置本地生效，不需要登录。默认值是为了「装完就安静地跑」。"
              en="Everything applies locally, no account needed. Defaults are tuned to run quietly out of the box."
            />
          </p>
        </div>
      </div>

      <div className="set-grid">
        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="网关" en="Gateway" />
              </h3>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="监听地址" en="Listen address" />
                </div>
                <div className="d">
                  <T zh="只绑回环，不对外暴露" en="loopback only, never exposed" />
                  {gw && !gw.running && (
                    <span style={{ color: 'var(--bad)' }}> · {gw.error || 'offline'}</span>
                  )}
                </div>
              </div>
              <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <span className="mono">127.0.0.1:{gw?.port ?? 8787}</span>
                <CopyBtn value={addr} label="" />
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="本地鉴权" en="Local auth" />
                </div>
                <div className="d mono" style={{ fontSize: 11 }}>
                  {desktop && gw ? gw.authToken.slice(0, 8) + '••••••••' : 'fp-••••••••'}
                </div>
              </div>
              <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                {desktop && gw && <CopyBtn value={gw.authToken} label="" />}
                {desktop && gw && (
                  <Switch
                    on={gw.requireAuth}
                    onChange={(v) => {
                      void setRequireAuth(v);
                      toast(v ? '已开启本地鉴权' : '已关闭本地鉴权（仅回环，风险自担）');
                    }}
                  />
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="冷却中的候选" en="Cooling candidates" />
                </div>
                <div className="d">
                  {gw?.cooldowns?.length
                    ? gw.cooldowns.map((c) => c.key).join(' · ')
                    : lang === 'zh'
                      ? '无'
                      : 'none'}
                </div>
              </div>
              <div className="v">
                {desktop && (
                  <button
                    className="btn ghost sm"
                    onClick={() => {
                      void clearCooldowns();
                      toast('已清除全部冷却 / Cooldowns cleared');
                    }}
                  >
                    <T zh="全部解除" en="Clear all" />
                  </button>
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="协议入口" en="Protocol endpoints" />
                </div>
                <div className="d">
                  <T
                    zh="OpenAI Chat · Responses · Anthropic · Gemini · Ollama 全部在线"
                    en="OpenAI Chat · Responses · Anthropic · Gemini · Ollama all live"
                  />
                </div>
              </div>
              <div className="v">
                <span className="badge ok">5 / 5</span>
              </div>
            </div>
          </div>

          <ApiKeys desktop={desktop} />

          <Transfer desktop={desktop} />

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="启动与退出" en="Startup" />
              </h3>
              <span className="sub">
                <T zh="这三项还没接上后端，先如实标注" en="not wired to the backend yet" />
              </span>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="开机自启" en="Launch at login" />
                </div>
                <div className="d">
                  <T zh="需要 autostart 插件，v1.1 接入" en="needs the autostart plugin, v1.1" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="未接入" en="pending" />
                </span>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="启动时最小化" en="Start minimized" />
                </div>
                <div className="d">
                  <T zh="当前固定行为：启动即显示主窗口" en="current behaviour: window shows on launch" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="固定" en="fixed" />
                </span>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="退出时清理注入" en="Clean up on quit" />
                </div>
                <div className="d">
                  <T zh="现在是手动：注入页逐项回滚" en="manual for now: revert per agent" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="手动" en="manual" />
                </span>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="网络" en="Network" />
              </h3>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="代理" en="Proxy" />
                </div>
                <div className="d">
                  <T
                    zh="跟随系统环境变量 HTTP_PROXY / HTTPS_PROXY，本机回环直连"
                    en="follows HTTP_PROXY / HTTPS_PROXY env vars; loopback bypasses it"
                  />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="环境变量" en="env" />
                </span>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="单次请求预算" en="Request budget" />
                </div>
                <div className="d">
                  <T zh="从第一跳开始计的总墙钟，用完即收尾" en="total wall clock from the first hop" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={gw?.requestBudgetMs ?? 45000}
                  disabled={!desktop}
                  onChange={(e) => {
                    const ms = Number(e.target.value);
                    void setBudget(ms).then(() =>
                      toast(tx(lang, `预算已改为 ${ms / 1000}s`, `Budget set to ${ms / 1000}s`)),
                    );
                  }}
                >
                  <option value={15000}>15s</option>
                  <option value={30000}>30s</option>
                  <option value={45000}>45s</option>
                  <option value={90000}>90s</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="并发上限" en="Concurrency" />
                </div>
                <div className="d">
                  <T zh="当前不排队也不限流；超时由预算兜底" en="no queueing or limiting yet; budget is the backstop" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="不限" en="unbounded" />
                </span>
              </div>
            </div>
          </div>
        </div>

        <div className="stack">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="目录" en="Catalog" />
              </h3>
              <span className="sub">
                <T zh="「新模型发布即到」的开关" en="how new models arrive" />
              </span>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="应用策略" en="Apply policy" />
                </div>
                <div className="d">
                  <T zh="auto 自动灰度 / notify 只提示 / pinned 锁目录" en="auto canary / notify only / pinned" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={feed?.policy ?? 'auto'}
                  disabled={!desktop}
                  onChange={(e) => {
                    const policy = e.target.value;
                    void setFeed({ policy }).then(() =>
                      toast(tx(lang, `目录策略：${policy}`, `Catalog policy: ${policy}`)),
                    );
                  }}
                >
                  <option value="auto">auto</option>
                  <option value="notify">notify</option>
                  <option value="pinned">pinned</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="频道" en="Channel" />
                </div>
                <div className="d">
                  <T zh="beta 先跑 24h 再 promote" en="beta runs 24h before promote" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={feed?.channel ?? 'stable'}
                  disabled={!desktop}
                  onChange={(e) => {
                    const channel = e.target.value;
                    void setFeed({ channel }).then(() =>
                      toast(tx(lang, `目录频道：${channel}`, `Catalog channel: ${channel}`)),
                    );
                  }}
                >
                  <option value="stable">stable</option>
                  <option value="beta">beta</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="目录签名" en="Feed signature" />
                </div>
                <div className="d">
                  {feed?.hasPubkey
                    ? tx(lang, '公钥已配置，每次应用前验签', 'pubkey configured, verified on each apply')
                    : tx(lang, '未配置公钥——目录只读，不会应用任何更新', 'no pubkey — catalog is read-only')}
                </div>
              </div>
              <div className="v">
                <span className={`badge ${feed?.hasPubkey ? 'ok' : 'warn'}`}>
                  {feed?.seq ? `seq ${feed.seq}` : tx(lang, '未启用', 'off')}
                </span>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="离线导入" en="Offline import" />
                </div>
                <div className="d">
                  <T zh="当前只支持从 feed 同步；离线机器用迁移包" en="sync from feed only; use the transfer bundle offline" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="未接入" en="pending" />
                </span>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="巡检" en="Health checks" />
              </h3>
              <span className="sub">
                <T zh="受预算约束，真实流量优先" en="budget-bounded, real traffic first" />
              </span>
            </div>
            <Row
              k_zh="自动巡检"
              k_en="Patrol"
              d_zh="按到期队列探活：热模型 6h、冷模型 24h，每日再全量一次"
              d_en="due queue: hot models every 6h, cold every 24h, plus a daily sweep"
              on={patrol?.enabled ?? true}
              set={(v) => void setPatrol({ enabled: v })}
            />
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="热模型周期" en="Hot interval" />
                </div>
                <div className="d">
                  <T zh="24 小时内有真实流量的模型" en="models used within 24h" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={patrol?.hotHours ?? 6}
                  onChange={(e) => void setPatrol({ hotHours: Number(e.target.value) })}
                >
                  <option value={3}>3h</option>
                  <option value={6}>6h</option>
                  <option value={12}>12h</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="冷模型周期" en="Cold interval" />
                </div>
                <div className="d">
                  <T zh="判为不可用后 30 分钟强制复检一次" en="unavailable models are rechecked after 30m" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={patrol?.coldHours ?? 24}
                  onChange={(e) => void setPatrol({ coldHours: Number(e.target.value) })}
                >
                  <option value={12}>12h</option>
                  <option value={24}>24h</option>
                  <option value={72}>72h</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="每日全量体检" en="Daily full sweep" />
                </div>
                <div className="d">
                  <T zh="本地时间，每天只跑一次" en="local time, once per day" />
                </div>
              </div>
              <div className="v">
                <input
                  className="select"
                  type="time"
                  value={patrol?.sweepAt ?? '03:00'}
                  onChange={(e) => void setPatrol({ sweepAt: e.target.value })}
                />
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="今日探针预算" en="Daily probe budget" />
                </div>
                <div className="d">
                  <T zh="默认 clamp(日配额×1%, 60, 800)" en="default clamp(1% of quota, 60, 800)" />
                </div>
              </div>
              <div className="v mono">
                {patrol ? `${patrol.usedToday} / ${patrol.budgetToday}` : '—'}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="累计探针" en="Probes total" />
                </div>
                <div className="d">
                  <T zh="探针请求单独计数，不占用户额度统计" en="probe traffic is counted separately" />
                </div>
              </div>
              <div className="v mono">
                {patrol ? `${patrol.probesOk} / ${patrol.probesTotal}` : '—'}
              </div>
            </div>
          </div>

          <License />

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="外观与语言" en="Appearance" />
              </h3>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="主题" en="Theme" />
                </div>
                <div className="d">
                  <T zh="跟随系统或固定" en="follow system or fix" />
                </div>
              </div>
              <div className="v">
                <select
                  className="select"
                  value={theme}
                  onChange={(e) => {
                    const t = e.target.value as 'light' | 'dark';
                    document.documentElement.dataset.theme = t;
                    setTheme(t);
                  }}
                >
                  <option value="light">
                    <T zh="浅色" en="Light" />
                  </option>
                  <option value="dark">
                    <T zh="深色" en="Dark" />
                  </option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="语言" en="Language" />
                </div>
                <div className="d">中文 / English</div>
              </div>
              <div className="v">
                <select className="select" value={lang} onChange={(e) => setLang(e.target.value as 'zh' | 'en')}>
                  <option value="zh">简体中文</option>
                  <option value="en">English</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="日志保留" en="Log retention" />
                </div>
                <div className="d">
                  <T zh="内存里最近 300 条，不落盘；重启即清空" en="last 300 in memory, never written to disk" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="300 条" en="300" />
                </span>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="高级与关于" en="Advanced & about" />
              </h3>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="扩展宿主" en="Extension host" />
                </div>
                <div className="d">
                  <T zh="v1.5 才做；现在没有任何扩展能加载" en="lands in v1.5; nothing can load today" />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="未接入" en="pending" />
                </span>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="版本" en="Version" />
                </div>
                <div className="d">
                  Freeport {version} · {window.freeport?.platform ?? 'web'} · schema 1
                  {update?.updateAvailable && (
                    <span style={{ color: 'var(--warn, #c98a2e)' }}> · v{update.latest} 可用</span>
                  )}
                </div>
              </div>
              <div className="v" style={{ display: 'flex', gap: 6 }}>
                {update?.updateAvailable && update.url && (
                  <button
                    className="btn sm"
                    onClick={() => void window.freeport?.shell.openExternal(update.url!)}
                  >
                    <T zh="去下载" en="Download" />
                  </button>
                )}
                <button className="btn ghost sm" disabled={checking} onClick={() => void checkUpdate()}>
                  {checking ? <T zh="检查中…" en="Checking…" /> : <T zh="检查更新" en="Check" />}
                </button>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="诊断信息" en="Diagnostics" />
                </div>
                <div className="d">
                  <T
                    zh="导出脱敏快照：版本、厂商与健康、账本、最近 20 条日志。不含密钥、掩码与鉴权 token。"
                    en="Redacted snapshot: version, providers and health, ledger, last 20 logs. No keys, masks or auth token."
                  />
                </div>
              </div>
              <div className="v">
                <button className="btn ghost sm" disabled={!desktop} onClick={() => void saveDiagnostics()}>
                  <T zh="保存诊断包" en="Save bundle" />
                </button>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="许可与隐私" en="License & privacy" />
                </div>
                <div className="d">
                  <T zh="AGPL-3.0 · 本地优先，无遥测；崩溃日志只留在本机" en="AGPL-3.0 · local-first, no telemetry; crash logs stay local" />
                </div>
              </div>
              <div className="v" style={{ display: 'flex', gap: 6 }}>
                <button
                  className="btn ghost sm"
                  onClick={() => void window.freeport?.shell.openExternal(`${REPO}/blob/main/docs/EULA.md`)}
                >
                  EULA
                </button>
                <button
                  className="btn ghost sm"
                  onClick={() => void window.freeport?.shell.openExternal(REPO)}
                >
                  <ExternalLink size={12} />
                  <T zh="仓库" en="Repo" />
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

function Row({
  k_zh,
  k_en,
  d_zh,
  d_en,
  on,
  set,
}: {
  k_zh: string;
  k_en: string;
  d_zh: string;
  d_en: string;
  on: boolean;
  set: (v: boolean) => void;
}) {
  return (
    <div className="set-row">
      <div>
        <div className="k">
          <T zh={k_zh} en={k_en} />
        </div>
        <div className="d">
          <T zh={d_zh} en={d_en} />
        </div>
      </div>
      <div className="v">
        <Switch on={on} onChange={set} />
      </div>
    </div>
  );
}
