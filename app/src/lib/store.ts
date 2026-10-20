import { create } from 'zustand';

export type PageId =
  | 'overview'
  | 'models'
  | 'providers'
  | 'accounts'
  | 'discover'
  | 'routing'
  | 'agents'
  | 'extensions'
  | 'logs'
  | 'settings';

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
  'providers',
  'accounts',
  'discover',
  'routing',
  'agents',
  'extensions',
  'logs',
  'settings',
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
  },
  setTheme: (theme) => {
    localStorage.setItem('fp.theme', theme);
    set({ theme });
  },
}));

export const applyTheme = (theme: Theme) => {
  document.documentElement.dataset.theme = theme;
};
