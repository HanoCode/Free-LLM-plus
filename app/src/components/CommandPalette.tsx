import { useEffect, useMemo, useRef, useState } from 'react';
import {
  Boxes,
  FlaskConical,
  LayoutDashboard,
  Moon,
  Pause,
  Play,
  Plug,
  Puzzle,
  Radar,
  RefreshCw,
  Route,
  ScrollText,
  Search,
  Settings,
  Sun,
  Waypoints,
} from 'lucide-react';
import { useApp, applyTheme, type PageId } from '../lib/store';
import { T } from '../lib/i18n';
import { useGateway } from '../lib/gateway';
import { toast } from './ui';

interface Cmd {
  id: string;
  zh: string;
  en: string;
  group: 'nav' | 'action';
  icon: typeof Boxes;
  run: () => void;
}

/**
 * ⌘K 命令面板：页面跳转 + 少量全局动作。
 * 不做模糊搜索排名这种花活——前缀/包含匹配对九个页面足够。
 */
export function CommandPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { setPage, theme, setTheme, lang, setLang } = useApp();
  const online = useGateway((s) => s.available);
  const paused = useGateway((s) => s.status?.paused ?? false);
  const setPaused = useGateway((s) => s.setPaused);
  const runPatrol = useGateway((s) => s.runPatrol);
  const refresh = useGateway((s) => s.refresh);

  const [q, setQ] = useState('');
  const [sel, setSel] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const cmds: Cmd[] = useMemo(() => {
    const nav = (id: PageId, zh: string, en: string, icon: typeof Boxes): Cmd => ({
      id: `nav:${id}`,
      zh,
      en,
      group: 'nav',
      icon,
      run: () => setPage(id),
    });
    const list: Cmd[] = [
      nav('overview', '打开概览', 'Go to Overview', LayoutDashboard),
      nav('connect', '打开连接（接入模型源）', 'Go to Connect', Plug),
      nav('models', '打开模型目录', 'Go to Models', Boxes),
      nav('routing', '打开路由', 'Go to Routing', Route),
      nav('agents', '打开注入（接入工具）', 'Go to Agents', Waypoints),
      nav('extensions', '打开扩展', 'Go to Extensions', Puzzle),
      nav('logs', '打开日志', 'Go to Logs', ScrollText),
      nav('settings', '打开设置', 'Go to Settings', Settings),
      nav('playground', '打开试验场', 'Go to Playground', FlaskConical),
      {
        id: 'act:pause',
        zh: paused ? '恢复路由' : '暂停路由',
        en: paused ? 'Resume routing' : 'Pause routing',
        group: 'action',
        icon: paused ? Play : Pause,
        run: () => {
          if (!online) return toast('没有连接到网关 / no gateway connection');
          void setPaused(!paused)
            .then(() => toast(paused ? '已恢复路由' : '已暂停路由，业务请求返回 503'))
            .catch((e) => toast(String(e)));
        },
      },
      {
        id: 'act:patrol',
        zh: '立即巡检（增量）',
        en: 'Run patrol now',
        group: 'action',
        icon: Radar,
        run: () => {
          if (!online) return toast('没有连接到网关 / no gateway connection');
          void runPatrol(false)
            .then((r) => toast(r.skipped ?? `巡检完成：探了 ${r.ran} 个 · 成功 ${r.ok} · 失败 ${r.failed}`))
            .catch((e) => toast(String(e)));
        },
      },
      {
        id: 'act:refresh',
        zh: '刷新全部数据',
        en: 'Refresh all data',
        group: 'action',
        icon: RefreshCw,
        run: () => {
          void refresh().then(() => toast('已刷新 / refreshed'));
        },
      },
      {
        id: 'act:theme',
        zh: theme === 'light' ? '切换到深色主题' : '切换到浅色主题',
        en: theme === 'light' ? 'Switch to dark theme' : 'Switch to light theme',
        group: 'action',
        icon: theme === 'light' ? Moon : Sun,
        run: () => {
          const next = theme === 'light' ? 'dark' : 'light';
          applyTheme(next);
          setTheme(next);
        },
      },
      {
        id: 'act:lang',
        zh: '切换到 English',
        en: '切换到中文',
        group: 'action',
        icon: Waypoints,
        run: () => setLang(lang === 'zh' ? 'en' : 'zh'),
      },
    ];
    return list;
  }, [setPage, paused, online, theme, lang, setPaused, runPatrol, refresh, setTheme, setLang]);

  const filtered = useMemo(() => {
    const key = q.trim().toLowerCase();
    if (!key) return cmds;
    return cmds.filter(
      (c) => c.zh.toLowerCase().includes(key) || c.en.toLowerCase().includes(key),
    );
  }, [cmds, q]);

  // 打开时重置输入并聚焦；关闭时还焦点给页面
  useEffect(() => {
    if (open) {
      setQ('');
      setSel(0);
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  useEffect(() => {
    if (sel >= filtered.length) setSel(0);
  }, [filtered, sel]);

  if (!open) return null;

  const commit = (c: Cmd | undefined) => {
    if (!c) return;
    c.run();
    onClose();
  };

  return (
    <div className="palette-mask" onMouseDown={onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        <div className="palette-input">
          <Search size={14} />
          <input
            ref={inputRef}
            value={q}
            placeholder="搜索页面或动作… / search pages or actions"
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') onClose();
              else if (e.key === 'ArrowDown') {
                e.preventDefault();
                setSel((s) => Math.min(s + 1, filtered.length - 1));
              } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                setSel((s) => Math.max(s - 1, 0));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                commit(filtered[sel]);
              }
            }}
          />
          <span className="kbd">esc</span>
        </div>
        <div className="palette-list" ref={listRef}>
          {filtered.length === 0 && (
            <div className="palette-empty">
              <T zh="没有匹配项" en="No matches" />
            </div>
          )}
          {filtered.map((c, i) => {
            const Icon = c.icon;
            return (
              <button
                key={c.id}
                className={`palette-item ${i === sel ? 'active' : ''}`}
                onMouseEnter={() => setSel(i)}
                onClick={() => commit(c)}
              >
                <Icon size={14} />
                <span>{lang === 'zh' ? c.zh : c.en}</span>
                <em>{c.group === 'nav' ? '页面' : '动作'}</em>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** 全局快捷键：⌘K / Ctrl+K 开面板。挂在 Topbar，一次监听即可。 */
export function useCommandPalette(open: boolean, setOpen: (v: boolean) => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        setOpen(!open);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, setOpen]);
}
