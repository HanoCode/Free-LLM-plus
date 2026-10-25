import { useCallback, useEffect, useState } from 'react';
import { Code2, Image as ImageIcon, Mic, Puzzle, RefreshCw, Square } from 'lucide-react';
import { T } from '../lib/i18n';
import { Badge, Empty, Status, Switch, toast } from '../components/ui';
import type { GwExt } from '../lib/bridge';

/**
 * 扩展页。
 *
 * v1 起这两个能力是真的：ASR 与图片生成各自跑在独立 sidecar 进程里，
 * 不进 chat 主链路。这一页展示的是 manifest 里声明过的权限与运行状态——
 * 端口号不在这里，它只存在于网关与 sidecar 之间的内部路由表。
 *
 * 常见误会（页头文案专门澄清）：chat 请求体里的 tools（function calling）
 * 由主网关原样透传，模型支持就直接用、不支持被能力门禁降权——那一路
 * **零配置**。这一页的扩展是 chat 协议之外的能力（语音转写、图片生成），
 * chat 免费厂商不提供这些端点，所以上游要单独配，与工具调用无关。
 */

function ExtIcon({ kind }: { kind: string }) {
  return (
    <div className="ext-ico">
      {kind === 'asr' ? <Mic size={15} /> : <ImageIcon size={15} />}
    </div>
  );
}

/** 单个扩展的配置表单。本地草稿只在点「保存」时落后端。 */
function ExtCard({
  ext,
  onChanged,
}: {
  ext: GwExt;
  onChanged: (list: GwExt[]) => void;
}) {
  const api = window.freeport?.gateway;
  const [baseUrl, setBaseUrl] = useState(ext.baseUrl);
  const [model, setModel] = useState(ext.model);
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);

  // 外部状态变化（保存成功返回新列表）时同步草稿
  useEffect(() => {
    setBaseUrl(ext.baseUrl);
    setModel(ext.model);
  }, [ext.baseUrl, ext.model]);

  const dirty = baseUrl !== ext.baseUrl || model !== ext.model;

  async function saveConfig(enabled: boolean) {
    if (!api) return;
    setBusy(true);
    try {
      onChanged(await api.extSetConfig(ext.id, enabled, baseUrl, model));
      toast(enabled ? '已启用' : '已停用');
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function saveKey() {
    if (!api || !key.trim()) return;
    setBusy(true);
    try {
      onChanged(await api.extSetKey(ext.id, key.trim()));
      setKey('');
      toast('凭据已保存（明文不落配置）');
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function stopProc() {
    if (!api) return;
    try {
      onChanged(await api.extStop(ext.id));
      toast('sidecar 已停止');
    } catch (e) {
      toast(String(e));
    }
  }

  return (
    <div className="card" style={{ marginBottom: 14 }}>
      <div className="card-head">
        <div className="ext-row" style={{ borderBottom: 'none', paddingBottom: 0 }}>
          <ExtIcon kind={ext.kind} />
          <div className="ext-main">
            <div className="ext-name">
              {ext.name}
              <span className="badge gray">v{ext.version}</span>
              {!ext.trusted && (
                <Badge tone="bad">
                  <T zh="验签失败" en="untrusted" />
                </Badge>
              )}
            </div>
            <div className="ext-sub">
              {ext.endpoint} · {ext.author}
            </div>
          </div>
          <div style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 10 }}>
            {ext.running && (
              <Status tone="ok">
                <T zh="运行中" en="running" />
              </Status>
            )}
            {ext.running && (
              <button className="btn ghost" onClick={() => void stopProc()} title="sidecar 独立进程，按需自动拉起">
                <Square size={11} />
                <T zh="停止" en="Stop" />
              </button>
            )}
            <Switch on={ext.enabled} onChange={(v) => void saveConfig(v)} />
          </div>
        </div>
      </div>
      <div className="card-body" style={{ fontSize: 12.5 }}>
        <div style={{ marginBottom: 10 }}>
          <span className="sub">
            <T zh="授权域名" en="Allowed domains" />
          </span>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginTop: 6 }}>
            {ext.domains.map((d) => (
              <Badge key={d} tone="info">
                {d}
              </Badge>
            ))}
          </div>
        </div>

        <div style={{ display: 'grid', gap: 8 }}>
          <label style={{ display: 'grid', gap: 4 }}>
            <span className="sub">
              <T zh="上游地址（OpenAI 兼容）" en="Upstream base URL (OpenAI-compatible)" />
            </span>
            <input
              className="input"
              placeholder="https://api.groq.com/openai/v1"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              spellCheck={false}
            />
          </label>
          <label style={{ display: 'grid', gap: 4 }}>
            <span className="sub">
              <T zh="默认模型（请求未指定时使用）" en="Default model (used when request omits it)" />
            </span>
            <input
              className="input"
              placeholder={ext.kind === 'asr' ? 'whisper-large-v3-turbo' : 'flux-schnell'}
              value={model}
              onChange={(e) => setModel(e.target.value)}
              spellCheck={false}
            />
          </label>
          {dirty && (
            <div>
              <button className="btn primary" disabled={busy} onClick={() => void saveConfig(ext.enabled)}>
                <T zh="保存配置" en="Save" />
              </button>
            </div>
          )}
          <label style={{ display: 'grid', gap: 4 }}>
            <span className="sub">
              <T zh="扩展自己的凭据" en="Extension credential" />
              {ext.hasKey ? ` · ${ext.keyMask}` : ''}
            </span>
            <div style={{ display: 'flex', gap: 8 }}>
              <input
                className="input"
                type="password"
                placeholder={ext.hasKey ? '换一把新 key' : '粘贴这个上游自己的 key（独立于 chat 厂商）'}
                value={key}
                onChange={(e) => setKey(e.target.value)}
                spellCheck={false}
              />
              <button className="btn ghost" disabled={busy || !key.trim()} onClick={() => void saveKey()}>
                <T zh="保存" en="Save" />
              </button>
            </div>
          </label>
        </div>

        <div className="hint-line" style={{ marginTop: 10 }}>
          <T
            zh="sidecar 独立进程：崩溃不带上主网关；动态端口只进内部路由表；空闲 10 分钟自动回收。"
            en="Sidecar is an isolated process: crashes never take the gateway down; its port stays internal; 10 min idle auto-recycle."
          />
        </div>
      </div>
    </div>
  );
}

