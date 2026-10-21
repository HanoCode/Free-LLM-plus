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
  | 'settings'
  | 'playground';

export type Lang = 'zh' | 'en';
export type Theme = 'light' | 'dark';

interface AppState {
  page: PageId;
  lang: Lang;
  theme: Theme;
  /** Pro 激活弹窗。放全局 store：侧边栏、设置页都能叫开它 */
  licenseDialog: boolean;
  setPage: (p: PageId) => void;
  setLang: (l: Lang) => void;
  setTheme: (t: Theme) => void;
  setLicenseDialog: (v: boolean) => void;
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
  licenseDialog: false,
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
  setLicenseDialog: (licenseDialog) => set({ licenseDialog }),
}));

export const applyTheme = (theme: Theme) => {
  document.documentElement.dataset.theme = theme;
};
