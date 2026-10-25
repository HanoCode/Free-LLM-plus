import { useEffect } from 'react';
import { Sidebar } from './components/Sidebar';
import { Topbar } from './components/Topbar';
import { ToastHost, ConfirmHost } from './components/ui';
import { useApp, applyTheme } from './lib/store';
import { initGateway } from './lib/gateway';
import { Overview } from './pages/Overview';
import { Models } from './pages/Models';
import { Connect } from './pages/Connect';
import { Routing } from './pages/Routing';
import { Agents } from './pages/Agents';
import { Extensions } from './pages/Extensions';
import { Logs } from './pages/Logs';
import { Reports } from './pages/Reports';
import { Settings } from './pages/Settings';
import { Playground } from './pages/Playground';

// 厂商/凭据/发现统一进「连接」页；旧 id 仍可达（深链兼容），渲染同一聚合页
const pages = {
  overview: Overview,
  models: Models,
  connect: Connect,
  providers: Connect,
  accounts: Connect,
  discover: Connect,
  routing: Routing,
  agents: Agents,
  extensions: Extensions,
  logs: Logs,
  reports: Reports,
  settings: Settings,
  playground: Playground,
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

  useEffect(() => initGateway(), []);

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
      <ConfirmHost />
    </div>
  );
}
