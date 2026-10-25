import {
  BarChart3,
  Boxes,
  FlaskConical,
  LayoutDashboard,
  Plug,
  Puzzle,
  Route,
  ScrollText,
  Server,
  Settings,
  ArrowUpRight,
} from 'lucide-react';
import { useApp, type PageId } from '../lib/store';
import { T } from '../lib/i18n';
import { CopyBtn } from './ui';
import { useGateway } from '../lib/gateway';
/* 品牌图标直接取自 src-tauri/icons 的同一张图（复制到 assets 供打包）：
   侧栏、favicon、应用图标从此是同一个源，不会再各画一套 */
import brandIcon from '../assets/brand.png';

interface NavDef {
  id: PageId;
  label_zh: string;
  label_en: string;
  icon: typeof Boxes;
  count?: string;
  accent?: boolean;
}

/** 连接页聚合的旧 id：深链落在其中任何一个，侧栏都点亮「连接」 */
const CONNECT_GROUP: PageId[] = ['connect', 'providers', 'accounts', 'discover'];

const groups: { zh: string; en: string; items: NavDef[] }[] = [
  {
    zh: '网关',
    en: 'Gateway',
    items: [
      { id: 'overview', label_zh: '概览', label_en: 'Overview', icon: LayoutDashboard },
      { id: 'models', label_zh: '模型', label_en: 'Models', icon: Boxes },
      { id: 'connect', label_zh: '连接', label_en: 'Connect', icon: Server },
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
      { id: 'reports', label_zh: '报表', label_en: 'Reports', icon: BarChart3 },
      { id: 'settings', label_zh: '设置', label_en: 'Settings', icon: Settings },
    ],
  },
];

export function Sidebar() {
  const { page, setPage } = useApp();
  const gw = useGateway((s) => s.status);
  const providers = useGateway((s) => s.providers);
  const agents = useGateway((s) => s.agents);

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
          <img src={brandIcon} alt="Freeport" draggable={false} />
        </div>
        <div>
          <div className="brand-name" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            Freeport
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
              const active = it.id === 'connect' ? CONNECT_GROUP.includes(page) : page === it.id;
              // 只显示真实计数；没有数据时不显示，不用占位数字充数
              const live =
                it.id === 'models' && modelCount
                  ? String(modelCount)
                  : it.id === 'connect' && credentialCount
                    ? String(credentialCount)
                    : it.id === 'agents' && injectedCount
                      ? String(injectedCount)
                      : it.count;
              return (
                <button
                  key={it.id}
                  className={`nav-item ${active ? 'active' : ''}`}
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
            <CopyBtn value={addr} label="" tip="复制网关地址 / Copy address" />
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
      </div>
    </aside>
  );
}
