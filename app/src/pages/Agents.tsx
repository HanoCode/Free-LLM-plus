import { useCallback, useEffect, useState } from 'react';
import {
  AlertTriangle,
  Copy,
  Download,
  FileCode2,
  FolderGit2,
  RefreshCw,
  ShieldCheck,
  Terminal,
  Undo2,
  Wand2,
  X,
} from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, Status, toast } from '../components/ui';
import { BrandLogo } from '../components/BrandLogo';
import type { AgentDiffLine, AgentInfo, ProjectInjection } from '../lib/bridge';

/** 每个工具的一句话介绍：面向「这个工具是什么」，而不是「我们改了哪个文件」。 */
const BLURB: Record<string, { zh: string; en: string }> = {
  'claude-code': {
    zh: 'Anthropic 的终端智能体，会在你的仓库里规划、改文件、跑命令。',
    en: 'Anthropic’s terminal agent: plans, edits and runs commands in your repo.',
  },
  'claude-code-vscode': {
    zh: 'Claude Code 的 VS Code 扩展形态，共享同一份 settings.json 配置。',
    en: 'The VS Code extension flavor of Claude Code, sharing the same settings.json.',
  },
  codex: {
    zh: 'OpenAI 的终端智能体，先在本地沙箱里跑一遍你的任务。',
    en: 'OpenAI’s terminal agent; runs your task in a local sandbox first.',
  },
  cline: {
    zh: 'VS Code 扩展，每一次改动和命令都要你点头才执行。',
    en: 'VS Code extension that asks before every edit and command.',
  },
  opencode: {
    zh: 'SST 的开源终端智能体，配置写在项目或全局 json 里。',
    en: 'SST’s open-source terminal agent, configured via project or global json.',
  },
  'vscode-chat': {
    zh: 'VS Code 内置 Chat 的模型接入点，通过 settings.json 指向网关。',
    en: 'VS Code Chat’s model endpoint, pointed at the gateway via settings.json.',
  },
  crush: {
    zh: 'Charm 的多会话终端智能体，providers 下加一个条目即可。',
    en: 'Charm’s multi-session terminal agent; add one provider entry.',
  },
  jetbrains: {
    zh: 'JetBrains AI Assistant 的 Claude Agent / ACP 模式。',
    en: 'JetBrains AI Assistant in Claude Agent / ACP mode.',
  },
  'claude-desktop': {
    zh: 'Claude 桌面版，在 Config.ts 里加一行环境变量。',
    en: 'The Claude desktop app; add one env line in Config.ts.',
  },
  continue: {
    zh: '开源 IDE 助手，config.yaml 里注释与端点一一对应。',
    en: 'Open-source IDE assistant; config.yaml maps 1:1 to endpoints.',
  },
  'gemini-cli': {
    zh: 'Google 的 Gemini 命令行工具，用 GOOGLE_GEMINI_BASE_URL 接入。',
    en: 'Google’s Gemini CLI, wired via GOOGLE_GEMINI_BASE_URL.',
  },
  roo: {
    zh: 'Roo Code 的配置存放在私有存档里，采用托管变量方式。',
    en: 'Roo Code keeps config in a private store; uses hosted variables.',
  },
};

function blurb(a: AgentInfo) {
  return BLURB[a.id] ?? { zh: a.note, en: a.note };
}

/**
 * 未安装时的安装方式。
 * 只放官方渠道：命令取各工具官方文档，链接取官方安装页；没把握的一律不给，
 * 让用户自己去官网装——界面不编造安装地址。
 */
