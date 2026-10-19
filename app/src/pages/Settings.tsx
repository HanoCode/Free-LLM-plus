import { useState } from 'react';
import { ExternalLink, FolderOpen } from 'lucide-react';
import { T } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { CopyBtn, Switch, toast } from '../components/ui';

export function Settings() {
  const [autoStart, setAutoStart] = useState(true);
  const [minimized, setMinimized] = useState(true);
  const [cleanup, setCleanup] = useState(true);
  const [probe, setProbe] = useState(true);
  const [cooldownCheck, setCooldownCheck] = useState(true);
  const [devMode, setDevMode] = useState(false);
  const [autoApply, setAutoApply] = useState(true);
  const { lang, setLang, theme, setTheme } = useApp();
  const desktop = useGateway((s) => s.available);
  const gw = useGateway((s) => s.status);
  const setRequireAuth = useGateway((s) => s.setRequireAuth);
  const clearCooldowns = useGateway((s) => s.clearCooldowns);

  const addr = `http://127.0.0.1:${gw?.port ?? 8787}`;

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
                  <T zh="OpenAI 已上线 · Anthropic / Gemini 在 M6" en="OpenAI live · Anthropic/Gemini in M6" />
                </div>
              </div>
              <div className="v">
                <span className="badge ok">OpenAI</span>
                <span className="badge gray" style={{ marginLeft: 6 }}>
                  Anthropic · M6
                </span>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="启动与退出" en="Startup" />
              </h3>
            </div>
            <Row
              k_zh="开机自启"
              k_en="Launch at login"
              d_zh="登录后静默启动，只在托盘显示"
              d_en="starts silently, tray only"
              on={autoStart}
              set={setAutoStart}
            />
            <Row
              k_zh="启动时最小化"
              k_en="Start minimized"
              d_zh="不弹主窗口，避免打断当前工作"
              d_en="no window on boot"
              on={minimized}
              set={setMinimized}
            />
            <Row
              k_zh="退出时清理注入"
              k_en="Clean up on quit"
              d_zh="把 Agent 配置恢复到注入前，避免指向已关闭的端口"
              d_en="restore agent configs so nothing points at a dead port"
              on={cleanup}
              set={setCleanup}
            />
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
                  <T zh="优先级：账号级 > 运行时 > 全局" en="account > runtime > global" />
                </div>
              </div>
              <div className="v">
                <select className="select" defaultValue="system">
                  <option value="system">
                    <T zh="跟随系统" en="System" />
                  </option>
                  <option value="none">None</option>
                  <option value="custom">Custom…</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="超时" en="Timeout" />
                </div>
                <div className="d">
                  <T zh="单跳首字节，超过即换下一个" en="per-hop first byte" />
                </div>
              </div>
              <div className="v">
                <select className="select" defaultValue="45">
                  <option value="15">15s</option>
                  <option value="30">30s</option>
                  <option value="45">45s</option>
                  <option value="90">90s</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="并发上限" en="Concurrency" />
                </div>
                <div className="d">
                  <T zh="超出的请求排队，不直接 429" en="queue instead of 429" />
                </div>
              </div>
              <div className="v">
                <select className="select" defaultValue="8">
                  <option>4</option>
                  <option>8</option>
                  <option>16</option>
                  <option>32</option>
                </select>
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
                  defaultValue="auto"
                  onChange={(e) => setAutoApply(e.target.value === 'auto')}
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
                <select className="select" defaultValue="stable">
                  <option value="stable">stable</option>
                  <option value="beta">beta</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="静默应用新增" en="Auto-apply additions" />
                </div>
                <div className="d">
                  <T zh={autoApply ? '新模型以 0.3 倍权重灰度' : '需手动确认'} en={autoApply ? 'canary at 0.3× weight' : 'needs confirmation'} />
                </div>
              </div>
              <div className="v">
                <Switch on={autoApply} onChange={setAutoApply} />
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="手动导入" en="Import JSON" />
                </div>
                <div className="d">
                  <T zh="离线机器用，格式与 feed 一致" en="for offline machines, feed schema" />
                </div>
              </div>
              <div className="v">
                <button className="btn sm" onClick={() => toast('选择 catalog.json / Pick catalog.json')}>
                  <FolderOpen size={12} />
                  <T zh="选择文件" en="Choose" />
                </button>
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="巡检" en="Health checks" />
              </h3>
              <span className="sub">
                <T zh="默认开启，但受预算约束" en="on by default, budget-bounded" />
              </span>
            </div>
            <Row
              k_zh="每日全量审计"
              k_en="Daily full audit"
              d_zh="03:00 起算，±90 分钟抖动"
              d_en="from 03:00, ±90m jitter"
              on={probe}
              set={setProbe}
            />
            <Row
              k_zh="冷却到期复检"
              k_en="Pre-expiry recheck"
              d_zh="提前 60 秒探一次，好了就提前解除"
              d_en="probe 60s early, lift cooldown if healthy"
              on={cooldownCheck}
              set={setCooldownCheck}
            />
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="每日探针预算" en="Daily probe budget" />
                </div>
                <div className="d">
                  <T zh="默认 clamp(日配额×1%, 60, 800)" en="default clamp(1% of quota, 60, 800)" />
                </div>
              </div>
              <div className="v">
                <select className="select" defaultValue="300">
                  <option>100</option>
                  <option>300</option>
                  <option>800</option>
                  <option>unlimited</option>
                </select>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="今日已用" en="Used today" />
                </div>
                <div className="d">
                  <T zh="探针请求单独计数，不占用户额度统计" en="probe traffic is counted separately" />
                </div>
              </div>
              <div className="v mono">137 / 300</div>
            </div>
          </div>

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
                  <T zh="Pro 可到 90 天" en="Pro keeps 90 days" />
                </div>
              </div>
              <div className="v">
                <select className="select" defaultValue="7">
                  <option>1</option>
                  <option>7</option>
                  <option>30</option>
                </select>
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
                  <T zh="扩展开发者模式" en="Extension dev mode" />
                </div>
                <div className="d">
                  <T zh="允许加载未签名扩展，每次启动都会提示" en="unsigned extensions, warned on every start" />
                </div>
              </div>
              <div className="v">
                <Switch on={devMode} onChange={setDevMode} />
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="版本" en="Version" />
                </div>
                <div className="d">Freeport 0.1.0 · Tauri 2 + Rust · schema 1</div>
              </div>
              <div className="v">
                <button className="btn sm" onClick={() => toast('已是最新版本 / Up to date')}>
                  <T zh="检查更新" en="Check" />
                </button>
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="许可与隐私" en="License & privacy" />
                </div>
                <div className="d">
                  <T zh="AGPL-3.0 · 本地优先，无遥测" en="AGPL-3.0 · local-first, no telemetry" />
                </div>
              </div>
              <div className="v">
                <button className="btn ghost sm" onClick={() => toast('打开文档 / Open docs')}>
                  <ExternalLink size={12} />
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
