/**
 * Electron 桌面端接入层。
 * 浏览器里打开时 window.freeport 不存在，所有调用自动降级为 no-op。
 */

interface FreeportBridge {
  isElectron: true;
  platform: NodeJS.Platform;
  app: { info: () => Promise<Record<string, unknown>> };
  win: {
    minimize: () => Promise<void>;
    toggleMaximize: () => Promise<boolean>;
    close: () => Promise<void>;
  };
  gateway: {
    status: () => Promise<{
      running: boolean;
      port: number;
      bind: string;
      requests24h: number;
    }>;
  };
}

declare global {
  interface Window {
    freeport?: FreeportBridge;
  }
}

export const isDesktop = typeof window !== 'undefined' && !!window.freeport;
export const desktop = () => window.freeport ?? null;

/** 在 main.tsx 最先调用：给 <html> 打标记，CSS 据此隐藏假红绿灯、启用拖拽区 */
export function initDesktop(): void {
  if (!isDesktop) return;
  const root = document.documentElement;
  root.classList.add('is-electron');
  root.classList.add(`platform-${window.freeport!.platform}`);

  // 把真实版本号喂给侧栏（data-*，Sidebar 读取）
  window.freeport!.app
    .info()
    .then((info) => {
      if (typeof info.version === 'string') {
        document.documentElement.dataset.appVersion = info.version;
      }
    })
    .catch(() => undefined);
}
