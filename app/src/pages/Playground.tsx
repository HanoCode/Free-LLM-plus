/**
 * 试验场：在本应用里直接跟网关对话，验证路由链与模型是否真的能用。
 *
 * 请求走的是本机网关的真实业务端点 `/v1/chat/completions`——与注入给
 * Agent 的路径完全一致，所以这里看到的落点、延迟、失败就是用户会遇到的。
 * 不做模拟数据：网关没起、池子是空的，这里就如实显示。
 *
 * 三种形态：
 * - 自动（仅免费）：网关按路由链挑，Gate 会拦掉付费模型；
 * - 融合（多模型对比）：同一问题并行发给链上前 N 个免费候选，回答并列展示；
 * - 指定模型：显式点名，不受免费约束。
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowUp,
  ChevronDown,
  Layers,
  PanelRightClose,
  PanelRightOpen,
  Plus,
  Search,
  Sparkles,
  Square,
} from 'lucide-react';
import { T, tx, useLang } from '../lib/i18n';
import { Empty, Switch, toast } from '../components/ui';
import { useGateway } from '../lib/gateway';

/** 融合模式并行询问的候选数。取链上前几个免费模型，太多了等得久、烧得快。 */
const FUSION_K = 3;

type Mode = 'auto' | 'fusion' | 'model';

/** 一条回答。融合模式下一次对话会产生多条并列。 */
interface Answer {
  /** 发请求用的模型 id（`auto` 或 `provider:model`） */
  target: string;
  content: string;
  /** 实际落点（响应里的 model 字段） */
  landed?: string;
  ms?: number;
  failed?: boolean;
  streaming?: boolean;
}

type Item =
  | { kind: 'user'; content: string }
  | { kind: 'bot'; answers: Answer[] };

interface ChatChunk {
  model?: string;
  error?: { message?: string };
  choices?: { delta?: { content?: string | null } }[];
}

function newSession() {
  return crypto.randomUUID().replace(/-/g, '').slice(0, 24);
}

/** 把已发生的对话整理成请求历史。融合轮取第一个成功回答进入历史。 */
function historyOf(items: Item[]): { role: string; content: string }[] {
  const out: { role: string; content: string }[] = [];
  for (const it of items) {
    if (it.kind === 'user') {
      out.push({ role: 'user', content: it.content });
    } else {
      const ok = it.answers.find((a) => !a.failed && a.content);
      if (ok) out.push({ role: 'assistant', content: ok.content });
    }
  }
  return out;
}

