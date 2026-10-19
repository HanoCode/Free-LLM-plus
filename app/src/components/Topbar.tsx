import { ChevronRight, Command, Moon, Pause, Play, Search, Sun } from 'lucide-react';
import { useApp, applyTheme, type PageId } from '../lib/store';
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
  const { page, theme, setTheme, lang, setLang, paused, togglePaused } = useApp();

  return (
    <div className="topbar">
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
          onClick={() => {
            togglePaused();
            toast(
              paused
                ? lang === 'zh'
                  ? '已恢复路由'
                  : 'Routing resumed'
                : lang === 'zh'
                  ? '已暂停路由，请求将直接返回 503'
                  : 'Routing paused, requests return 503',
            );
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
