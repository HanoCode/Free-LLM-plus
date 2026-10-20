import { useCallback, useEffect, useState } from 'react';
import {
  AlertTriangle,
  FileCode2,
  FolderGit2,
  RefreshCw,
  ShieldCheck,
  Undo2,
  Wand2,
} from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, Status, toast } from '../components/ui';
import type { AgentDiffLine, AgentInfo, ProjectInjection } from '../lib/bridge';

/** 两个字母的方块图标，避免为每个 Agent 单独引入品牌图。 */
function short(name: string): string {
  const parts = name.split(/[\s-]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

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

export function Agents() {
  const api = window.freeport?.gateway;

  const [list, setList] = useState<AgentInfo[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  const [diff, setDiff] = useState<AgentDiffLine[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [port, setPort] = useState<number | null>(null);
  const [prefix, setPrefix] = useState<string>('fp-preview');

  const agent = list.find((a) => a.id === sel) ?? list[0] ?? null;

  const load = useCallback(async () => {
    if (!api) return;
    try {
      const next = await api.agents();
      setList(next);
      setErr(null);
      setSel((cur) => cur ?? next[0]?.id ?? null);
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

  // 选中项变化就重取 diff：读的是磁盘真实内容，不是缓存
  useEffect(() => {
    if (!api || !agent) return;
    setDiff(null);
    if (agent.strategy === 'guide' || agent.state === 'missing' || agent.state === 'unsupported') return;
    let alive = true;
    void (async () => {
      try {
        const d = await api.agentPreview(agent.id);
        if (alive) setDiff(d);
      } catch (e) {
        if (alive) toast(String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [api, agent?.id, agent?.state, agent?.strategy]);

  async function apply() {
    if (!api || !agent) return;
    setBusy(true);
    try {
      const info = await api.agentApply(agent.id);
      toast(
        `已注入 ${info.name}${info.backup ? `，备份 ${info.backup.split('/').pop()}` : ''} / Injected`,
      );
      await load();
      setDiff(await api.agentPreview(agent.id));
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function revert() {
    if (!api || !agent) return;
    setBusy(true);
    try {
      const info = await api.agentRevert(agent.id);
      toast(`已回滚 ${info.name}，文件恢复原样 / Rolled back`);
      await load();
      setDiff(agent.path ? await api.agentPreview(agent.id).catch(() => null) : null);
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  /* ── 项目级 ── */
  const [repo, setRepo] = useState(() => localStorage.getItem('freeport.repo') ?? '');
  const [repoList, setRepoList] = useState<ProjectInjection[] | null>(null);
  const [pdiff, setPdiff] = useState<AgentDiffLine[] | null>(null);
  const [repoErr, setRepoErr] = useState<string | null>(null);
  const repoRoot = repo.trim();
  const repoReady = repoRoot !== '' && repoErr === null;
  const inRepo = !!agent && !!repoList?.some((r) => r.id === agent.id);

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

  // 挂载时恢复上次用的仓库：不然「已接入」的标记要等用户再点一次才准
  useEffect(() => {
    if (repo.trim()) void loadRepo(repo);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 换 Agent 或换仓库就重算 diff（读的是磁盘真实内容，不缓存）
  useEffect(() => {
    if (!api || !agent?.projectPath || !repoRoot || repoErr) {
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
  }, [api, agent?.id, agent?.projectPath, repoRoot, repoErr]);

  async function applyProject() {
    if (!api || !agent) return;
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
    if (!api || !agent) return;
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

  const injected = list.filter((a) => a.state === 'injected').length;
  const base = port ? `http://127.0.0.1:${port}` : 'http://127.0.0.1:8787';
  const adds = diff?.filter((d) => d.kind === 'add').length ?? 0;
  const dels = diff?.filter((d) => d.kind === 'del').length ?? 0;

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="注入" en="Agents" />
          </h1>
          <p>
            <T
              zh="只改自己写的那几行，写入前给你看逐行 diff，任何时候都能一键恢复原样。"
              en="Only our own lines get written. You see the diff first, and one click restores the original."
            />
          </p>
        </div>
        <div className="head-actions">
          <Badge tone={injected > 0 ? 'ok' : 'gray'}>
            <T zh={`${injected} 个已接管`} en={`${injected} injected`} />
          </Badge>
          <button className="btn ghost" onClick={() => void load()} disabled={busy}>
            <RefreshCw />
            <T zh="重新扫描" en="Rescan" />
          </button>
        </div>
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
        <div className="agents-grid">
          <div className="card">
            <div className="card-head">
              <h3>
                <T zh="检测到的 Agent" en="Detected agents" />
              </h3>
              <span className="sub">{list.length}</span>
            </div>
            {list.map((a) => (
              <button
                key={a.id}
                className={`agent-item ${agent?.id === a.id ? 'on' : ''}`}
                onClick={() => setSel(a.id)}
              >
                <span className="ic">{short(a.name)}</span>
                <span style={{ minWidth: 0 }}>
                  <div className="nm">{a.name}</div>
                  <div className="p">{a.path?.split('/').pop() ?? a.note.slice(0, 18)}</div>
                </span>
                <span className="st">
                  <StateTag state={a.state} />
                </span>
              </button>
            ))}
          </div>

          {agent && (
            <div className="card">
              <div className="card-head">
                <h3>{agent.name}</h3>
                <span className="sub">
                  {agent.protocol === 'anthropic'
                    ? 'Anthropic'
                    : agent.protocol === 'gemini'
                      ? 'Gemini'
                      : 'OpenAI'}{' '}
                  · <Mode kind={agent.discovery} />
                </span>
                {agent.state === 'injected' ? (
                  <button
                    className="btn danger sm"
                    style={{ marginLeft: 8 }}
                    onClick={() => void revert()}
                    disabled={busy}
                  >
                    <Undo2 size={12} />
                    <T zh="回滚" en="Rollback" />
                  </button>
                ) : agent.state === 'detected' ? (
                  <button
                    className="btn primary sm"
                    style={{ marginLeft: 8 }}
                    onClick={() => void apply()}
                    disabled={busy}
                  >
                    <Wand2 size={12} />
                    <T zh="注入" en="Inject" />
                  </button>
                ) : null}
              </div>

              <div className="filebar">
                <FileCode2 />
                {agent.path ?? agent.note}
              </div>

              {agent.state === 'guide' ? (
                <div className="card-body" style={{ fontSize: 12.5, lineHeight: 1.9, color: 'var(--text-2)' }}>
                  <div className="risk-note" style={{ marginBottom: 12 }}>
                    <ShieldCheck />
                    <p>
                      <T
                        zh={`${agent.name}：${agent.note}。Freeport 只给步骤，不代写。`}
                        en={`${agent.name}: ${agent.note}. Freeport only gives steps.`}
                      />
                    </p>
                  </div>
                  <div>
                    <b className="mono">1.</b>{' '}
                    <T zh="打开设置 → Models / Providers" en="Open Settings → Models / Providers" />
                  </div>
                  <div>
                    <b className="mono">2.</b> Base URL{' '}
                    <span className="kbd">
                      {base}
                      {agent.protocol === 'anthropic'
                        ? ''
                        : agent.protocol === 'gemini'
                          ? '/v1beta'
                          : '/v1'}
                    </span>
                  </div>
                  <div>
                    <b className="mono">3.</b> API Key <span className="kbd">{prefix}…</span>{' '}
                    <T zh="（在设置页新建一把）" en="(create one in Settings)" />
                  </div>
                  <div>
                    <b className="mono">4.</b>{' '}
                    <T zh="模型名填 freeport/auto，保存后发一条消息验证" en="Model: freeport/auto. Send one message." />
                  </div>
                </div>
              ) : agent.state === 'missing' || agent.state === 'unsupported' ? (
                <div className="card-body" style={{ fontSize: 12.5, lineHeight: 1.9, color: 'var(--text-2)' }}>
                  <div className="risk-note">
                    <AlertTriangle />
                    <p>
                      {agent.state === 'missing'
                        ? (
                            <T
                              zh={`没检测到 ${agent.name} 的安装目录，装了之后点「重新扫描」。`}
                              en={`${agent.name} is not installed. Install it, then rescan.`}
                            />
                          )
                        : (
                            <T
                              zh={`网关还没提供 ${agent.protocol} 原生协议，现在注入也连不上，所以不写文件。`}
                              en={`The gateway does not speak ${agent.protocol} natively yet, so nothing is written.`}
                            />
                          )}
                    </p>
                  </div>
                </div>
              ) : (
                <>
                  <div className="diff" style={{ padding: '8px 0' }}>
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
                  <div
                    className="card-head"
                    style={{ borderTop: '1px solid var(--line)', borderBottom: 'none' }}
                  >
                    <span className="sub" style={{ marginLeft: 0 }}>
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
                    <span className="sub" style={{ marginLeft: 'auto', fontSize: 11.5 }}>
                      {agent.appliedAt ? new Date(agent.appliedAt).toLocaleString() : ''}
                    </span>
                  </div>
                </>
              )}

              {/* 项目级：只对「客户端自己认的个人文件」开放，其余 Agent 不显示这块 */}
              {agent.projectPath && (
                <div
                  className="card-body"
                  style={{ borderTop: '1px solid var(--line)', paddingTop: 12 }}
                >
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
                      <button
                        className="btn danger sm"
                        onClick={() => void revertProject()}
                        disabled={busy}
                      >
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
                  {repoErr && (
                    <div style={{ color: 'var(--bad)', fontSize: 11.5, marginTop: 8 }}>{repoErr}</div>
                  )}
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
              )}
            </div>
          )}
        </div>
      )}

      <div className="hint-line">
        <T
          zh="注入五件套：托管块标记、写入前 diff、原子写 + 文件锁、幂等（重复注入不重复加行）、退出 / 卸载时清理。"
          en="Five rules: managed markers, diff before write, atomic write with lock, idempotent, cleanup on exit."
        />
      </div>
    </>
  );
}