export function Extensions() {
  const api = window.freeport?.gateway;
  const [list, setList] = useState<GwExt[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!api) return;
    try {
      setList(await api.extList());
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, [api]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="扩展" en="Extensions" />
          </h1>
          <p>
            <T
              zh="chat 之外的能力插件（语音转写、图片生成）。工具调用不在这里——它随主链路自动透传，零配置。"
              en="Capability plugins for things outside chat (ASR, image). Tool calling is NOT here — it passes through the main gateway, zero config."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => void load()}>
            <RefreshCw />
            <T zh="刷新" en="Refresh" />
          </button>
          <button
            className="btn ghost"
            onClick={() => toast('开发者模式还没有开放：第三方扩展的安装入口在后续版本')}
          >
            <Code2 />
            <T zh="开发者模式" en="Developer mode" />
          </button>
        </div>
      </div>

      {/* 常见误会当场说清：工具调用走主链路零配置，扩展是另一条路 */}
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="card-body" style={{ display: 'flex', gap: 18, flexWrap: 'wrap' }}>
          <div style={{ flex: 1, minWidth: 260 }}>
            <div style={{ marginBottom: 4 }}>
              <b>
                <T zh="工具调用（function calling）—— 不用配" en="Tool calling — nothing to configure" />
              </b>
            </div>
            <div className="sub" style={{ lineHeight: 1.7 }}>
              <T
                zh="Agent 请求里的 tools 参数随主网关原样透传：模型支持就直接用；没实测支持会被能力门禁降权、自动换支持的家。全程零配置，也不需要扩展。"
                en="The tools parameter in agent requests passes through the gateway untouched: supported models use it directly; unverified ones get down-weighted by the capability gate. Zero config, no extension needed."
              />
            </div>
          </div>
          <div style={{ flex: 1, minWidth: 260 }}>
            <div style={{ marginBottom: 4 }}>
              <b>
                <T zh="扩展（语音 / 图片）—— 要单独配上游" en="Extensions (ASR / image) — need their own upstream" />
              </b>
            </div>
            <div className="sub" style={{ lineHeight: 1.7 }}>
              <T
                zh="语音转写、图片生成不是 chat 模型的能力，免费 chat 厂商不提供这类端点——所以每个扩展单独配一个支持它的上游（地址 / 模型 / key）。配一次，之后按需自动拉起。"
                en="ASR and image generation are not chat-model capabilities — free chat providers don't serve them. Each extension points at its own upstream (URL / model / key); configure once, it auto-starts on demand."
              />
            </div>
          </div>
        </div>
      </div>

      {!api && (
        <div className="card" style={{ marginBottom: 14 }}>
          <Empty
            title={<T zh="桌面端专属" en="Desktop only" />}
            hint={
              <T
                zh="扩展宿主跑在应用进程里，浏览器预览没有它。装好桌面版后这里会列出可用的能力插件。"
                en="The extension host lives inside the app process. The browser preview does not have it."
              />
            }
          />
        </div>
      )}

      {err && <div className="hint-line">{err}</div>}

      {list &&
        (list.length === 0 ? (
          <div className="card" style={{ marginBottom: 14 }}>
            <Empty title={<T zh="还没有可用扩展" en="No extensions yet" />} />
          </div>
        ) : (
          <div className="two-col">
            <div>
              {list.map((e) => (
                <ExtCard key={e.id} ext={e} onChanged={setList} />
              ))}
            </div>

            <div className="card" style={{ alignSelf: 'flex-start' }}>
              <div className="card-head">
                <h3>
                  <T zh="安装规则" en="Install rules" />
                </h3>
              </div>
              <div className="card-body" style={{ fontSize: 12, lineHeight: 1.9, color: 'var(--text-2)' }}>
                <div>
                  1.{' '}
                  <b>
                    <T zh="看权限" en="Read permissions" />
                  </b>
                  ：<T zh="列清可访问的域名与目录，不勾不给装" en="domains and paths are listed up front" />
                </div>
                <div>
                  2.{' '}
                  <b>
                    <T zh="验签名" en="Verify signature" />
                  </b>
                  ：<T zh="作者公钥 + 哈希，双击安装也要校验" en="author key + hash, even on double-click" />
                </div>
                <div>
                  3.{' '}
                  <b>
                    <T zh="进程隔离" en="Process isolation" />
                  </b>
                  ：<T zh="sidecar 独立进程，崩溃不带上主网关" en="sidecar crash never takes the gateway down" />
                </div>
                <div>
                  4.{' '}
                  <b>
                    <T zh="端口不暴露" en="No exposed port" />
                  </b>
                  ：<T zh="动态端口只进内部路由表" en="dynamic ports stay internal" />
                </div>
                <div className="hint-line">
                  <T
                    zh="这四条是上线门槛，不是愿望清单：任何一条做不到，扩展就不该进安装目录。"
                    en="These four are gates, not a wish list: fail any one and the extension does not ship."
                  />
                </div>
              </div>
            </div>
          </div>
        ))}

      <div className="hint-line">
        <Puzzle size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
        <T
          zh="图片 / 音频这类非 chat 能力走扩展，不进主网关：它们的授权模型、风险、资源占用都和 chat 完全不同。"
          en="Image and audio capabilities live in extensions, not the gateway: different auth model, risk, and footprint."
        />
      </div>
    </>
  );
}
