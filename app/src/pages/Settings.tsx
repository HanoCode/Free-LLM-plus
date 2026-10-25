import { useEffect, useState } from 'react';
import { ExternalLink } from 'lucide-react';
import { T, tx, useLang } from '../lib/i18n';
import { useApp } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { CopyBtn, Switch, confirmDanger, toast } from '../components/ui';
import { ApiKeys } from '../components/ApiKeys';
import { Transfer } from '../components/Transfer';
import type { DesktopPrefs, GwBackupFile, UpdateCheck } from '../lib/bridge';

const REPO = 'https://github.com/HanoCode/Free-LLM-plus';

export function Settings() {
  const { lang, setLang, theme, setTheme } = useApp();
  const desktop = useGateway((s) => s.available);
  const gw = useGateway((s) => s.status);
  const setRequireAuth = useGateway((s) => s.setRequireAuth);
  const setRemote = useGateway((s) => s.setRemote);
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
  /** 进阶设置默认收起：压缩/缓存/目录/巡检等留给需要的用户 */
  const [showAdv, setShowAdv] = useState(
    () => localStorage.getItem('fp.settings.adv') === '1',
  );
  /** 开发者档：MCP JSON、协议入口明细。默认彻底隐藏，点两下才解锁（防误触，不设密码） */
  const [showDev, setShowDev] = useState(
    () => localStorage.getItem('fp.settings.dev') === '1',
  );
  const [devArm, setDevArm] = useState(false);

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

  /** 下载进度文本。null = 没在进行 */
  const [upProgress, setUpProgress] = useState<string | null>(null);

  useEffect(() => {
    const off = window.freeport?.gateway.onUpdaterProgress?.((p) => {
      if (p.phase === 'install') {
        setUpProgress(zh ? '安装中，即将重启…' : 'Installing, restarting…');
      } else if (p.total) {
        const pct = Math.round(((p.downloaded ?? 0) / p.total) * 100);
        setUpProgress(`${zh ? '下载中' : 'Downloading'} ${pct}%`);
      } else {
        setUpProgress(zh ? '下载中…' : 'Downloading…');
      }
    });
    return off;
  }, [zh]);

  /** 真去 GitHub 查，不弹「已是最新」的假提示。
   *  优先走带签名校验的更新通道（tauri-plugin-updater）；没配置或
   *  不可达时回落到 GitHub API + 打开浏览器页的老路。 */
  const checkUpdate = async () => {
    const api = window.freeport?.app;
    if (!api) return toast(zh ? '检查更新需要桌面端' : 'Update check needs the desktop app');
    setChecking(true);
    try {
      try {
        const u = await api.updaterCheck();
        setUpdate({
          current: u.current,
          latest: u.version ?? u.current,
          updateAvailable: u.available,
          url: null,
          notes: u.notes ?? null,
        });
        if (u.available) {
          toast(tx(lang, `有新版本 v${u.version}`, `v${u.version} is available`));
        } else {
          toast(tx(lang, `已是最新（v${u.current}）`, `Up to date (v${u.current})`));
        }
        return;
      } catch {
        // 更新通道未配置或不可达：走老通道
      }
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

  /** 自动更新：下载 + 安装 + 重启。失败把进度清掉并把原因说给用户听 */
  const installUpdate = async () => {
    const api = window.freeport?.app;
    if (!api) return;
    setUpProgress(zh ? '准备下载…' : 'Preparing…');
    try {
      await api.updaterInstall();
      // 成功路径不会走到这里：应用直接重启
    } catch (e) {
      setUpProgress(null);
      toast(String(e));
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

      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 14 }}>
        <button
          className="btn ghost sm"
          onClick={() => {
            const v = !showAdv;
            setShowAdv(v);
            localStorage.setItem('fp.settings.adv', v ? '1' : '0');
          }}
        >
          <T zh={showAdv ? '隐藏进阶设置' : '显示进阶设置'} en={showAdv ? 'Hide advanced' : 'Show advanced'} />
        </button>
        <span className="sub">
          {showAdv ? (
            <T zh="以下进阶项保留默认值即可" en="defaults are fine for most setups" />
          ) : (
            <T zh="压缩、缓存、目录、巡检、迁移备份都收在这里" en="compression, cache, catalog, patrol and backup live here" />
          )}
        </span>
        <button
          className={`btn ghost sm ${showDev ? '' : 'faint'}`}
          style={{ marginLeft: 'auto' }}
          title={zh ? '面向开发者的诊断信息' : 'developer diagnostics'}
          onClick={() => {
            if (showDev) {
              setShowDev(false);
              setDevArm(false);
              localStorage.setItem('fp.settings.dev', '0');
              return;
            }
            // 两步解锁：第一下只点亮确认，第二下才真的开
            if (!devArm) {
              setDevArm(true);
              return;
            }
            setShowDev(true);
            setDevArm(false);
            localStorage.setItem('fp.settings.dev', '1');
            toast(zh ? '开发者选项已开启' : 'Developer options enabled');
          }}
        >
          {showDev
            ? zh ? '隐藏开发者选项' : 'Hide developer options'
            : devArm
              ? zh ? '再点一次确认开启' : 'Click again to confirm'
              : zh ? '开发者选项' : 'Developer options'}
        </button>
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
                  {gw && gw.bind && gw.bind !== '127.0.0.1' ? (
                    <T
                      zh="正在对局域网监听——请确认你在可信网络里，且鉴权保持开启"
                      en="listening on LAN — make sure you trust this network and keep auth on"
                    />
                  ) : (
                    <T zh="只绑回环，不对外暴露" en="loopback only, never exposed" />
                  )}
                  {gw && !gw.running && (
                    <span style={{ color: 'var(--bad)' }}> · {gw.error || 'offline'}</span>
                  )}
                </div>
              </div>
              <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <span className="mono">{gw?.bind || '127.0.0.1'}:{gw?.port ?? 8787}</span>
                <CopyBtn value={addr} label="" tip="复制网关地址 / Copy address" />
              </div>
            </div>
            <div className="set-row">
              <div>
                <div className="k" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <T zh="远程接入" en="Remote access" />
                </div>
                <div className="d">
                  <T
                    zh="允许局域网里的其他设备（手机、另一台电脑）调用这台网关。开启后强制要求鉴权；改完重启网关生效。"
                    en="Let other devices on your LAN (phone, another computer) call this gateway. Auth is forced on; restart the gateway to apply."
                  />
                </div>
              </div>
              <div className="v">
                <Switch
                  on={!!gw && gw.bind !== '127.0.0.1'}
                  disabled={desktop === false}
                  onChange={(v) => {
                    if (!setRemote) return;
                    setRemote(v)
                      .then(() =>
                        toast(
                          v
                            ? '已切换到局域网监听，重启网关后生效'
                            : '已收回本机监听，重启网关后生效',
                        ),
                      )
                      .catch((e) => toast(e instanceof Error ? e.message : String(e)));
                  }}
                />
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
                {desktop && gw && <CopyBtn value={gw.authToken} label="" tip="复制 / Copy" />}
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
            {showAdv && (
            <>
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
                          ? tx(lang, '已开启智能过滤：结构化输出按结构裁，省得更多', 'Structure-aware trimming on: recognised shapes are trimmed by structure')
                          : tx(lang, '已关闭智能过滤：统一走首尾截断', 'Structure-aware trimming off: head + tail trimming only'),
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
                            ? tx(lang, '已开启响应缓存：完全相同的请求直接复用，不消耗上游额度', 'Response cache on: identical requests are reused without hitting upstream')
                            : tx(lang, '已关闭响应缓存：每个请求都会真的发出去', 'Response cache off: every request goes upstream for real'),
                        );
                      }}
                    />
                  </>
                )}
              </div>
            </div>
            {/* ── OTLP 导出（opt-in）──
                把请求元数据推给用户自己的观测栈。默认关、不含对话内容、
                绝不反压——这三条缺一条都不能做成默认行为。 */}
            <div className="set-row">
              <div>
                <div className="k">
                  <T zh="OTLP 导出" en="OTLP export" />
                </div>
                <div className="d">
                  <T
                    zh="把每次请求的 span（厂商 / 模型 / 耗时 / token / 折算成本，不含对话内容）按 OTLP/HTTP+JSON 推给你自己的 collector（Jaeger、Tempo、SigNoz…）。默认关：这会把请求元数据发往网络。"
                    en="Push one span per request (provider / model / latency / tokens / cost, never conversation content) to your own collector (Jaeger, Tempo, SigNoz…) over OTLP/HTTP+JSON. Off by default: this sends request metadata over the network."
                  />
                </div>
              </div>
              <div className="v">
                {desktop && gw && (
                  <Switch
                    on={gw.otlpEnabled}
                    onChange={(v) => {
                      void patchConfig({ otlpEnabled: v });
                      toast(
                        v
                          ? tx(lang, '已开启 OTLP 导出', 'OTLP export on')
                          : tx(lang, '已关闭 OTLP 导出', 'OTLP export off'),
                      );
                    }}
                  />
                )}
              </div>
            </div>
            {desktop && gw && gw.otlpEnabled && (
              <>
                <div className="set-row">
                  <div>
                    <div className="k">
                      <T zh="Collector 地址" en="Collector endpoint" />
                    </div>
                    <div className="d">
                      <T
                        zh="如 http://127.0.0.1:4318（自动补 /v1/traces）。失败只记日志，不影响请求。"
                        en="e.g. http://127.0.0.1:4318 (/v1/traces appended automatically). Failures are logged only; requests are never affected."
                      />
                    </div>
                  </div>
                  <div className="v">
                    <input
                      type="text"
                      defaultValue={gw.otlpEndpoint}
                      key={`otlp-${gw.otlpEndpoint}`}
                      placeholder="http://127.0.0.1:4318"
                      onBlur={(e) => {
                        const v = e.target.value.trim();
                        if (v === gw.otlpEndpoint) return;
                        void patchConfig({ otlpEndpoint: v });
                      }}
                      style={{
                        width: 260,
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
                  </div>
                </div>
                <div className="set-row">
                  <div>
                    <div className="k">
                      <T zh="自定义请求头" en="Custom headers" />
                    </div>
                    <div className="d">
                      <T
                        zh="一行一个 Key: Value，collector 要求鉴权时用。失焦即保存。"
                        en="One `Key: Value` per line, for collectors that require auth. Saved on blur."
                      />
                    </div>
                  </div>
                  <div className="v">
                    <textarea
                      defaultValue={gw.otlpHeaders.join('\n')}
                      key={`otlph-${gw.otlpHeaders.join('|')}`}
                      placeholder={'Authorization: Bearer …'}
                      rows={2}
                      onBlur={(e) => {
                        const v = e.target.value;
                        if (v === gw.otlpHeaders.join('\n')) return;
                        void patchConfig({ otlpHeaders: v.split('\n') });
                      }}
                      style={{
                        width: 260,
                        padding: '6px 8px',
                        border: '1px solid var(--line)',
                        borderRadius: 7,
                        background: 'var(--panel-2)',
                        color: 'var(--text)',
                        fontFamily: 'var(--mono)',
                        fontSize: 11.5,
                        resize: 'vertical',
                      }}
                    />
                  </div>
                </div>
              </>
            )}
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
                      toast(
                        v === 0
                          ? tx(lang, '已取消单来源限速', 'Per-source rate limit removed')
                          : tx(lang, `单来源限速已设为 ${v} 次/分钟`, `Per-source rate limit set to ${v}/min`),
                      );
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
                    onClick={async () => {
                      if (
                        !(await confirmDanger({
                          zh: '解除全部冷却？被限速的凭据会立刻重新参与路由，可能再次触发上游限流。',
                          en: 'Clear all cooldowns? Rate-limited credentials will rejoin routing immediately and may hit upstream limits again.',
                        }))
                      )
                        return;
                      void clearCooldowns();
                      toast(tx(lang, '已清除全部冷却', 'All cooldowns cleared'));
                    }}
                  >
                    <T zh="全部解除" en="Clear all" />
                  </button>
                )}
              </div>
            </div>
            </>
            )}
            {showDev && (
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
            )}
          </div>

          <ApiKeys desktop={desktop} />

          {showAdv && <Transfer desktop={desktop} />}

          {showDev && (
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
          )}

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

          {showAdv && (
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
          )}
        </div>

        <div className="stack">
          {showAdv && (
          <>
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
                  <T
                    zh="按已配厂商的日额度 1% 自动扩展（下限 60、上限 2400）"
                    en="scales with your configured daily quota (min 60, max 2400)"
                  />
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
          </>
          )}

          <ProCard />

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
              <div className="v" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                {upProgress && <span className="sub">{upProgress}</span>}
                {update?.updateAvailable && !upProgress && (
                  <button className="btn sm" onClick={() => void installUpdate()}>
                    <T zh="自动更新" en="Auto-update" />
                  </button>
                )}
                {update?.updateAvailable && update.url && !upProgress && (
                  <button
                    className="btn ghost sm"
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
                  <T zh="专有软件（EULA）· 本地优先，无遥测；崩溃日志只留在本机" en="Proprietary (EULA) · local-first, no telemetry; crash logs stay local" />
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

/* ── 增强功能开关 ─────────────────────────────
   全部免费可用、全部本机运行：预热、解粘阈值、粘性窗口、加密自动备份。 */

function ProCard() {
  const lang = useLang();
  const zh = lang === 'zh';
  const pro = useGateway((s) => s.pro);
  const loadPro = useGateway((s) => s.loadPro);
  const updatePro = useGateway((s) => s.updatePro);
  const backupNow = useGateway((s) => s.backupNow);
  const backupList = useGateway((s) => s.backupList);
  const backupRestore = useGateway((s) => s.backupRestore);

  const [backups, setBackups] = useState<GwBackupFile[] | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void loadPro();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const patch = async (p: Parameters<typeof updatePro>[0], okMsg: string) => {
    setBusy(true);
    try {
      await updatePro(p);
      toast(okMsg);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const refreshList = async () => {
    try {
      setBackups(await backupList());
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    }
  };

  const doBackupNow = async () => {
    setBusy(true);
    try {
      const path = await backupNow();
      toast(zh ? `已备份到 ${path}` : `Backed up to ${path}`);
      await refreshList();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const doRestore = async (name: string) => {
    if (
      !(await confirmDanger({
        zh: `从快照 ${name} 恢复？按合并方式导入：同名配置以快照为准，其余保留。`,
        en: `Restore from snapshot ${name}? Imports in merge mode: snapshot wins on conflicts, everything else is kept.`,
      }))
    )
      return;
    setBusy(true);
    try {
      await backupRestore(name);
      toast(zh ? '已恢复，列表与额度已刷新' : 'Restored; lists and quota refreshed');
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const backup = pro?.backup;
  const inputStyle: React.CSSProperties = {
    width: 210,
    height: 28,
    padding: '0 8px',
    border: '1px solid var(--line)',
    borderRadius: 7,
    background: 'var(--panel-2)',
    color: 'var(--text)',
    fontFamily: 'var(--mono)',
    fontSize: 11.5,
  };
  const off = busy;

  return (
    <div className="card">
      <div className="card-head">
        <h3>
          <T zh="增强功能" en="Enhancements" />
        </h3>
        <span className="sub">
          <T zh="全部免费可用，数据不出本机" en="all free, everything runs locally" />
        </span>
      </div>

      {!pro ? (
        <div className="card-body">
          <span className="sub">
            <T zh="读取中…" en="loading…" />
          </span>
        </div>
      ) : (
        <>
          <div className="set-row">
            <div>
              <div className="k">
                <T zh="会话预热" en="Session prewarm" />
              </div>
              <div className="d">
                <T
                  zh="粘住的通道剩余额度跌破 30% 时，提前探针下一个候选——切换发生在你需要它之前，而不是正好卡在半截。"
                  en="When the sticky channel drops below 30% headroom, probe the next candidate early — the switch happens before you need it."
                />
              </div>
            </div>
            <div className="v">
              <Switch
                on={pro.prewarm}
                disabled={off}
                onChange={(v) => void patch({ prewarm: v }, v ? '已开启会话预热' : '已关闭会话预热')}
              />
            </div>
          </div>

          <div className="set-row">
            <div>
              <div className="k">
                <T zh="解粘阈值" en="Unstick threshold" />
              </div>
              <div className="d">
                <T
                  zh="粘住的会话剩余额度低于这个比例时主动解粘，换更宽裕的通道。默认 0.10（10%）；清空即恢复默认。"
                  en="The sticky session unsticks below this remaining ratio and moves to a roomier channel. Default 0.10; clear to restore."
                />
              </div>
            </div>
            <div className="v" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
              <input
                type="number"
                min={0.02}
                max={0.5}
                step={0.01}
                defaultValue={pro.unstickRatio ?? ''}
                key={`unstick-${pro.unstickRatio ?? 'default'}`}
                placeholder="0.10"
                disabled={off}
                onBlur={(e) => {
                  const raw = e.target.value.trim();
                  if (raw === '') {
                    if (pro.unstickRatio == null) return;
                    void patch({ unstickRatio: null }, '解粘阈值已恢复默认 0.10');
                    return;
                  }
                  const v = Math.max(0.02, Math.min(0.5, Number(raw) || 0.1));
                  if (v === pro.unstickRatio) return;
                  void patch({ unstickRatio: v }, `解粘阈值已设为 ${Math.round(v * 100)}%`);
                }}
                style={{ ...inputStyle, width: 90 }}
              />
              <span className="faint" style={{ fontSize: 11 }}>
                0.02 – 0.5
              </span>
            </div>
          </div>

          <div className="set-row">
            <div>
              <div className="k">
                <T zh="粘性保持" en="Session stickiness" />
              </div>
              <div className="d">
                <T
                  zh="同一段对话粘回上次的通道，第二次起读上游缓存：更快、更省额度。免费版固定 30 分钟；Pro 可调 5 分钟–24 小时，长会话不用中途换家重烧缓存。"
                  en="A conversation sticks to its last channel so upstream prompt caches hit: faster and cheaper. Free is fixed at 30 minutes; Pro tunes 5 minutes – 24 hours."
                />
              </div>
            </div>
            <div className="v">
              <select
                className="select"
                value={String(pro.stickTtlMin ?? 30)}
                disabled={off}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  void patch(
                    { stickTtlMin: v === 30 ? null : v },
                    v === 30 ? '粘性窗口已恢复默认 30 分钟' : `粘性窗口已设为 ${v >= 60 ? `${v / 60} 小时` : `${v} 分钟`}`,
                  );
                }}
              >
                <option value={5}>5 分钟</option>
                <option value={15}>15 分钟</option>
                <option value={30}>30 分钟（默认）</option>
                <option value={60}>1 小时</option>
                <option value={120}>2 小时</option>
                <option value={360}>6 小时</option>
                <option value={720}>12 小时</option>
                <option value={1440}>24 小时</option>
              </select>
            </div>
          </div>

          <div className="set-row">
            <div>
              <div className="k">
                <T zh="加密自动备份" en="Encrypted auto-backup" />
              </div>
              <div className="d">
                <T
                  zh="定期把全部配置打包成迁移信封（Argon2id + AES-256-GCM，口令由本机主密钥派生，不落盘）。恢复走合并，不会整盘覆盖。"
                  en="Periodically packs all config into a transfer envelope (Argon2id + AES-256-GCM, passphrase derived from the local master key). Restore merges; never wipes."
                />
              </div>
            </div>
            <div className="v">
              <Switch
                on={backup?.enabled ?? false}
                disabled={off}
                onChange={(v) => void patch({ backupEnabled: v }, v ? '已开启自动备份' : '已关闭自动备份')}
              />
            </div>
          </div>

          {(backup?.enabled ?? false) && (
            <>
              <div className="set-row">
                <div>
                  <div className="k">
                    <T zh="备份目录" en="Backup directory" />
                  </div>
                  <div className="d">
                    <T
                      zh="绝对路径。留空用实例目录下的 backups/。目录要你自己能找到——备份的意义在网关之外。"
                      en="Absolute path; empty uses backups/ inside the instance dir. Pick somewhere you can find — a backup only matters outside the app."
                    />
                  </div>
                </div>
                <div className="v">
                  <input
                    defaultValue={backup?.dir ?? ''}
                    key={`bdir-${backup?.dir ?? ''}`}
                    placeholder={zh ? '留空 = 实例目录/backups' : 'empty = instance dir/backups'}
                    spellCheck={false}
                    disabled={busy}
                    onBlur={(e) => {
                      const v = e.target.value.trim();
                      if (v === (backup?.dir ?? '')) return;
                      void patch({ backupDir: v }, v ? `备份目录已设为 ${v}` : '备份目录已恢复默认');
                    }}
                    style={inputStyle}
                  />
                </div>
              </div>
              <div className="set-row">
                <div>
                  <div className="k">
                    <T zh="间隔与保留" en="Interval & retention" />
                  </div>
                  <div className="d">
                    <T
                      zh={`每 ${backup?.intervalHours ?? 24} 小时一份，最多留 ${backup?.keep ?? 7} 份，旧的自动清`}
                      en={`every ${backup?.intervalHours ?? 24}h, keep ${backup?.keep ?? 7}, older ones pruned`}
                    />
                  </div>
                </div>
                <div className="v" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                  <select
                    className="select"
                    value={backup?.intervalHours ?? 24}
                    disabled={busy}
                    onChange={(e) =>
                      void patch(
                        { backupIntervalHours: Number(e.target.value) },
                        `备份间隔已设为 ${e.target.value} 小时`,
                      )
                    }
                  >
                    <option value={6}>6h</option>
                    <option value={12}>12h</option>
                    <option value={24}>24h</option>
                    <option value={48}>48h</option>
                  </select>
                  <input
                    type="number"
                    min={1}
                    max={30}
                    defaultValue={backup?.keep ?? 7}
                    key={`bkeep-${backup?.keep ?? 7}`}
                    disabled={busy}
                    onBlur={(e) => {
                      const v = Math.max(1, Math.min(30, Number(e.target.value) || 7));
                      if (v === (backup?.keep ?? 7)) return;
                      void patch({ backupKeep: v }, `保留份数已设为 ${v}`);
                    }}
                    style={{ ...inputStyle, width: 70 }}
                  />
                  <button className="btn ghost sm" disabled={busy} onClick={() => void doBackupNow()}>
                    <T zh="立即备份" en="Back up now" />
                  </button>
                </div>
              </div>

              <div className="set-row">
                <div>
                  <div className="k">
                    <T zh="快照" en="Snapshots" />
                  </div>
                  <div className="d">
                    {backup?.lastAt
                      ? tx(
                          lang,
                          `上次备份：${new Date(backup.lastAt * 1000).toLocaleString()}`,
                          `Last backup: ${new Date(backup.lastAt * 1000).toLocaleString()}`,
                        )
                      : tx(lang, '还没有备份过', 'never backed up yet')}
                  </div>
                </div>
                <div className="v">
                  <button className="btn ghost sm" disabled={busy} onClick={() => void refreshList()}>
                    <T zh="刷新列表" en="Refresh list" />
                  </button>
                </div>
              </div>

              {backups !== null && backups.length > 0 && (
                <div className="card-body" style={{ paddingTop: 0 }}>
                  <div className="chain-list">
                    {backups.map((f) => (
                      <div className="chain-row" key={f.name}>
                        <code style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis' }}>{f.name}</code>
                        <span className="faint" style={{ flex: 'none' }}>
                          {(f.bytes / 1024).toFixed(1)} KB
                        </span>
                        <button className="btn ghost sm" disabled={busy} onClick={() => void doRestore(f.name)}>
                          <T zh="恢复" en="Restore" />
                        </button>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </>
          )}
        </>
      )}
    </div>
  );
}
