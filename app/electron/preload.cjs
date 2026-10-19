/* Freeport preload —— 只暴露白名单能力，渲染层拿不到 Node */

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('freeport', {
  isElectron: true,
  platform: process.platform,

  app: {
    info: () => ipcRenderer.invoke('app:info'),
  },

  win: {
    minimize: () => ipcRenderer.invoke('win:minimize'),
    toggleMaximize: () => ipcRenderer.invoke('win:toggle-maximize'),
    close: () => ipcRenderer.invoke('win:close'),
    toggleDevTools: () => ipcRenderer.invoke('win:toggle-devtools'),
  },

  gateway: {
    status: () => ipcRenderer.invoke('gateway:status'),
  },

  shell: {
    openExternal: (url) => ipcRenderer.invoke('shell:open-external', url),
  },
});