const INSTALL: Record<string, { cmd?: string; url?: string }> = {
  'claude-code': {
    cmd: 'npm install -g @anthropic-ai/claude-code',
    url: 'https://docs.anthropic.com/en/docs/claude-code/setup',
  },
  'claude-code-vscode': {
    url: 'https://marketplace.visualstudio.com/items?itemName=anthropic.claude-code',
  },
  codex: { cmd: 'npm install -g @openai/codex', url: 'https://github.com/openai/codex' },
  cline: {
    url: 'https://marketplace.visualstudio.com/items?itemName=saoudrizwan.claude-dev',
  },
  opencode: { cmd: 'brew install sst/tap/opencode', url: 'https://opencode.ai' },
  'vscode-chat': { url: 'https://code.visualstudio.com/' },
  crush: {
    cmd: 'brew install charmbracelet/tap/crush',
    url: 'https://github.com/charmbracelet/crush',
  },
  jetbrains: { url: 'https://www.jetbrains.com/ai/' },
  'claude-desktop': { url: 'https://claude.ai/download' },
  continue: {
    url: 'https://marketplace.visualstudio.com/items?itemName=Continue.continue',
  },
  'gemini-cli': {
    cmd: 'npm install -g @google/gemini-cli',
    url: 'https://github.com/google-gemini/gemini-cli',
  },
  roo: {
    url: 'https://marketplace.visualstudio.com/items?itemName=RooVeterinaryInc.roo-cline',
  },
};

/** 接入模式：决定模型表从哪来，比「写了哪个文件」更值得展示。 */
function Mode({ kind }: { kind: string }) {
  switch (kind) {
    case 'self':
      return <T zh="自发现" en="self-discover" />;
    case 'pull':
      return <T zh="客户端拉取" en="client pull" />;
    case 'push':
      return <T zh="写入模型表" en="pushed list" />;
    default:
      return <T zh="寄宿 env" en="hosted env" />;
  }
}

function Protocol({ p }: { p: string }) {
  return <>{p === 'anthropic' ? 'Anthropic' : p === 'gemini' ? 'Gemini' : 'OpenAI'}</>;
}

function StateTag({ state }: { state: string }) {
  if (state === 'injected')
    return (
      <Status tone="ok">
        <T zh="已注入" en="on" />
      </Status>
    );
  if (state === 'detected')
    return (
      <Status tone="idle">
        <T zh="未注入" en="off" />
      </Status>
    );
  if (state === 'missing')
    return (
      <Badge tone="gray">
        <T zh="未安装" en="absent" />
      </Badge>
    );
  if (state === 'unsupported')
    return (
      <Badge tone="warn">
        <T zh="待适配" en="unsupported" />
      </Badge>
    );
  return (
    <Badge tone="gray">
      <T zh="手动" en="manual" />
    </Badge>
  );
}

/** 复制一行文本：桌面端优先走剪贴板，失败时提示。 */
async function copyText(text: string, label?: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast(label ? `已复制${label}` : '已复制 / Copied');
  } catch {
    toast('复制失败：请手动选择文本');
  }
}

/** 命令 / 键值盒子：FreeLLMAPI 风格，右侧一个复制按钮。 */
function CmdBox({ lines, label }: { lines: string[]; label?: string }) {
  return (
    <div className="cmdbox">
      <div className="cmdbox-text">
        {lines.map((l, i) => (
          <div key={i}>{l}</div>
        ))}
      </div>
      <button
        className="cmdbox-copy"
        title="复制 / Copy"
        onClick={() => void copyText(lines.join('\n'), label)}
      >
        <Copy size={12} />
      </button>
    </div>
  );
}

