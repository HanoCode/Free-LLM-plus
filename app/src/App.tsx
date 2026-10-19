import { useEffect } from 'react';
import { Sidebar } from './components/Sidebar';
import { Topbar } from './components/Topbar';
import { ToastHost } from './components/ui';
import { useApp, applyTheme } from './lib/store';
import { Overview } from './pages/Overview';
import { Models } from './pages/Models';
import { Providers } from './pages/Providers';
import { Accounts } from './pages/Accounts';
import { Discover } from './pages/Discover';
import { Routing } from './pages/Routing';
import { Agents } from './pages/Agents';
import { Extensions } from './pages/Extensions';
import { Logs } from './pages/Logs';
import { Settings } from './pages/Settings';

const pages = {
  overview: Overview,
  models: Models,
  providers: Providers,
  accounts: Accounts,
  discover: Discover,
  routing: Routing,
  agents: Agents,
  extensions: Extensions,
  logs: Logs,
  settings: Settings,
};

export default function App() {
  const { page, theme, lang } = useApp();
  const View = pages[page];

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    document.documentElement.lang = lang === 'zh' ? 'zh-CN' : 'en';
  }, [lang]);

  return (
    <div className="app">
      <Sidebar />
      <main className="main">
        <Topbar />
        <section className="page" key={page}>
          <View />
        </section>
      </main>
      <ToastHost />
    </div>
  );
}
