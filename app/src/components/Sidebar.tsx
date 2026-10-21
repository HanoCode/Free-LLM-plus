import {
  Boxes,
  Compass,
  FlaskConical,
  LayoutDashboard,
  Plug,
  Puzzle,
  Route,
  ScrollText,
  Server,
  Settings,
  Sparkles,
  Users,
  ArrowUpRight,
} from 'lucide-react';
import { useApp, type PageId } from '../lib/store';
import { T } from '../lib/i18n';
import { CopyBtn } from './ui';
import { useGateway } from '../lib/gateway';

interface NavDef {
  id: PageId;
  label_zh: string;
  label_en: string;
  icon: typeof Boxes;
  count?: string;
  accent?: boolean;
}

/** 宽限期剩余天数（向上取整）。宽限内能力完整，但 `daysLeft` 已是负数，得另算。 */
function graceDays(until?: number): number {
  if (!until) return 0;
  return Math.max(0, Math.ceil((until - Date.now() / 1000) / 86400));
}

const groups: { zh: string; en: string; items: NavDef[] }[] = [
  {
    zh: '网关',
    en: 'Gateway',
    items: [
      { id: 'overview', label_zh: '概览', label_en: 'Overview', icon: LayoutDashboard },
      { id: 'models', label_zh: '模型', label_en: 'Models', icon: Boxes },
      { id: 'providers', label_zh: '厂商', label_en: 'Providers', icon: Server },
      { id: 'accounts', label_zh: '账号', label_en: 'Accounts', icon: Users },
      { id: 'discover', label_zh: '发现', label_en: 'Discover', icon: Compass, accent: true },
      { id: 'playground', label_zh: '试验场', label_en: 'Playground', icon: FlaskConical },
    ],
  },
  {
    zh: '调度',
    en: 'Dispatch',
    items: [
      { id: 'routing', label_zh: '路由', label_en: 'Routing', icon: Route },
      { id: 'agents', label_zh: '注入', label_en: 'Agents', icon: Plug },
    ],
  },
  {
    zh: '系统',
    en: 'System',
    items: [
      { id: 'extensions', label_zh: '扩展', label_en: 'Extensions', icon: Puzzle },
      { id: 'logs', label_zh: '日志', label_en: 'Logs', icon: ScrollText },
      { id: 'settings', label_zh: '设置', label_en: 'Settings', icon: Settings },
    ],
  },
];

export function Sidebar() {
  const { page, setPage, setLicenseDialog } = useApp();
  const gw = useGateway((s) => s.status);
  const providers = useGateway((s) => s.providers);
  const agents = useGateway((s) => s.agents);
  const license = useGateway((s) => s.license);

  const running = gw ? gw.running : true;
  const port = gw?.port ?? 8787;
  const addr = `http://127.0.0.1:${port}`;
  const version = document.documentElement.dataset.appVersion || '0.1.0';
  const requests = gw ? gw.requests24h : 1284;
  const modelCount = providers.reduce((n, p) => n + p.models, 0);
  // 有 Key 才算一份凭据；没填 Key 的厂商不该在「账号」里计数。
  // 一厂商多凭据之后这里的粒度是凭据，不是厂商。
  const credentialCount = providers.reduce(
    (n, p) => n + p.accounts.filter((a) => a.hasKey).length,
    0,
  );
  const injectedCount = agents.filter((a) => a.state === 'injected').length;

  return (
    <aside className="sidebar">
      <div className="traffic">
        <i />
        <i />
        <i />
      </div>

      <div className="brand">
        <div className="logo">
          <Sparkles />
        </div>
        <div>
          <div className="brand-name" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            Freeport
            {/* 档位徽标跟着许可证走：宽限期仍是 Pro，但用警示色提示续费 */}
            {license?.pro && (
              <span className={`brand-pro ${license.status === 'grace' ? 'warn' : ''}`}>
                PRO
              </span>
            )}
          </div>
          <div className="brand-tag">自由港</div>
        </div>
      </div>

      <nav className="nav">
        {groups.map((g) => (
          <div key={g.zh}>
            <div className="nav-label">
              <T zh={g.zh} en={g.en} />
            </div>
            {g.items.map((it) => {
              const Icon = it.icon;
              // 只显示真实计数；没有数据时不显示，不用占位数字充数
              const live =
                it.id === 'models' && modelCount
                  ? String(modelCount)
                  : it.id === 'providers' && providers.length
                    ? String(providers.length)
                    : it.id === 'accounts' && credentialCount
                      ? String(credentialCount)
                      : it.id === 'agents' && injectedCount
                        ? String(injectedCount)
                        : it.count;
              return (
                <button
                  key={it.id}
                  className={`nav-item ${page === it.id ? 'active' : ''}`}
                  onClick={() => setPage(it.id)}
                >
                  <Icon />
                  <span>
                    <T zh={it.label_zh} en={it.label_en} />
                  </span>
                  {live && <span className={`count ${it.accent ? 'accent' : ''}`}>{live}</span>}
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      <div className="sidebar-foot">
        <div className="gw-card">
          <div className="gw-top">
            <span className={`pulse ${running ? '' : 'off'}`} style={running ? undefined : { background: 'var(--faint)' }} />
            <span>
              <T zh={running ? '网关运行中' : '网关未启动'} en={running ? 'Gateway running' : 'Gateway offline'} />
            </span>
            <CopyBtn value={addr} label="" />
          </div>
          <div className="gw-addr">
            <span>127.0.0.1:{port}</span>
            <ArrowUpRight size={11} className="faint" />
          </div>
          <div className="gw-meta">
            <span>v{version}</span>
            <span>·</span>
            <span>
              <T zh={`今日 ${requests.toLocaleString()} 次`} en={`${requests.toLocaleString()} today`} />
            </span>
          </div>
        </div>

        <div className={`pro-card ${license?.pro ? 'active' : ''}`}>
          <b>
            <T zh="Freeport Pro" en="Freeport Pro" />
          </b>
          {license?.pro ? (
            <>
              <p>
                {license.status === 'grace' ? (
                  <T
                    zh={`宽限期：${license.subject ?? 'Pro'} 已过期，完全能力还保留 ${graceDays(license.graceUntil)} 天。`}
                    en={`Grace: ${license.subject ?? 'Pro'} expired, full features for ${graceDays(license.graceUntil)} more days.`}
                  />
                ) : (
                  <T
                    zh={`已激活 · ${license.subject ?? 'Pro'} · 还剩 ${license.daysLeft ?? 0} 天。`}
                    en={`Active · ${license.subject ?? 'Pro'} · ${license.daysLeft ?? 0} days left.`}
                  />
                )}
              </p>
              <button onClick={() => setPage('settings')}>
                <T zh="管理许可证" en="Manage license" />
                <ArrowUpRight size={11} />
              </button>
            </>
          ) : (
            <>
              <p>
                <T
                  zh="实时目录 · 额度情报 · 多机同步。免费版功能完整，只是目录 7 天更新一次。"
                  en="Live catalog, quota intel, multi-device sync. Free tier keeps every feature, catalog refreshes weekly."
                />
              </p>
              <button onClick={() => setLicenseDialog(true)}>
                <T zh="输入激活码" en="Enter license key" />
                <ArrowUpRight size={11} />
              </button>
            </>
          )}
        </div>
      </div>
    </aside>
  );
}