export function Agents() {
  const api = window.freeport?.gateway;

  const [list, setList] = useState<AgentInfo[]>([]);
  /** 弹窗展示的 Agent id；null = 关闭 */
  const [openId, setOpenId] = useState<string | null>(null);
  const [diff, setDiff] = useState<AgentDiffLine[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [port, setPort] = useState<number | null>(null);
  const [prefix, setPrefix] = useState<string>('fp-preview');

  const selected = list.find((a) => a.id === openId) ?? null;

  const load = useCallback(async () => {
    if (!api) return;
    try {
      const next = await api.agents();
      setList(next);
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  // 端口与 key 前缀只给「手动步骤」用，失败不影响主流程
  useEffect(() => {
    if (!api) return;
    void (async () => {
      try {
        const st = await api.status();
        setPort(st.port ?? null);
        const keys = await api.keys();
        const k = keys.find((x) => x.name.startsWith('agent:'));
        if (k) setPrefix(k.prefix);
      } catch {
        /* 网关未启动时留空，步骤里给占位值 */
      }
    })();
  }, [api, list]);

  // 打开弹窗就读一次磁盘真实内容（不缓存）
  useEffect(() => {
    if (!api || !selected) return;
    setDiff(null);
    if (
      selected.strategy === 'guide' ||
      selected.state === 'missing' ||
      selected.state === 'unsupported'
    )
      return;
    let alive = true;
    void (async () => {
      try {
        const d = await api.agentPreview(selected.id);
        if (alive) setDiff(d);
      } catch (e) {
        if (alive) toast(String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [api, selected?.id, selected?.state, selected?.strategy]);

  async function apply(a: AgentInfo) {
    if (!api) return;
    setBusy(a.id);
    try {
      const info = await api.agentApply(a.id);
      toast(
        `已注入 ${info.name}${info.backup ? `，备份 ${info.backup.split('/').pop()}` : ''} / Injected`,
      );
      await load();
      if (openId === a.id) setDiff(await api.agentPreview(a.id).catch(() => null));
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function revert(a: AgentInfo) {
    if (!api) return;
    setBusy(a.id);
    try {
      const info = await api.agentRevert(a.id);
      toast(`已回滚 ${info.name}，文件恢复原样 / Rolled back`);
      await load();
      if (openId === a.id) setDiff(await api.agentPreview(a.id).catch(() => null));
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  }

  const injected = list.filter((a) => a.state === 'injected').length;
  const base = port ? `http://127.0.0.1:${port}` : 'http://127.0.0.1:8787';
  const keyPrefix = `${prefix}…`;

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="注入" en="Agents" />
          </h1>
          <p>
            <T
              zh="通过可安全备份的配置，把编程工具接入本机免费模型网关。只改自己写的那几行，随时一键恢复原样。"
              en="Wire coding tools to the local free-model gateway via safely backed-up configs. Only our own lines are written, always revertible."
            />
          </p>
        </div>
        <div className="head-actions">
          <Badge tone={injected > 0 ? 'ok' : 'gray'}>
            <T zh={`${injected} 个已接管`} en={`${injected} injected`} />
          </Badge>
          <button className="btn ghost" onClick={() => void load()} disabled={!!busy}>
            <RefreshCw />
            <T zh="重新扫描" en="Rescan" />
          </button>
        </div>
      </div>

      {/* 渠道来源风险提示：上游会校验请求渠道，滥用接入有封号风险 */}
      <div className="risk-note" style={{ marginBottom: 12 }}>
        <AlertTriangle />
        <p>
          <T
            zh="近期 WorkBuddy 请求会校验渠道来源，请合理使用接入功能，可能会引起封号风险！"
            en="WorkBuddy now verifies the channel origin of requests. Use the injection feature responsibly — account bans are a real risk!"
          />
        </p>
      </div>

      {err && (
        <div className="risk-note" style={{ marginBottom: 12 }}>
          <AlertTriangle />
          <p>{err}</p>
        </div>
      )}

      {!api ? (
        <div className="card">
          <div className="card-body" style={{ color: 'var(--text-2)', fontSize: 12.5 }}>
            <T
              zh="注入需要在桌面端运行：它要读写本机配置文件，浏览器里做不到。"
              en="Injection needs the desktop app — it reads and writes local config files."
            />
          </div>
        </div>
      ) : list.length === 0 ? (
        <div className="card">
          <div className="card-body" style={{ color: 'var(--text-2)', fontSize: 12.5 }}>
            <T zh="正在扫描本机 Agent…" en="Scanning for agents…" />
          </div>
        </div>
      ) : (
        <>
          {/* 快速开始：与厂商无关的公共信息，一次复制到处可用 */}
          <div className="card quickstart">
            <div className="quickstart-head">
              <b>
                <T zh="快速开始" en="Quick start" />
              </b>
              <span className="sub">
                <T
                  zh="任意工具的手动接入都是同一组地址和密钥。"
                  en="Manual setup is the same base URL and key for every tool."
                />
              </span>
            </div>
            <div className="quickstart-row">
              <div className="qs-field">
                <span className="qs-label">BASE URL</span>
                <CmdBox lines={[base]} label="Base URL" />
              </div>
              <div className="qs-field">
                <span className="qs-label">API KEY</span>
                <CmdBox lines={[keyPrefix]} label="Key 前缀" />
              </div>
            </div>
            <div className="sub" style={{ fontSize: 11.5 }}>
              <T
                zh={`当前统一密钥：${prefix}…（完整密钥在设置页新建虚拟 key 后获取）`}
                en={`Unified key: ${prefix}… (create a full virtual key in Settings)`}
              />
            </div>
          </div>

          {/* Agent 卡片墙：等高，操作区吸底 */}
          <div className="agent-cards">
            {list.map((a) => {
              const b = blurb(a);
              const install = INSTALL[a.id];
              return (
                <div className="card acard" key={a.id}>
                  <div className="acard-top">
                    <span className="acard-logo">
                      <BrandLogo id={a.id} name={a.name} size={26} />
                    </span>
                    <div style={{ minWidth: 0 }}>
                      <h4>{a.name}</h4>
                      <div className="meta">
                        <Protocol p={a.protocol} /> · <Mode kind={a.discovery} />
                      </div>
                    </div>
                    <span className="acard-state">
                      <StateTag state={a.state} />
                    </span>
                  </div>

                  <p className="acard-blurb">
                    <T zh={b.zh} en={b.en} />
                  </p>

                  <div className="acard-sec">
                    {a.state === 'detected' || a.state === 'injected' ? (
                      <>
                        <b>
                          <T zh="自动配置" en="Auto config" />
                        </b>
                        <span className="sub">
                          <T
                            zh="为您写出该工具的配置文件，并指向此网关。写入前先给你看 diff。"
                            en="Writes this tool's config file, pointed at this gateway. Diff shown before writing."
                          />
                        </span>
                        {a.path && (
                          <div className="acard-path">
                            <FileCode2 size={11} />
                            <span className="mono">{a.path.split('/').slice(-2).join('/')}</span>
                          </div>
                        )}
                      </>
                    ) : a.state === 'guide' ? (
                      <>
                        <b>
                          <T zh="手动连接" en="Manual setup" />
                        </b>
                        <span className="sub">
                          <T
                            zh="在该工具的设置中输入基础 URL 和密钥，Freeport 不代写。"
                            en="Enter the base URL and key in the tool's settings; Freeport does not write this one."
                          />
                        </span>
                      </>
                    ) : a.state === 'missing' ? (
                      <>
                        <b>
                          <T zh="未检测到安装" en="Not installed" />
                        </b>
                        <span className="sub">
                          {install ? (
                            <T
                              zh="可以按官方方式安装，装好后回来重新扫描即可注入。"
                              en="Install it the official way, then rescan here to inject."
                            />
                          ) : (
                            <T
                              zh={`没找到 ${a.name} 的安装目录。请到官网安装后点「重新扫描」。`}
                              en={`${a.name} was not found. Install it from the official site, then rescan.`}
                            />
                          )}
                        </span>
                      </>
                    ) : (
                      <>
                        <b>
                          <T zh="待适配" en="Unsupported" />
                        </b>
                        <span className="sub">
                          <T
                            zh={`网关还没提供 ${a.protocol} 原生协议，现在注入也连不上，所以不写文件。`}
                            en={`The gateway does not speak ${a.protocol} natively yet, so nothing is written.`}
                          />
                        </span>
                      </>
                    )}
                  </div>

                  {/* 吸底操作区：等高卡片的关键 */}
                  <div className="acard-actions">
                    {a.state === 'detected' && (
                      <>
                        <button
                          className="btn primary sm"
                          onClick={() => void apply(a)}
                          disabled={!!busy}
                        >
                          <Wand2 size={12} />
                          <T zh="一键注入" en="Inject" />
                        </button>
                        <button className="btn ghost sm" onClick={() => setOpenId(a.id)}>
                          <FileCode2 size={12} />
                          <T zh="查看 diff" en="View diff" />
                        </button>
                      </>
                    )}
                    {a.state === 'injected' && (
                      <>
                        <button className="btn ghost sm" onClick={() => setOpenId(a.id)}>
                          <FileCode2 size={12} />
                          <T zh="查看 diff" en="View diff" />
                        </button>
                        <button
                          className="btn danger sm"
                          onClick={() => void revert(a)}
                          disabled={!!busy}
                        >
                          <Undo2 size={12} />
                          <T zh="回滚" en="Rollback" />
                        </button>
                      </>
                    )}
                    {a.state === 'guide' && (
                      <button className="btn ghost sm" onClick={() => setOpenId(a.id)}>
                        <Terminal size={12} />
                        <T zh="查看步骤" en="Steps" />
                      </button>
                    )}
                    {a.state === 'missing' && (
                      <>
                        <button
                          className="btn primary sm"
                          onClick={() => setOpenId(a.id)}
                          disabled={!install}
                        >
                          <Download size={12} />
                          <T zh="一键安装" en="Install" />
                        </button>
                        {install?.url && (
                          <button
                            className="btn ghost sm"
                            onClick={() => window.freeport?.shell.openExternal(install.url!)}
                          >
                            <T zh="官网" en="Site" />
                          </button>
                        )}
                      </>
                    )}
                    {a.state === 'unsupported' && (
                      <button className="btn ghost sm" onClick={() => setOpenId(a.id)}>
                        <T zh="详情" en="Details" />
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}

      <div className="hint-line">
        <Terminal size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T
          zh="注入五件套：托管块标记、写入前 diff、原子写 + 文件锁、幂等（重复注入不重复加行）、退出 / 卸载时清理。"
          en="Five rules: managed markers, diff before write, atomic write with lock, idempotent, cleanup on exit."
        />
      </div>

      {selected && api && (
        <AgentModal
          agent={selected}
          diff={diff}
          busy={busy === selected.id}
          base={base}
          keyPrefix={prefix}
          onClose={() => setOpenId(null)}
          onApply={() => void apply(selected)}
          onRevert={() => void revert(selected)}
          onRescan={async () => {
            await load();
            setOpenId(null);
          }}
        />
      )}
    </>
  );
}

/* ── 详情弹窗：diff / 手动步骤 / 安装指引 ─────────────── */
function AgentModal({
  agent,
  diff,
  busy,
  base,
  keyPrefix,
  onClose,
  onApply,
  onRevert,
  onRescan,
}: {
  agent: AgentInfo;
  diff: AgentDiffLine[] | null;
  busy: boolean;
  base: string;
  keyPrefix: string;
  onClose: () => void;
  onApply: () => void;
  onRevert: () => void;
  onRescan: () => Promise<void>;
}) {
  const install = INSTALL[agent.id];
  const adds = diff?.filter((d) => d.kind === 'add').length ?? 0;
  const dels = diff?.filter((d) => d.kind === 'del').length ?? 0;
  const writable = agent.state === 'detected' || agent.state === 'injected';

  return (
    <div className="modal-mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal agent-modal">
        <div className="agent-modal-head">
          <span className="acard-logo">
            <BrandLogo id={agent.id} name={agent.name} size={26} />
          </span>
          <div style={{ minWidth: 0 }}>
            <h3 style={{ margin: 0 }}>{agent.name}</h3>
            <div className="sub" style={{ margin: '2px 0 0' }}>
              <Protocol p={agent.protocol} /> · <Mode kind={agent.discovery} />
            </div>
          </div>
          <span className="agent-modal-status">
            <StateTag state={agent.state} />
            <button className="cmdbox-copy" onClick={onClose} title="关闭 / Close">
              <X size={14} />
            </button>
          </span>
        </div>

        <div className="agent-modal-body">
          {agent.state === 'missing' ? (
            <>
              <div className="risk-note">
                <Download />
                <p>
                  {install ? (
                    <T
                      zh={`没检测到 ${agent.name} 的安装目录。按下面任一方式装好，再点「我已装好，重新扫描」。`}
                      en={`${agent.name} was not found. Install it with one of the options below, then rescan.`}
                    />
                  ) : (
                    <T
                      zh={`没检测到 ${agent.name} 的安装目录。请到官网下载安装，装好后重新扫描。`}
                      en={`${agent.name} was not found. Install it from the official site, then rescan.`}
                    />
                  )}
                </p>
              </div>
              {install?.cmd && (
                <div className="field" style={{ marginBottom: 0 }}>
                  <label>
                    <T zh="命令行安装" en="INSTALL VIA CLI" />
                  </label>
                  <CmdBox lines={[install.cmd]} label="安装命令" />
                </div>
              )}
              {install?.url && (
                <div className="field" style={{ marginBottom: 0 }}>
                  <label>
                    <T zh="官方下载页" en="OFFICIAL DOWNLOAD" />
                  </label>
                  <div className="agent-url-row">
                    <span className="mono">{install.url}</span>
                    <button
                      className="btn ghost sm"
                      onClick={() => window.freeport?.shell.openExternal(install.url!)}
                    >
                      <Download size={11} />
                      <T zh="打开下载页" en="Open" />
                    </button>
                  </div>
                </div>
              )}
            </>
          ) : agent.state === 'guide' ? (
            <>
              <div className="risk-note">
                <ShieldCheck />
                <p>
                  <T
                    zh={`${agent.name}：${agent.note}。Freeport 只给步骤，不代写。`}
                    en={`${agent.name}: ${agent.note}. Freeport only gives steps.`}
                  />
                </p>
              </div>
              <ol className="agent-steps">
                <li>
                  <T zh="打开设置 → Models / Providers" en="Open Settings → Models / Providers" />
                </li>
                <li>
                  <span>
                    Base URL <span className="kbd">{base}</span>
                  </span>
                </li>
                <li>
                  <span>
                    API Key <span className="kbd">{keyPrefix}…</span>{' '}
                    <T zh="（在设置页新建一把）" en="(create one in Settings)" />
                  </span>
                </li>
                <li>
                  <T
                    zh="模型名填 freeport/auto，保存后发一条消息验证"
                    en="Model: freeport/auto. Send one message."
                  />
                </li>
              </ol>
            </>
          ) : agent.state === 'unsupported' ? (
            <div className="risk-note">
              <AlertTriangle />
              <p>
                <T
                  zh={`网关还没提供 ${agent.protocol} 原生协议，现在注入也连不上，所以不写文件。`}
                  en={`The gateway does not speak ${agent.protocol} natively yet, so nothing is written.`}
                />
              </p>
            </div>
          ) : (
            <>
              <div className="filebar agent-filebar">
                <FileCode2 />
                {agent.path ?? agent.note}
              </div>
              <div className="diff agent-diff">
                {diff === null ? (
                  <div className="ln ctx">
                    <span className="gutter">·</span>
                    <span>
                      <T zh="读取文件中…" en="Reading file…" />
                    </span>
                  </div>
                ) : diff.length === 0 ? (
                  <div className="ln ctx">
                    <span className="gutter">·</span>
                    <span>
                      <T zh="文件里已经是目标状态，无需改动。" en="Already up to date." />
                    </span>
                  </div>
                ) : (
                  diff.map((l, i) => (
                    <div className={`ln ${l.kind}`} key={i}>
                      <span className="gutter">{l.no}</span>
                      <span>
                        {l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '} {l.text}
                        {l.managed && <span className="managed-tag">freeport</span>}
                      </span>
                    </div>
                  ))
                )}
              </div>
              <div className="agent-diff-foot">
                <span className="sub">
                  {agent.state === 'injected' ? (
                    <T
                      zh={`${adds} 行新增 · ${dels} 行删除 · 备份 ${agent.backup?.split('/').pop() ?? '—'}`}
                      en={`${adds} added · ${dels} removed · backup ${agent.backup?.split('/').pop() ?? '—'}`}
                    />
                  ) : (
                    <T
                      zh={`写入后 ${adds} 行新增 · 原文件先备份再原子替换`}
                      en={`${adds} lines will be added · original backed up first`}
                    />
                  )}
                </span>
                <span className="sub">
                  {agent.appliedAt ? new Date(agent.appliedAt).toLocaleString() : ''}
                </span>
              </div>
              {agent.projectPath && <ProjectScope agent={agent} />}
            </>
          )}
        </div>

        <div className="modal-actions">
          {writable ? (
            agent.state === 'injected' ? (
              <button className="btn danger" onClick={onRevert} disabled={busy}>
                <Undo2 size={12} />
                <T zh="回滚" en="Rollback" />
              </button>
            ) : (
              <button className="btn primary" onClick={onApply} disabled={busy}>
                <Wand2 size={12} />
                <T zh="注入" en="Inject" />
              </button>
            )
          ) : agent.state === 'missing' ? (
            <button className="btn primary" onClick={() => void onRescan()} disabled={busy}>
              <RefreshCw size={12} />
              <T zh="我已装好，重新扫描" en="Installed — rescan" />
            </button>
          ) : null}
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            <T zh="关闭" en="Close" />
          </button>
        </div>
      </div>
    </div>
  );
}

/* ── 项目级：只对「客户端自己认的个人文件」开放 ────────── */
function ProjectScope({ agent }: { agent: AgentInfo }) {
  const api = window.freeport?.gateway;
  const [repo, setRepo] = useState(() => localStorage.getItem('freeport.repo') ?? '');
  const [repoList, setRepoList] = useState<ProjectInjection[] | null>(null);
  const [pdiff, setPdiff] = useState<AgentDiffLine[] | null>(null);
  const [repoErr, setRepoErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const repoRoot = repo.trim();
  const repoReady = repoRoot !== '' && repoErr === null;
  const inRepo = !!repoList?.some((r) => r.id === agent.id);

  /** 载入一个仓库：列出这里已注入的 Agent，并算出当前 Agent 的 diff */
  const loadRepo = useCallback(
    async (root: string) => {
      if (!api || !root.trim()) return;
      setRepoErr(null);
      try {
        const list = await api.agentProjectList(root);
        setRepoList(list);
        localStorage.setItem('freeport.repo', root.trim());
      } catch (e) {
        // 路径写错是最常见的失败，不要静默：留着错误、清掉列表
        setRepoErr(String(e));
        setRepoList(null);
      }
    },
    [api],
  );

  // 打开弹窗时恢复上次用的仓库：不然「已接入」的标记要等用户再点一次才准
  useEffect(() => {
    if (repo.trim()) void loadRepo(repo);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 换 Agent 或换仓库就重算 diff（读的是磁盘真实内容，不缓存）
  useEffect(() => {
    if (!api || !repoRoot || repoErr) {
      setPdiff(null);
      return;
    }
    let alive = true;
    setPdiff(null);
    void (async () => {
      try {
        const d = await api.agentProjectPreview(agent.id, repoRoot);
        if (alive) setPdiff(d);
      } catch (e) {
        if (alive) setRepoErr(String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [api, agent.id, repoRoot, repoErr]);

  async function applyProject() {
    if (!api) return;
    setBusy(true);
    try {
      await api.agentProjectApply(agent.id, repoRoot);
      toast(`已写进仓库 ${repoRoot.split('/').pop() ?? ''} / Injected into repo`);
      await loadRepo(repoRoot);
      setPdiff(await api.agentProjectPreview(agent.id, repoRoot).catch(() => null));
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function revertProject() {
    if (!api) return;
    setBusy(true);
    try {
      await api.agentProjectRevert(agent.id, repoRoot);
      toast('已把这个仓库撤出 / Removed from repo');
      await loadRepo(repoRoot);
      setPdiff(null);
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="project-scope">
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <FolderGit2 size={13} />
        <b style={{ fontSize: 12.5 }}>
          <T zh="项目级" en="Project scope" />
        </b>
        <span className="sub" style={{ fontSize: 11.5 }}>
          {agent.projectPath}
        </span>
        <span style={{ marginLeft: 'auto' }}>
          {inRepo ? (
            <Badge tone="ok">
              <T zh="已接入这个仓库" en="in this repo" />
            </Badge>
          ) : repoReady ? (
            <Badge tone="info">
              <T zh="未接入" en="not yet" />
            </Badge>
          ) : null}
        </span>
      </div>
      <div className="repo-row">
        <input
          placeholder="/path/to/repo"
          value={repo}
          onChange={(e) => setRepo(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') void loadRepo(repo);
          }}
          spellCheck={false}
        />
        <button
          className="btn ghost sm"
          onClick={() => void loadRepo(repo)}
          disabled={busy || !repo.trim()}
        >
          <T zh="载入" en="Load" />
        </button>
        {inRepo ? (
          <button className="btn danger sm" onClick={() => void revertProject()} disabled={busy}>
            <Undo2 size={12} />
            <T zh="撤出" en="Remove" />
          </button>
        ) : (
          <button
            className="btn primary sm"
            onClick={() => void applyProject()}
            disabled={busy || !repoReady}
          >
            <Wand2 size={12} />
            <T zh="写进仓库" en="Write" />
          </button>
        )}
      </div>
      {repoErr && <div style={{ color: 'var(--bad)', fontSize: 11.5, marginTop: 8 }}>{repoErr}</div>}
      {repoReady && !repoErr && (
        <div className="diff" style={{ marginTop: 10, maxHeight: 150, overflow: 'auto' }}>
          {pdiff === null ? (
            <div className="ln ctx">
              <span className="gutter">·</span>
              <span>
                <T zh="读取仓库文件…" en="Reading repo file…" />
              </span>
            </div>
          ) : pdiff.length === 0 ? (
            <div className="ln ctx">
              <span className="gutter">·</span>
              <span>
                <T zh="仓库里已是目标状态。" en="Already up to date in this repo." />
              </span>
            </div>
          ) : (
            pdiff.map((l, i) => (
              <div className={`ln ${l.kind}`} key={i}>
                <span className="gutter">{l.no}</span>
                <span>
                  {l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '} {l.text}
                </span>
              </div>
            ))
          )}
        </div>
      )}
      <div className="sub" style={{ fontSize: 11, marginTop: 8, lineHeight: 1.7 }}>
        <T
          zh="只写客户端自己认定为「个人本地」的文件（默认不进 git）；会被提交的文件一律不代写，免得 key 跟着进仓库。"
          en="Only files the client itself treats as local and uncommitted are written; anything that gets committed is left alone so the key never lands in git."
        />
      </div>
      {repoReady && repoList && repoList.length > 0 && (
        <div style={{ marginTop: 8, fontSize: 11.5, color: 'var(--text-2)' }}>
          <T zh="这个仓库已接入：" en="Injected here:" />{' '}
          {repoList.map((r) => r.name).join('、')}
        </div>
      )}
    </div>
  );
}
