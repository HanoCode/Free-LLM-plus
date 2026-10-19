import {
  Boxes,
  Compass,
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

interface NavDef {
  id: PageId;
  label_zh: string;
  label_en: string;
  icon: typeof Boxes;
  count?: string;
  accent?: boolean;
}

const groups: { zh: string; en: string; items: NavDef[] }[] = [
  {
    zh: '网关',
    en: 'Gateway',
    items: [
      { id: 'overview', label_zh: '概览', label_en: 'Overview', icon: LayoutDashboard },
      { id: 'models', label_zh: '模型', label_en: 'Models', icon: Boxes, count: '14' },
      { id: 'providers', label_zh: '厂商', label_en: 'Providers', icon: Server, count: '11' },
      { id: 'accounts', label_zh: '账号', label_en: 'Accounts', icon: Users, count: '10' },
      { id: 'discover', label_zh: '发现', label_en: 'Discover', icon: Compass, count: '2', accent: true },
    ],
  },
  {
    zh: '调度',
    en: 'Dispatch',
    items: [
      { id: 'routing', label_zh: '路由', label_en: 'Routing', icon: Route },
      { id: 'agents', label_zh: '注入', label_en: 'Agents', icon: Plug, count: '4' },
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
  const { page, setPage, paused } = useApp();

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
          <div className="brand-name">Freeport</div>
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
                  {it.count && <span className={`count ${it.accent ? 'accent' : ''}`}>{it.count}</span>}
                </button>
              );
            })}
          </div>
        ))}
      </nav>

      <div className="sidebar-foot">
        <div className="gw-card">
          <div className="gw-top">
            <span className={`pulse ${paused ? 'off' : ''}`} style={paused ? { background: 'var(--faint)' } : undefined} />
            <span>
              <T zh={paused ? '网关已暂停' : '网关运行中'} en={paused ? 'Gateway paused' : 'Gateway running'} />
            </span>
            <CopyBtn value="http://127.0.0.1:8787" label="" />
          </div>
          <div className="gw-addr">
            <span>127.0.0.1:8787</span>
            <ArrowUpRight size={11} className="faint" />
          </div>
          <div className="gw-meta">
            <span>v0.1.0</span>
            <span>·</span>
            <span>
              <T zh="今日 1,284 次" en="1,284 today" />
            </span>
          </div>
        </div>

        <div className="pro-card">
          <b>
            <T zh="Freeport Pro" en="Freeport Pro" />
          </b>
          <p>
            <T
              zh="实时目录 · 额度情报 · 多机同步。免费版功能完整，只是目录 7 天更新一次。"
              en="Live catalog, quota intel, multi-device sync. Free tier keeps every feature, catalog refreshes weekly."
            />
          </p>
          <button>
            <T zh="了解 Pro" en="See Pro" />
            <ArrowUpRight size={11} />
          </button>
        </div>
      </div>
    </aside>
  );
}
