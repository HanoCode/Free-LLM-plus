import { ChevronRight, Command, Moon, Pause, Play, Search, Sun } from 'lucide-react';
import { useApp, applyTheme, type PageId } from '../lib/store';
import { useGateway } from '../lib/gateway';
import { T } from '../lib/i18n';
import { toast } from './ui';

const titles: Record<PageId, [string, string]> = {
  overview: ['概览', 'Overview'],
  models: ['模型', 'Models'],
  providers: ['厂商', 'Providers'],
  accounts: ['账号', 'Accounts'],
  discover: ['发现', 'Discover'],
  routing: ['路由', 'Routing'],
  agents: ['注入', 'Agents'],
  extensions: ['扩展', 'Extensions'],
  logs: ['日志', 'Logs'],
  settings: ['设置', 'Settings'],
};

export function Topbar() {
  const { page, theme, setTheme, lang, setLang } = useApp();
  const online = useGateway((s) => s.available);
  const paused = useGateway((s) => s.status?.paused ?? false);
  const setPaused = useGateway((s) => s.setPaused);

  return (
    <div className="topbar" data-tauri-drag-region>
      <div className="crumbs">
        <span>Freeport</span>
        <ChevronRight />
        <b>
          <T zh={titles[page][0]} en={titles[page][1]} />
        </b>
      </div>

      <div className="top-actions">
        <button
          className="chip-btn"
          onClick={() => toast(lang === 'zh' ? '搜索：⌘K 打开命令面板（M4 实现）' : 'Search: ⌘K palette (M4)')}
        >
          <Search />
          <span className="faint">⌘K</span>
          <span className="kbd">/</span>
        </button>

        <button
          className="chip-btn"
          onClick={() => setLang(lang === 'zh' ? 'en' : 'zh')}
          title="语言 / Language"
        >
          <Command />
          {lang === 'zh' ? '中' : 'EN'}
        </button>

        <button
          className="chip-btn"
          onClick={() => {
            const next = theme === 'light' ? 'dark' : 'light';
            applyTheme(next);
            setTheme(next);
          }}
          title="主题 / Theme"
        >
          {theme === 'light' ? <Moon /> : <Sun />}
        </button>

        <button
          className="chip-btn"
          disabled={!online}
          title={
            online
              ? lang === 'zh'
                ? '暂停后业务请求返回 503，/health 与 /v1/models 仍可用'
                : 'Paused: business requests return 503, /health and /v1/models stay up'
              : lang === 'zh'
                ? '没有连接到网关'
                : 'no gateway connection'
          }
          onClick={async () => {
            try {
              await setPaused(!paused);
              toast(
                !paused
                  ? lang === 'zh'
                    ? '已暂停路由，业务请求返回 503'
                    : 'Routing paused, business requests return 503'
                  : lang === 'zh'
                    ? '已恢复路由'
                    : 'Routing resumed',
              );
            } catch (e) {
              toast(String(e));
            }
          }}
        >
          {paused ? <Play /> : <Pause />}
          <T zh={paused ? '恢复' : '暂停'} en={paused ? 'Resume' : 'Pause'} />
        </button>

        <div className="avatar">FP</div>
      </div>
    </div>
  );
}
