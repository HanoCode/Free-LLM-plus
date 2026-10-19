/**
 * 桌面端接入层。
 *
 * 启动时装 Tauri 桥；在普通浏览器里打开时不做任何事，所有调用自然降级。
 */

import './bridge';
import { installTauriBridge, isTauri } from './ipc';

const installed = installTauriBridge();

export const isDesktop = typeof window !== 'undefined' && !!window.freeport;
export const desktop = () => window.freeport ?? null;
export const isTauriRuntime = installed && isTauri();

/** 在 main.tsx 最先调用：给 <html> 打标记，CSS 据此隐藏假红绿灯、启用拖拽区 */
export function initDesktop(): void {
  if (!isDesktop) return;
  const root = document.documentElement;
  root.classList.add('is-desktop');
  root.classList.add(`platform-${window.freeport!.platform}`);
  if (isTauriRuntime) root.classList.add('is-tauri');

  window.freeport!.app
    .info()
    .then((info) => {
      if (typeof info.version === 'string') {
        document.documentElement.dataset.appVersion = info.version;
      }
    })
    .catch(() => undefined);
}
