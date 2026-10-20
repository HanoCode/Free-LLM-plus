import { Code2, Image as ImageIcon, Mic, Puzzle } from 'lucide-react';
import { T } from '../lib/i18n';
import { toast } from '../components/ui';

/**
 * 扩展页。
 *
 * v1 **没有任何扩展可装**：Extension Host 在 v1.5。所以这里不摆「已安装」
 * 的假条目，也不编资源占用数字——那会让用户以为点一下就能用。
 * 保留的是安装规则与能力边界，这两部分是已经定下来的设计约束。
 */
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
          <button
            className="btn ghost"
            onClick={() => toast('扩展宿主还没实现（v1.5）/ Extension host ships in v1.5')}
          >
            <Code2 />
            <T zh="开发者模式" en="Developer mode" />
          </button>
        </div>
      </div>

      <div className="card" style={{ marginBottom: 14, padding: '28px 20px', textAlign: 'center' }}>
        <div style={{ fontWeight: 600, fontSize: 13.5 }}>
          <T zh="扩展宿主在 v1.5" en="Extension host lands in v1.5" />
        </div>
        <div className="muted" style={{ fontSize: 12.5, marginTop: 6, lineHeight: 1.8 }}>
          <T
            zh="当前版本只有 chat 主链路，没有 sidecar 进程、没有签名校验、也没有扩展目录。这一页存在的意义是把安装规则先定下来，避免上线时边写边改。"
            en="This build only has the chat path: no sidecars, no signature checks, no catalog. The page exists so the install rules are fixed before the code lands."
          />
        </div>
      </div>

      <div className="two-col">
        <div className="card">
          <div className="card-head">
            <h3>
              <T zh="计划中的能力" en="Planned capabilities" />
            </h3>
            <span className="sub">
              <T zh="都还没实现" en="none implemented yet" />
            </span>
          </div>
          <div className="card-body">
            <div className="ext-row" style={{ opacity: 0.75 }}>
              <div className="ext-ico">
                <Mic size={15} />
              </div>
              <div className="ext-main">
                <div className="ext-name">
                  ASR
                  <span className="badge gray">
                    <T zh="计划中" en="planned" />
                  </span>
                </div>
                <div className="ext-sub">
                  <T zh="音频转写 sidecar，按需拉起" en="audio transcription sidecar, started on demand" />
                </div>
              </div>
            </div>
            <div className="ext-row" style={{ opacity: 0.75, borderBottom: 'none' }}>
              <div className="ext-ico">
                <ImageIcon size={15} />
              </div>
              <div className="ext-main">
                <div className="ext-name">
                  <T zh="图片生成" en="Image" />
                  <span className="badge gray">
                    <T zh="计划中" en="planned" />
                  </span>
                </div>
                <div className="ext-sub">
                  <T zh="图片 sidecar，不进 chat 主链路" en="image sidecar, kept out of the chat path" />
                </div>
              </div>
            </div>
            <div className="hint-line">
              <T
                zh="这两项的共同点：授权模型、风险、资源占用都和 chat 完全不同，混进主网关会把限额与故障域搅在一起。"
                en="Both differ from chat in auth model, risk, and footprint. Mixing them into the gateway would tangle quotas and failure domains."
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
            <div className="hint-line">
              <T
                zh="这四条是上线门槛，不是愿望清单：任何一条做不到，扩展就不该进安装目录。"
                en="These four are gates, not a wish list: fail any one and the extension does not ship."
              />
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
