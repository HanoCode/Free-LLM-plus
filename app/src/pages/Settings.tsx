import { useEffect, useState } from 'react';
import { ExternalLink } from 'lucide-react';
import { T, tx } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { CopyBtn, Switch, toast } from '../components/ui';
import { ApiKeys } from '../components/ApiKeys';
import { Transfer } from '../components/Transfer';
import { License } from '../components/License';
import type { DesktopPrefs, UpdateCheck } from '../lib/bridge';

const REPO = 'https://github.com/HanoCode/Free-LLM-plus';

export function Settings() {
  const { lang, setLang, theme, setTheme } = useApp();
  const desktop = useGateway((s) => s.available);
  const gw = useGateway((s) => s.status);
  const setRequireAuth = useGateway((s) => s.setRequireAuth);
  const setBudget = useGateway((s) => s.setBudget);
  const setCompress = useGateway((s) => s.setCompress);
  const patchConfig = useGateway((s) => s.patchConfig);
  const clearCooldowns = useGateway((s) => s.clearCooldowns);
  const patrol = useGateway((s) => s.patrol);
  const setPatrol = useGateway((s) => s.setPatrol);
  const feed = useGateway((s) => s.feed);
  const setFeed = useGateway((s) => s.setFeed);

  const [version, setVersion] = useState('—');
  const [checking, setChecking] = useState(false);
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [prefs, setPrefs] = useState<DesktopPrefs | null>(null);
  const [autostart, setAutostart] = useState(false);

  useEffect(() => {
    const api = window.freeport?.app;
    if (!api) return;
    void api.prefs().then(setPrefs).catch(() => {});
    // 自启状态以系统为准：用户在「登录项」里手删过也要如实显示
    void api.autostart().then(setAutostart).catch(() => {});
  }, []);

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
                  <T zh="工具输出压缩" en="Compress tool output" />
                </div>
                <div className="d">
                  <T
                    zh="超过阈值的工具输出只留开头与结尾，中间换成一句说明。省 token，但确实会丢信息——所以默认关。"
                    en="Trim long tool outputs to head + tail with an explicit marker. Saves tokens, loses information — off by default."
                  />
                  {desktop && gw && (
                    <span className="mono" style={{ marginLeft: 6 }}>
                      {`>${gw.compressMaxToolChars.toLocaleString()} chars`}
                    </span>
                  )}
                </div>
              </div>
              <div className="v">
                {desktop && gw && (
                  <Switch
                    on={gw.compress}
                    onChange={(v) => {
                      void setCompress(v);
                      toast(
                        v
                          ? '已开启工具输出压缩：超长输出会保留首尾，并写明省略了多少'
                          : '已关闭工具输出压缩：请求体原样转发',
                      );
                    }}
                  />
                )}
              </div>
            </div>
            {/* 智能过滤是压缩的子选项：压缩关着时它没有任何作用，
                所以直接跟着压缩状态禁用，而不是让它看起来能独立生效 */}
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="按内容裁（智能过滤）" en="Structure-aware trimming" />
                </div>
                <div className="d">
                  <T
                    zh="认出 git diff / grep 结果 / 重复日志后按结构裁：diff 只留改动块和 hunk 头，grep 每个文件留三条，连续重复行折叠。认不出来的形态仍然走首尾截断。"
                    en="Recognises git diffs, grep output and repeated logs, then trims by structure: keeps hunks and changed lines, three hits per file, collapses runs. Unknown shapes still fall back to head + tail."
                  />
                </div>
              </div>
              <div className="v">
                {desktop && gw && (
                  <Switch
                    on={gw.compressSmart}
                    disabled={!gw.compress}
                    onChange={(v) => {
                      void patchConfig({ compressSmart: v });
                      toast(
                        v
                          ? '已开启智能过滤：结构化输出按结构裁，省得更多'
                          : '已关闭智能过滤：统一走首尾截断',
                      );
                    }}
                  />
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="响应缓存" en="Response cache" />
                </div>
                <div className="d">
                  <T
                    zh="同一段请求体在有效期内直接复用上次的非流式回答。只缓存正常收尾（无工具调用）的完整响应：带 tool_calls 或 finish_reason 不是 stop 的一律不缓存。"
                    en="Reuse the previous non-streaming answer for an identical request body within the TTL. Only complete responses with no tool calls are cached; anything with tool_calls or a non-stop finish reason is never stored."
                  />
                  {desktop && gw && gw.cache && (
                    <span className="mono" style={{ marginLeft: 6 }}>
                      TTL {Math.round(gw.cacheTtlSecs / 60)} min
                    </span>
                  )}
                </div>
              </div>
              <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                {desktop && gw && (
                  <>
                    {gw.cache && (
                      <select
                        value={gw.cacheTtlSecs}
                        onChange={(e) => {
                          void patchConfig({ cacheTtlSecs: Number(e.target.value) });
                        }}
                        style={{
                          height: 28,
                          padding: '0 8px',
                          border: '1px solid var(--line)',
                          borderRadius: 7,
                          background: 'var(--panel-2)',
                          color: 'var(--text)',
                          fontSize: 11.5,
                        }}
                      >
                        {[60, 300, 600, 1800, 3600].map((v) => (
                          <option key={v} value={v}>
                            {v < 60 ? `${v}s` : tx(lang, `${v / 60} 分钟`, `${v / 60} min`)}
                          </option>
                        ))}
                      </select>
                    )}
                    <Switch
                      on={gw.cache}
                      onChange={(v) => {
                        void patchConfig({ cache: v });
                        toast(
                          v
                            ? '已开启响应缓存：完全相同的请求直接复用，不消耗上游额度'
                            : '已关闭响应缓存：每个请求都会真的发出去',
                        );
                      }}
                    />
                  </>
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="单来源限速" en="Per-source rate limit" />
                </div>
                <div className="d">
                  <T
                    zh="同一个 x-freeport-identifier 每分钟最多这么多次，超了直接 429 并带上 retry-after。填 0 表示不限。这是防「一个脚本把额度瞬间打光」的闸，不是安全边界——它只按请求头算。"
                    en="Max requests per minute for the same x-freeport-identifier; over that returns 429 with retry-after. 0 disables it. This throttles runaway scripts — it is keyed on a header, not a security boundary."
                  />
                </div>
              </div>
              <div className="v">
                {desktop && gw && (
                  <input
                    type="number"
                    min={0}
                    max={10000}
                    defaultValue={gw.identifierLimit}
                    key={`ident-${gw.identifierLimit}`}
                    onBlur={(e) => {
                      const v = Math.max(0, Math.min(10000, Number(e.target.value) || 0));
                      if (v === gw.identifierLimit) return;
                      void patchConfig({ identifierLimit: v });
                      toast(v === 0 ? '已取消单来源限速' : `单来源限速已设为 ${v} 次/分钟`);
                    }}
                    style={{
                      width: 90,
                      height: 28,
                      padding: '0 8px',
                      border: '1px solid var(--line)',
                      borderRadius: 7,
                      background: 'var(--panel-2)',
                      color: 'var(--text)',
                      fontFamily: 'var(--mono)',
                      fontSize: 11.5,
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
                <T zh="MCP 服务器" en="MCP server" />
              </h3>
              <span className="sub">
                <T zh="让 agent 自省「能用什么、刚才为什么挂」· 只读" en="read-only introspection for agents" />
              </span>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="同一个可执行文件，加 --mcp" en="same binary, with --mcp" />
                </div>
                <div className="d">
                  <T
                    zh="走 stdio 协议，不起网关、不转发流量。给 Claude Desktop / Codex 这类客户端当自省通道。"
                    en="speaks the stdio protocol; starts no gateway and forwards no traffic."
                  />
                </div>
              </div>
              <div className="v">
                <span className="badge gray">
                  <T zh="只读" en="read-only" />
                </span>
              </div>
            </div>
            {/* 卡片直接子元素没有 .set-row 的内边距，这里统一包一层 */}
            <div style={{ padding: '4px 16px 16px' }}>
              <div
                className="mono"
                style={{
                  fontSize: 11,
                  lineHeight: 1.6,
                  padding: '10px 12px',
                  borderRadius: 8,
                  background: 'var(--bg-2)',
                  border: '1px solid var(--line)',
                  overflowX: 'auto',
                  userSelect: 'all',
                }}
              >{`{
  "mcpServers": {
    "freeport": {
      "command": "/Applications/Freeport.app/Contents/MacOS/Freeport",
      "args": ["--mcp"]
    }
  }
}`}</div>
              <div className="t-sub" style={{ marginTop: 8 }}>
                <T
                  zh="六个工具：网关状态、可用模型、链路预演、额度余量、今日摘要、最近失败。改配置仍然只能在界面里做——MCP 的调用方是一个语言模型。"
                  en="Six tools: status, available models, route preview, quota, daily digest, recent failures. Configuration stays in the app — the MCP caller is a language model."
                />
              </div>
            </div>
          </div>

          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="启动与退出" en="Startup" />
              </h3>
              <span className="sub">
                <T zh="改完立即生效，不用重启" en="takes effect immediately" />
              </span>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="开机自启" en="Launch at login" />
                </div>
                <div className="d">
                  <T
                    zh="登录后自动启动，网关与注入保持可用；状态直接读系统登录项"
                    en="starts on login so the gateway stays reachable; state is read from the OS login items"
                  />
                </div>
              </div>
              <div className="v">
                {desktop && (
                  <Switch
                    on={autostart}
                    onChange={(v) => {
                      void window.freeport?.app
                        .setAutostart(v)
                        .then((on) => {
                          setAutostart(on);
                          toast(
                            on
                              ? tx(lang, '已加入登录项', 'Added to login items')
                              : tx(lang, '已从登录项移除', 'Removed from login items'),
                          );
                        })
                        .catch((e) => toast(String(e)));
                    }}
                  />
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="启动时最小化" en="Start minimized" />
                </div>
                <div className="d">
                  <T
                    zh="启动后只驻留托盘，网关照常服务；点托盘图标再开窗"
                    en="stays in the tray after launch, gateway keeps serving; click the tray icon to open"
                  />
                </div>
              </div>
              <div className="v">
                {desktop && (
                  <Switch
                    on={prefs?.startMinimized ?? false}
                    onChange={(v) => {
                      void window.freeport?.app
                        .setPrefs({ startMinimized: v })
                        .then(setPrefs)
                        .catch((e) => toast(String(e)));
                    }}
                  />
                )}
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="退出时清理注入" en="Clean up on quit" />
                </div>
                <div className="d">
                  <T
                    zh="退出前把已注入的 Agent 配置回滚到注入前（用户级）；只动 Freeport 自己写过的那几处，没注入过的客户端不受影响。项目级注入请在各仓库页面回滚。默认关——它会改你客户端的配置。"
                    en="Reverts user-level injected agent configs before quitting; only touches what Freeport wrote. Project-level injections are reverted per repo. Off by default — it edits your client configs."
                  />
                </div>
              </div>
              <div className="v">
                {desktop && (
                  <Switch
                    on={prefs?.cleanupOnQuit ?? false}
                    onChange={(v) => {
                      void window.freeport?.app
                        .setPrefs({ cleanupOnQuit: v })
                        .then(setPrefs)
                        .catch((e) => toast(String(e)));
                    }}
                  />
                )}
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
                  <option value="auto">
                    <T zh="自动（灰度后应用）" en="auto (canary then apply)" />
                  </option>
                  <option value="notify">
                    <T zh="仅提示（先问再应用）" en="notify (ask first)" />
                  </option>
                  <option value="pinned">
                    <T zh="锁定（不接收更新）" en="pinned (no updates)" />
                  </option>
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
                  <option value="stable">
                    <T zh="稳定" en="stable" />
                  </option>
                  <option value="beta">
                    <T zh="测试（beta）" en="beta" />
                  </option>
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
                  <T zh="本次下发" en="This delivery" />
                </div>
                <div className="d">
                  {feed?.servedFeats?.length
                    ? feed.servedFeats.join(' · ')
                    : tx(
                        lang,
                        '没有按能力单独下发的字段（公共目录 / 许可证未激活）',
                        'no feature packs in this delivery (public catalog / not activated)',
                      )}
                  {feed?.featsSkipped?.length ? `　·　${tx(lang, '跳过：', 'skipped: ')}${feed.featsSkipped.join('、')}` : ''}
                </div>
              </div>
              <div className="v">
                <span
                  className={`badge ${feed?.servedTier === 'pro' ? 'ok' : 'gray'}`}
                  title={tx(
                    lang,
                    '服务端按许可证的 feat 名单挑了哪几项；本机不据此拦功能',
                    'server picked packs from the license feat list; the client never gates on this',
                  )}
                >
                  {feed?.servedTier === 'pro' ? 'pro' : tx(lang, '公共目录', 'public')}
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