/** 融合模式的目标：复用网关真实算出来的候选链，取前 N 个免费 hop。 */
async function fusionTargets(session: string, lang: 'zh' | 'en'): Promise<string[]> {
  const gw = useGateway.getState();
  const route = await gw.explainRoute('auto', session, 'chat');
  const seen = new Set<string>();
  const out: string[] = [];
  for (const hop of route?.chain ?? []) {
    const key = `${hop.provider}:${hop.model}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(key);
    if (out.length >= FUSION_K) break;
  }
  if (out.length === 0) {
    toast(
      tx(
        lang,
        '融合模式需要至少一个免费候选，当前链是空的',
        'Fusion needs at least one free candidate; the chain is empty',
      ),
    );
  }
  return out;
}

/* ── 模型选择器：搜索 + 形态置顶 + 按厂商分组 ── */
function ModelPicker({
  mode,
  setMode,
  model,
  setModel,
  options,
  lang,
}: {
  mode: Mode;
  setMode: (m: Mode) => void;
  model: string;
  setModel: (m: string) => void;
  options: { id: string }[];
  lang: 'zh' | 'en';
}) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', close);
    return () => document.removeEventListener('mousedown', close);
  }, [open]);

  const label =
    mode === 'auto'
      ? tx(lang, '自动（仅免费）', 'Auto (free only)')
      : mode === 'fusion'
        ? tx(lang, `融合（${FUSION_K} 模型对比）`, `Fusion (${FUSION_K}-model compare)`)
        : model;

  const groups = useMemo(() => {
    const kw = q.trim().toLowerCase();
    const hit = options.filter((o) => !kw || o.id.toLowerCase().includes(kw));
    const map = new Map<string, string[]>();
    for (const o of hit) {
      const pid = o.id.includes(':') ? o.id.split(':')[0] : '·';
      if (!map.has(pid)) map.set(pid, []);
      map.get(pid)!.push(o.id);
    }
    return [...map.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }, [options, q]);

  const pick = (m: Mode, id?: string) => {
    setMode(m);
    if (id != null) setModel(id);
    setOpen(false);
    setQ('');
  };

  return (
    <div className="pg-dd" ref={ref}>
      <button className="select pg-dd-btn" onClick={() => setOpen(!open)}>
        <span className="pg-dd-label">{label}</span>
        <ChevronDown />
      </button>
      {open && (
        <div className="pg-dd-pop">
          <div className="pg-dd-search">
            <Search />
            <input
              autoFocus
              placeholder={tx(lang, '搜索模型…', 'Search models…')}
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
          </div>
          <button
            className={`pg-dd-opt ${mode === 'auto' ? 'on' : ''}`}
            onClick={() => pick('auto')}
          >
            {mode === 'auto' && <span className="pg-dd-check">✓</span>}
            <Sparkles />
            <T zh="自动（仅免费）" en="Auto (free only)" />
          </button>
          <button
            className={`pg-dd-opt ${mode === 'fusion' ? 'on' : ''}`}
            onClick={() => pick('fusion')}
          >
            {mode === 'fusion' && <span className="pg-dd-check">✓</span>}
            <Layers />
            <T zh={`融合（${FUSION_K} 模型对比）`} en={`Fusion (${FUSION_K}-model compare)`} />
          </button>
          {groups.length === 0 ? (
            <div className="pg-dd-none">
              {options.length === 0
                ? tx(lang, '暂无模型。请先在「厂商」页接入并探测出模型。', 'No models yet. Connect a provider first.')
                : tx(lang, '没有匹配的模型', 'No matching models')}
            </div>
          ) : (
            groups.map(([pid, ids]) => (
              <div key={pid}>
                <div className="pg-dd-group">{pid}</div>
                {ids.map((id) => (
                  <button
                    key={id}
                    className={`pg-dd-opt ${mode === 'model' && model === id ? 'on' : ''}`}
                    onClick={() => pick('model', id)}
                  >
                    {mode === 'model' && model === id && (
                      <span className="pg-dd-check">✓</span>
                    )}
                    <span className="pg-dd-id">{id}</span>
                  </button>
                ))}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}

export function Playground() {
  const lang = useLang();
  const available = useGateway((s) => s.available);
  const status = useGateway((s) => s.status);
  const models = useGateway((s) => s.models);

  const [items, setItems] = useState<Item[]>([]);
  const [input, setInput] = useState('');
  const [streaming, setStreaming] = useState(false);
  const [showPanel, setShowPanel] = useState(true);
  const [session, setSession] = useState(newSession);

  const [mode, setMode] = useState<Mode>('auto');
  const [model, setModel] = useState('');
  const [system, setSystem] = useState('');
  const [useTemp, setUseTemp] = useState(false);
  const [temp, setTemp] = useState(0.7);
  const [useTopP, setUseTopP] = useState(false);
  const [topP, setTopP] = useState(1);
  const [useMax, setUseMax] = useState(false);
  const [maxTokens, setMaxTokens] = useState(1024);

  const abortRef = useRef<AbortController[]>([]);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // 候选模型：各厂商的带前缀真实模型（provider:model）
  const options = useMemo(() => {
    const seen = new Set<string>();
    const rows: { id: string }[] = [];
    for (const m of models) {
      if (!m.freeport?.upstreamModel) continue;
      if (seen.has(m.id)) continue;
      seen.add(m.id);
      rows.push({ id: m.id });
    }
    rows.sort((a, b) => a.id.localeCompare(b.id));
    return rows;
  }, [models]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items]);

  const running = available && !!status?.running && !status?.paused;
  const busy = streaming;

  function reset() {
    if (busy) return;
    setItems([]);
    setSession(newSession());
    inputRef.current?.focus();
  }

  /** 就地更新某条回答的片段 */
  function patchAnswer(itemIdx: number, answerIdx: number, patch: Partial<Answer>) {
    setItems((list) =>
      list.map((it, i) => {
        if (i !== itemIdx || it.kind !== 'bot') return it;
        return {
          ...it,
          answers: it.answers.map((a, j) => (j === answerIdx ? { ...a, ...patch } : a)),
        };
      }),
    );
  }

  async function streamOnce(
    target: string,
    payload: Record<string, unknown>,
    headers: Record<string, string>,
    port: number,
    ac: AbortController,
    onDelta: (text: string) => void,
    onLanded: (model: string) => void,
  ): Promise<{ ms: number; failed: boolean; error?: string }> {
    const started = performance.now();
    try {
      const res = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
        method: 'POST',
        headers,
        body: JSON.stringify({ ...payload, model: target }),
        signal: ac.signal,
      });

      if (!res.ok || !res.body) {
        const detail = await res.text().catch(() => '');
        let msg = `HTTP ${res.status}`;
        try {
          const j = JSON.parse(detail);
          msg = j?.error?.message ?? j?.message ?? msg;
        } catch {
          if (detail) msg = detail.slice(0, 400);
        }
        return { ms: Math.round(performance.now() - started), failed: true, error: msg };
      }

      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = '';
      let acc = '';
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const parts = buf.split('\n\n');
        buf = parts.pop() ?? '';
        for (const part of parts) {
          const line = part.split('\n').find((l) => l.startsWith('data:'));
          if (!line) continue;
          const data = line.slice(5).trim();
          if (!data || data === '[DONE]') continue;
          try {
            const chunk = JSON.parse(data) as ChatChunk;
            if (chunk.model) onLanded(chunk.model);
            if (chunk.error?.message) throw new Error(chunk.error.message);
            const delta = chunk.choices?.[0]?.delta?.content;
            if (delta) {
              acc += delta;
              onDelta(acc);
            }
          } catch (e) {
            if (e instanceof SyntaxError) continue;
            throw e;
          }
        }
      }

      const failed = !acc;
      return {
        ms: Math.round(performance.now() - started),
        failed,
        error: failed
          ? tx(lang, '（模型没有返回文本内容）', '(model returned no text content)')
          : undefined,
      };
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        // 用户主动停止：已生成的部分已经通过 onDelta 写进去了，原样保留
        return { ms: Math.round(performance.now() - started), failed: false };
      }
      return {
        ms: Math.round(performance.now() - started),
        failed: true,
        error: e instanceof Error ? e.message : String(e),
      };
    }
  }

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    if (!running || !status?.port) {
      toast(tx(lang, '网关未运行，无法发送', 'Gateway is not running'));
      return;
    }
    const port = status.port;

    // 融合模式先算目标：链是空的就别把用户的话吞掉
    let targets: string[];
    if (mode === 'fusion') {
      try {
        targets = await fusionTargets(session, lang);
      } catch {
        targets = [];
      }
      if (targets.length === 0) return;
    } else {
      targets = [mode === 'auto' ? 'auto' : model];
    }

    const userItem: Item = { kind: 'user', content: text };
    const botItem: Item = {
      kind: 'bot',
      answers: targets.map((t) => ({ target: t, content: '', streaming: true })),
    };
    const botIdx = items.length + 1;
    setItems([...items, userItem, botItem]);
    setInput('');
    setStreaming(true);

    const payload: Record<string, unknown> = {
      stream: true,
      messages: [
        ...(system.trim() ? [{ role: 'system', content: system.trim() }] : []),
        ...historyOf([...items, userItem]),
      ],
    };
    if (useTemp) payload.temperature = temp;
    if (useTopP) payload.top_p = topP;
    if (useMax) payload.max_tokens = maxTokens;

    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      // 会话粘性：同一轮对话尽量落在同一条凭据上，与 Agent 注入的行为一致
      'x-freeport-session': session,
    };
    if (status.requireAuth && status.authToken) {
      headers.Authorization = `Bearer ${status.authToken}`;
    }

    const acs = targets.map(() => new AbortController());
    abortRef.current = acs;

    await Promise.all(
      targets.map((target, i) =>
        streamOnce(
          target,
          payload,
          headers,
          port,
          acs[i],
          (content) => patchAnswer(botIdx, i, { content }),
          (landed) => patchAnswer(botIdx, i, { landed }),
        ).then((r) => {
          // 注意：Partial 里的 undefined 也会覆盖，错误文本只在有值时才写
          const patch: Partial<Answer> = { streaming: false, ms: r.ms, failed: r.failed };
          if (r.error) patch.content = r.error;
          patchAnswer(botIdx, i, patch);
        }),
      ),
    );

    setStreaming(false);
    abortRef.current = [];
  }

  function stop() {
    for (const ac of abortRef.current) ac.abort();
  }

  const stopAt = (i: number) => {
    abortRef.current[i]?.abort();
  };

  const modelLabel =
    mode === 'auto'
      ? tx(lang, '自动（仅免费）', 'Auto (free only)')
      : mode === 'fusion'
        ? tx(lang, `融合（${FUSION_K} 模型对比）`, `Fusion (${FUSION_K}-model compare)`)
        : model;

  return (
    <div className="pg">
      <div className="pg-head">
        <button className="btn sm" onClick={reset} disabled={busy}>
          <Plus />
          <T zh="新对话" en="New chat" />
        </button>
        <span className="pg-landed mono">
          {streaming && mode === 'fusion' ? (
            <T zh={`融合中 · ${FUSION_K} 路并行…`} en={`fusing · ${FUSION_K} lanes…`} />
          ) : (
            ''
          )}
        </span>
        <div className="spacer" />
        <button
          className="btn sm ghost"
          onClick={() => setShowPanel(!showPanel)}
          title={tx(lang, '参数面板', 'Settings panel')}
        >
          {showPanel ? <PanelRightClose /> : <PanelRightOpen />}
          <T zh={showPanel ? '收起设置' : '展开设置'} en={showPanel ? 'Hide panel' : 'Show panel'} />
        </button>
      </div>

      <div className="pg-body">
        <div className="pg-chat card">
          <div className="pg-msgs" ref={scrollRef}>
            {items.length === 0 ? (
              <div className="pg-empty">
                <Empty
                  title={<T zh="发送消息即可开始。" en="Send a message to start." />}
                  hint={
                    <T
                      zh={`正在使用 ${modelLabel}。可在右侧选择器中切换形态或模型。`}
                      en={`Using ${modelLabel}. Switch mode or model in the panel on the right.`}
                    />
                  }
                />
              </div>
            ) : (
              items.map((it, i) =>
                it.kind === 'user' ? (
                  <div className="pg-row user" key={i}>
                    <div className="pg-bubble user">{it.content}</div>
                  </div>
                ) : (
                  <div className={`pg-row bot ${it.answers.length > 1 ? 'multi' : ''}`} key={i}>
                    <div
                      className="pg-answers"
                      style={
                        it.answers.length > 1
                          ? { gridTemplateColumns: `repeat(${it.answers.length}, 1fr)` }
                          : undefined
                      }
                    >
                      {it.answers.map((a, j) => (
                        <div className="pg-answer" key={j}>
                          {it.answers.length > 1 && (
                            <div className="pg-answer-head mono" title={a.landed ?? a.target}>
                              <span className="pg-answer-name">{a.landed ?? a.target}</span>
                              {a.streaming && (
                                <button
                                  className="pg-stop-mini"
                                  title={tx(lang, '停止这一路', 'Stop this lane')}
                                  onClick={() => stopAt(j)}
                                >
                                  <Square />
                                </button>
                              )}
                            </div>
                          )}
                          <div className={`pg-bubble bot ${a.failed ? 'failed' : ''}`}>
                            {a.content ||
                              (a.streaming ? (
                                <span className="pg-typing">{tx(lang, '思考中…', 'thinking…')}</span>
                              ) : (
                                ''
                              ))}
                            {a.streaming && a.content && <span className="pg-caret" />}
                          </div>
                          {!a.streaming && !a.failed && (a.landed || a.ms != null) && (
                            <div className="pg-meta mono">
                              {[a.landed, a.ms != null ? `${a.ms}ms` : null].filter(Boolean).join(' · ')}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                ),
              )
            )}
          </div>

          <div className="pg-composer">
            <textarea
              ref={inputRef}
              className="pg-input"
              placeholder={tx(
                lang,
                '输入消息…（回车发送，Shift+回车换行）',
                'Type a message… (Enter to send, Shift+Enter for newline)',
              )}
              value={input}
              rows={1}
              disabled={!running}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  void send();
                }
              }}
            />
            {streaming ? (
              <button className="pg-send stop" title={tx(lang, '停止生成', 'Stop')} onClick={stop}>
                <Square />
              </button>
            ) : (
              <button
                className="pg-send"
                title={tx(lang, '发送', 'Send')}
                disabled={!input.trim() || !running}
                onClick={() => void send()}
              >
                <ArrowUp />
              </button>
            )}
          </div>
        </div>

        {showPanel && (
          <aside className="pg-panel">
            <div className="pg-sec">
              <div className="pg-sec-t">
                <T zh="模型" en="Model" />
              </div>
              <ModelPicker
                mode={mode}
                setMode={setMode}
                model={model}
                setModel={setModel}
                options={options}
                lang={lang}
              />
              {mode === 'fusion' && (
                <p className="pg-hint">
                  <T
                    zh={`同一问题并行发给链上前 ${FUSION_K} 个免费候选，回答并列对比。每一路单独计费、单独进日志。`}
                    en={`The same prompt goes to the top ${FUSION_K} free candidates in parallel; answers are shown side by side. Each lane is logged and metered separately.`}
                  />
                </p>
              )}
            </div>

            <div className="pg-sec">
              <div className="pg-sec-t">
                <T zh="系统提示词" en="System prompt" />
              </div>
              <textarea
                className="pg-sys"
                placeholder={tx(
                  lang,
                  '可选。作为系统消息在您的对话之前发送。',
                  'Optional. Sent as a system message before your conversation.',
                )}
                value={system}
                rows={4}
                onChange={(e) => setSystem(e.target.value)}
              />
            </div>

            <div className="pg-sec">
              <div className="pg-sec-t">
                <T zh="采样" en="Sampling" />
              </div>
              <p className="pg-hint">
                <T
                  zh="关闭的开关将采用提供商自身的默认值，只有您开启的选项才会发送。"
                  en="Disabled switches use the provider's own defaults; only enabled options are sent."
                />
              </p>

              <div className="pg-param">
                <div className="pg-param-head">
                  <b>
                    <T zh="温度" en="Temperature" />
                  </b>
                  <span className="pg-param-val mono">{useTemp ? temp.toFixed(1) : ''}</span>
                  <Switch on={useTemp} onChange={setUseTemp} label={tx(lang, '默认', 'Default')} />
                </div>
                <input
                  type="range"
                  min={0}
                  max={2}
                  step={0.1}
                  value={temp}
                  disabled={!useTemp}
                  onChange={(e) => setTemp(Number(e.target.value))}
                />
              </div>

              <div className="pg-param">
                <div className="pg-param-head">
                  <b>Top P</b>
                  <span className="pg-param-val mono">{useTopP ? topP.toFixed(2) : ''}</span>
                  <Switch on={useTopP} onChange={setUseTopP} label={tx(lang, '默认', 'Default')} />
                </div>
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.05}
                  value={topP}
                  disabled={!useTopP}
                  onChange={(e) => setTopP(Number(e.target.value))}
                />
              </div>

              <div className="pg-param">
                <div className="pg-param-head">
                  <b>
                    <T zh="最大词元数" en="Max tokens" />
                  </b>
                  <span className="spacer" />
                  <Switch on={useMax} onChange={setUseMax} label={tx(lang, '默认', 'Default')} />
                </div>
                <input
                  type="number"
                  className="pg-num mono"
                  min={1}
                  value={maxTokens}
                  disabled={!useMax}
                  onChange={(e) => setMaxTokens(Math.max(1, Number(e.target.value) || 1))}
                />
              </div>
            </div>
          </aside>
        )}
      </div>
    </div>
  );
}
