// 必须排在 App 之前：页面模块一求值就会读 window.freeport 判断是否有网关
import './lib/ipc';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { initDesktop } from './lib/desktop';
import './styles/tokens.css';
import './styles/app.css';

initDesktop();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
