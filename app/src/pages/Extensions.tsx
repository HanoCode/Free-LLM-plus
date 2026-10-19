import { Code2, Image as ImageIcon, Mic, Puzzle } from 'lucide-react';
import { T } from '../lib/i18n';
import { extensions } from '../lib/data';
import { Badge, Status, toast } from '../components/ui';

export function Extensions() {
  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="扩展" en="Extensions" />
          </h1>
          <p>
            <T
              zh="独立进程的能力插件，只为 chat 之外的东西存在。装之前先看清它能访问什么。"
              en="Out-of-process capability plugins. Read the permissions before you install."
            />
          </p>
        </div>
        <div className="head-actions">
          <button className="btn ghost" onClick={() => toast('开发者模式：仅加载未签名扩展 / Dev mode')}>
            <Code2 />
            <T zh="开发者模式" en="Developer mode" />
          </button>
          <button className="btn primary" onClick={() => toast('扩展目录尚未上线（v1.5）/ Catalog coming in v1.5')}>
            <T zh="浏览目录" en="Browse" />
          </button>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 14 }}>
        <div className="card-head">
          <h3>
            <T zh="已安装" en="Installed" />
          </h3>
          <span className="sub">
            <T zh="扩展崩溃不影响 chat 主链路" en="a crash never touches the chat path" />
          </span>
        </div>

        {extensions.map((e) => (
          <div className="ext-row" key={e.id}>
            <div className="ext-ico">
              {e.id === 'whisper' ? <Mic size={15} /> : <ImageIcon size={15} />}
            </div>
            <div className="ext-main">
              <div className="ext-name">
                {e.name}
                {e.state === 'running' ? (
                  <Status tone="ok">
                    <T zh="运行中" en="running" />
                  </Status>
                ) : (
                  <Status tone="idle">
                    <T zh="已停止" en="stopped" />
                  </Status>
                )}
                <Badge tone="gray">{e.version}</Badge>
                {e.signed && (
                  <Badge tone="accent">
                    <T zh="已签名" en="signed" />
                  </Badge>
                )}
              </div>
              <div className="ext-sub">
                {e.caps}
                <br />
                <T zh={e.detail_zh} en={e.detail_en} />
              </div>
              <div className="ext-perm">{e.perms}</div>
              {e.warn && (
                <div className="ext-sub" style={{ color: 'var(--warn)' }}>
                  ⚠ <T zh={e.warn.zh} en={e.warn.en} />
                </div>
              )}
            </div>
            <div className="ext-act">
              <button className="btn ghost sm" onClick={() => toast('打开扩展日志 / Extension logs')}>
                <T zh="日志" en="Logs" />
              </button>
              <button
                className="btn ghost sm"
                onClick={() =>
                  toast(
                    e.state === 'running'
                      ? '已停止扩展 / Stopped'
                      : '已启动扩展 / Started',
                  )
                }
              >
                {e.state === 'running' ? <T zh="停止" en="Stop" /> : <T zh="启动" en="Start" />}
              </button>
              <button className="btn ghost sm" onClick={() => toast('卸载前会清理扩展数据目录 / Data will be removed')}>
                <T zh="卸载" en="Remove" />
              </button>
            </div>
          </div>
        ))}
      </div>

      <div className="two-col">
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="资源占用" en="Resources" />
            </h3>
          </div>
          <div className="card-body">
            <div className="quota-row">
              <span className="nm">
                <T zh="内存" en="Memory" />
              </span>
              <div className="track">
                <i style={{ width: '5%' }} />
              </div>
              <span className="pc">96M</span>
            </div>
            <div className="quota-row">
              <span className="nm">
                <T zh="磁盘" en="Disk" />
              </span>
              <div className="track">
                <i style={{ width: '18%' }} />
              </div>
              <span className="pc">2.1G</span>
            </div>
            <div className="quota-row" style={{ marginBottom: 0 }}>
              <span className="nm">
                <T zh="今日调用" en="Calls" />
              </span>
              <div className="track">
                <i style={{ width: '0%' }} />
              </div>
              <span className="pc">0</span>
            </div>
            <div className="hint-line">
              <T
                zh="扩展运行在独立 sidecar 进程里，端口动态分配且不对外暴露。"
                en="Extensions run in isolated sidecars with dynamic, non-exposed ports."
              />
            </div>
          </div>
        </div>

        <div className="card">
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
          </div>
        </div>
      </div>

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
