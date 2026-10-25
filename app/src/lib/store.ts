import { create } from 'zustand';

export type PageId =
  | 'overview'
  | 'models'
  | 'connect'
  | 'providers'
  | 'accounts'
  | 'discover'
  | 'routing'
  | 'agents'
  | 'extensions'
  | 'logs'
  | 'reports'
  | 'settings'
  | 'playground';

export type Lang = 'zh' | 'en';
export type Theme = 'light' | 'dark';

interface AppState {
  page: PageId;
  lang: Lang;
  theme: Theme;
  setPage: (p: PageId) => void;
  setLang: (l: Lang) => void;
  setTheme: (t: Theme) => void;
}

const savedLang = (localStorage.getItem('fp.lang') as Lang) || 'zh';
const savedTheme =
  (localStorage.getItem('fp.theme') as Theme) ||
  (window.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');

const validPages: PageId[] = [
  'overview',
  'models',
  'connect',
  'providers',
  'accounts',
  'discover',
  'routing',
  'agents',
  'extensions',
  'logs',
  'reports',
  'settings',
  'playground',
];

function initialPage(): PageId {
  const h = location.hash.replace(/^#\/?/, '') as PageId;
  return validPages.includes(h) ? h : 'overview';
}

export const useApp = create<AppState>((set) => ({
  page: initialPage(),
  lang: savedLang,
  theme: savedTheme,
  setPage: (page) => {
    history.replaceState(null, '', `#/${page}`);
    set({ page });
  },
  setLang: (lang) => {
    localStorage.setItem('fp.lang', lang);
    set({ lang });
    // 托盘菜单在 Rust 侧，读不到 localStorage：把语言同步进 DesktopPrefs。
    // web 环境（无 Tauri）静默跳过；失败也不影响界面语言本身。
    if ('__TAURI_INTERNALS__' in window) {
      void import('@tauri-apps/api/core')
        .then(({ invoke }) => invoke('win_set_prefs', { patch: { uiLang: lang } }))
        .catch(() => {});
    }
  },
  setTheme: (theme) => {
    localStorage.setItem('fp.theme', theme);
    set({ theme });
  },
}));

export const applyTheme = (theme: Theme) => {
  document.documentElement.dataset.theme = theme;
};
