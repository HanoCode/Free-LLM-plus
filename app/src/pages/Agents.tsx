import { useState } from 'react';
import { FileCode2, ShieldCheck, Undo2, Wand2 } from 'lucide-react';
import { T } from '../lib/i18n';
import { agents } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';

export function Agents() {
  const [sel, setSel] = useState(agents[0].id);
  const agent = agents.find((a) => a.id === sel)!;
  const injected = agents.filter((a) => a.status === 'injected').length;

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
          <Badge tone="ok">
            <T zh={`${injected} 个已接管`} en={`${injected} injected`} />
          </Badge>
          <button className="btn ghost" onClick={() => toast('已重新扫描本机已安装的 Agent / Rescanned')}>
            <Wand2 />
            <T zh="重新扫描" en="Rescan" />
          </button>
        </div>
      </div>

      <div className="agents-grid">
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="检测到的 Agent" en="Detected agents" />
            </h3>
            <span className="sub">{agents.length}</span>
          </div>
          {agents.map((a) => (
            <button
              key={a.id}
              className={`agent-item ${sel === a.id ? 'on' : ''}`}
              onClick={() => setSel(a.id)}
            >
              <span className="ic">{a.short}</span>
              <span style={{ minWidth: 0 }}>
                <div className="nm">{a.name}</div>
                <div className="p">{a.path.split('/').pop()}</div>
              </span>
              <span className="st">
                {a.status === 'injected' && <Status tone="ok"><T zh="已注入" en="on" /></Status>}
                {a.status === 'detected' && <Status tone="idle"><T zh="未注入" en="off" /></Status>}
                {a.status === 'guide' && <Badge tone="gray"><T zh="手动" en="manual" /></Badge>}
              </span>
            </button>
          ))}
        </div>

        <div className="card">
          <div className="card-head">
            <h3>{agent.name}</h3>
            <span className="sub">
              {agent.protocol === 'anthropic' ? 'Anthropic' : agent.protocol === 'gemini' ? 'Gemini' : 'OpenAI'} ·{' '}
              {agent.scope}
            </span>
            {agent.status === 'injected' ? (
              <button
                className="btn danger sm"
                style={{ marginLeft: 8 }}
                onClick={() => toast(`已回滚 ${agent.name}，文件恢复到注入前的哈希 / Rolled back`)}
              >
                <Undo2 size={12} />
                <T zh="回滚" en="Rollback" />
              </button>
            ) : (
              agent.status === 'detected' && (
                <button
                  className="btn primary sm"
                  style={{ marginLeft: 8 }}
                  onClick={() => toast(`已注入 ${agent.name}，备份 bak-freeport-20261019 / Injected`)}
                >
                  <Wand2 size={12} />
                  <T zh="注入" en="Inject" />
                </button>
              )
            )}
          </div>

          <div className="filebar">
            <FileCode2 />
            {agent.path}
          </div>

          {agent.status === 'guide' ? (
            <div className="card-body" style={{ fontSize: 12.5, lineHeight: 1.9, color: 'var(--text-2)' }}>
              <div className="risk-note" style={{ marginBottom: 12 }}>
                <ShieldCheck />
                <p>
                  <T
                    zh={`${agent.name} 的配置存在应用私有存储里，外部写入会被它覆盖，也可能损坏。Freeport 只给步骤，不代写。`}
                    en={`${agent.name} keeps settings in private app storage; external writes get overwritten or corrupted. Freeport only gives you steps.`}
                  />
                </p>
              </div>
              <div>
                <b className="mono">1.</b>{' '}
                <T zh="打开设置 → Models / Providers" en="Open Settings → Models / Providers" />
              </div>
              <div>
                <b className="mono">2.</b> Base URL <span className="kbd">http://127.0.0.1:8787/v1</span>
              </div>
              <div>
                <b className="mono">3.</b> API Key <span className="kbd">fp_local_7k2x…</span>{' '}
                <button className="copy-mini" onClick={() => toast('已复制 / Copied')}>
                  <T zh="复制" en="copy" />
                </button>
              </div>
              <div>
                <b className="mono">4.</b>{' '}
                <T zh="模型名填 freeport/auto，保存后发一条消息验证" en="Model: freeport/auto. Send one message." />
              </div>
            </div>
          ) : (
            <>
              <div className="diff" style={{ padding: '8px 0' }}>
                {agent.diff.map((l, i) => (
                  <div className={`ln ${l.kind}`} key={i}>
                    <span className="gutter">{l.no}</span>
                    <span>
                      {l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '} {l.text}
                      {l.managed && <span className="managed-tag">freeport</span>}
                    </span>
                  </div>
                ))}
              </div>
              <div className="card-head" style={{ borderTop: '1px solid var(--line)', borderBottom: 'none' }}>
                <span className="sub" style={{ marginLeft: 0 }}>
                  <T
                    zh={`${agent.diff.filter((d) => d.kind === 'add').length} 行新增 · 0 行删除 · 备份 bak-freeport-20261019-1142`}
                    en={`${agent.diff.filter((d) => d.kind === 'add').length} added · 0 removed · backup bak-freeport-20261019-1142`}
                  />
                </span>
                <button
                  className="btn ghost sm"
                  style={{ marginLeft: 'auto' }}
                  onClick={() => toast('打开备份目录 / Open backups')}
                >
                  <T zh="查看备份" en="Backups" />
                </button>
              </div>
            </>
          )}
        </div>
      </div>

      <div className="hint-line">
        <T
          zh="注入五件套：托管块标记、写入前 diff、原子写 + 文件锁、幂等（重复注入不重复加行）、退出 / 卸载时清理。"
          en="Five rules: managed markers, diff before write, atomic write with lock, idempotent, cleanup on exit."
        />
      </div>
    </>
  );
}
