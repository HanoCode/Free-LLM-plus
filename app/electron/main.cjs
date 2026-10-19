/* ============================================================
   Freeport — Electron 主进程
   - 窗口壳（macOS 使用 hiddenInset，红绿灯与侧栏对齐）
   - 托盘常驻、点击呼出、开机自启
   - 单实例、外链拦截、窗口位置记忆
   - v0.1 只托管窗口；网关（Hono :8787）在 M1 接入本进程
   ============================================================ */

const {
  app,
  BrowserWindow,
  Tray,
  Menu,
  nativeImage,
  ipcMain,
  shell,
  screen,
} = require('electron');
const path = require('node:path');
const fs = require('node:fs');

const isMac = process.platform === 'darwin';
const isDev = process.env.FP_DEV === '1' || !app.isPackaged;
const DEV_URL = process.env.FP_DEV_URL || 'http://127.0.0.1:5273';
const SMOKE = process.env.FP_SMOKE === '1';
const SMOKE_OUT = process.env.FP_SMOKE_OUT || path.join(app.getPath('temp'), 'freeport-smoke.png');

// 冒烟模式（CI / 沙箱环境）下 Chromium 沙箱无法初始化，禁用以完成截图自检
if (SMOKE) {
  app.commandLine.appendSwitch('no-sandbox');
  // 每次冒烟独立配置目录：锁不冲突、缓存不污染
  app.setPath(
    'userData',
    path.join(app.getPath('temp'), `freeport-smoke-${Date.now()}`),
  );
}

/** 开发态从仓库根取资源，打包后从 resources 取 */
function assetPath(name) {
  const base = app.isPackaged ? process.resourcesPath : path.join(__dirname, '..', 'build');
  return path.join(base, name);
}

let win = null;
let tray = null;
let quitting = false;

/* ── 窗口位置记忆 ─────────────────────────────── */
const statePath = () => path.join(app.getPath('userData'), 'window-state.json');

function loadBounds() {
  try {
    const s = JSON.parse(fs.readFileSync(statePath(), 'utf8'));
    if (typeof s.width === 'number' && typeof s.height === 'number') return s;
  } catch {
    /* 首次启动 */
  }
  return { width: 1240, height: 840 };
}

function saveBounds() {
  if (!win || win.isDestroyed() || win.isMinimized()) return;
  const b = win.getNormalBounds();
  try {
    fs.writeFileSync(statePath(), JSON.stringify({ ...b, maximized: win.isMaximized() }));
  } catch {
    /* 忽略：不影响功能 */
  }
}

/** 确保窗口回到可见区域（外接屏拔掉后不会跑到屏幕外） */
function clampToScreen(b) {
  const displays = screen.getAllDisplays();
  const visible = displays.some((d) => {
    const a = d.workArea;
    return (
      b.x + b.width > a.x &&
      b.y + b.height > a.y &&
      b.x < a.x + a.width &&
      b.y < a.y + a.height
    );
  });
  if (visible) return b;
  const p = screen.getPrimaryDisplay().workArea;
  return {
    width: Math.min(b.width, p.width - 80),
    height: Math.min(b.height, p.height - 80),
    x: Math.round(p.x + (p.width - Math.min(b.width, p.width - 80)) / 2),
    y: Math.round(p.y + (p.height - Math.min(b.height, p.height - 80)) / 2),
  };
}

/* ── 创建主窗口 ───────────────────────────────── */
function createWindow() {
  const saved = loadBounds();
  const bounds = clampToScreen({
    x: saved.x,
    y: saved.y,
    width: saved.width,
    height: saved.height,
  });

  win = new BrowserWindow({
    ...bounds,
    minWidth: 1000,
    minHeight: 680,
    show: false,
    title: 'Freeport',
    backgroundColor: '#FAFAF9',
    // macOS：隐藏标题栏但保留红绿灯，窗口更像原生应用
    ...(isMac
      ? {
          titleBarStyle: 'hiddenInset',
          trafficLightPosition: { x: 18, y: 18 },
          vibrancy: undefined,
        }
      : { autoHideMenuBar: true }),
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: false,
    },
  });

  if (saved.maximized) win.maximize();

  win.once('ready-to-show', () => {
    if (!SMOKE) win.show();
    if (isMac) app.dock?.show();
  });

  win.on('resize', saveBounds);
  win.on('move', saveBounds);
  win.on('close', (e) => {
    saveBounds();
    // 关闭 = 收进托盘，真正退出走 Cmd+Q / 托盘菜单
    if (!quitting && !SMOKE) {
      e.preventDefault();
      win.hide();
      if (isMac) app.dock?.hide();
    }
  });
  win.on('closed', () => {
    win = null;
  });

  // 外链一律交给系统浏览器，绝不在应用内开新窗口
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:/i.test(url)) shell.openExternal(url);
    return { action: 'deny' };
  });
  win.webContents.on('will-navigate', (e, url) => {
    const allowed = isDev ? url.startsWith(DEV_URL) : url.startsWith('file://');
    if (!allowed) {
      e.preventDefault();
      if (/^https?:/i.test(url)) shell.openExternal(url);
    }
  });

  win.webContents.on('did-fail-load', (_e, code, desc, url) => {
    console.error('[freeport] load failed', code, desc, url);
  });

  if (isDev) {
    win.loadURL(DEV_URL);
    if (!SMOKE) win.webContents.openDevTools({ mode: 'detach' });
  } else {
    win.loadFile(path.join(__dirname, '..', 'dist', 'index.html'));
  }

  if (SMOKE) {
    win.webContents.once('did-finish-load', async () => {
      await new Promise((r) => setTimeout(r, 2200));
      win.showInactive();
      await new Promise((r) => setTimeout(r, 600));
      const img = await win.webContents.capturePage();
      fs.writeFileSync(SMOKE_OUT, img.toPNG());
      console.log('SMOKE_OK', SMOKE_OUT);
      quitting = true;
      app.exit(0);
    });
  }

  return win;
}

function showWindow() {
  if (!win) {
    createWindow();
    return;
  }
  if (win.isMinimized()) win.restore();
  win.show();
  win.focus();
  if (isMac) app.dock?.show();
}

function toggleWindow() {
  if (!win) return createWindow();
  if (win.isVisible() && !win.isMinimized()) {
    win.hide();
    if (isMac) app.dock?.hide();
  } else {
    showWindow();
  }
}

/* ── 托盘 ─────────────────────────────────────── */
function buildTray() {
  let img = nativeImage.createFromPath(assetPath('trayTemplate.png'));
  if (img.isEmpty()) {
    // 资源缺失时退化为 1x1 透明图，避免托盘初始化直接抛错
    img = nativeImage.createEmpty();
  }
  if (isMac) img.setTemplateImage(true);

  tray = new Tray(img.resize({ width: 18, height: 18 }));
  tray.setToolTip('Freeport · 网关运行中');

  const menu = Menu.buildFromTemplate([
    { label: '打开 Freeport', click: showWindow },
    { type: 'separator' },
    {
      label: '复制网关地址',
      click: () => {
        require('electron').clipboard.writeText('http://127.0.0.1:8787/v1');
      },
    },
    {
      label: '复制 API Key',
      click: () => {
        require('electron').clipboard.writeText('fp-local-0000');
      },
    },
    { type: 'separator' },
    {
      label: '开机自启',
      type: 'checkbox',
      checked: app.getLoginItemSettings().openAtLogin,
      click: (item) => {
        app.setLoginItemSettings({ openAtLogin: item.checked, openAsHidden: true });
      },
    },
    { label: '打开日志文件夹', click: () => shell.openPath(app.getPath('userData')) },
    { type: 'separator' },
    {
      label: '退出 Freeport',
      accelerator: isMac ? 'Command+Q' : undefined,
      click: () => {
        quitting = true;
        app.quit();
      },
    },
  ]);

  tray.setContextMenu(menu);
  tray.on('click', toggleWindow);
  tray.on('double-click', showWindow);
}

/* ── 应用菜单（macOS 需要，否则连 Cmd+Q 都没有） ── */
function buildAppMenu() {
  const template = [
    ...(isMac
      ? [
          {
            label: 'Freeport',
            submenu: [
              { role: 'about', label: '关于 Freeport' },
              { type: 'separator' },
              { role: 'hide', label: '隐藏 Freeport' },
              { role: 'hideOthers', label: '隐藏其他' },
              { type: 'separator' },
              {
                label: '退出 Freeport',
                accelerator: 'Command+Q',
                click: () => {
                  quitting = true;
                  app.quit();
                },
              },
            ],
          },
        ]
      : []),
    {
      label: '编辑',
      submenu: [
        { role: 'undo', label: '撤销' },
        { role: 'redo', label: '重做' },
        { type: 'separator' },
        { role: 'cut', label: '剪切' },
        { role: 'copy', label: '复制' },
        { role: 'paste', label: '粘贴' },
        { role: 'selectAll', label: '全选' },
      ],
    },
    {
      label: '视图',
      submenu: [
        { role: 'reload', label: '重新载入' },
        { role: 'toggleDevTools', label: '开发者工具' },
        { type: 'separator' },
        { role: 'resetZoom', label: '实际大小' },
        { role: 'zoomIn', label: '放大' },
        { role: 'zoomOut', label: '缩小' },
        { type: 'separator' },
        { role: 'togglefullscreen', label: '全屏' },
      ],
    },
    {
      label: '窗口',
      submenu: [
        { role: 'minimize', label: '最小化' },
        { role: 'zoom', label: '缩放' },
        { type: 'separator' },
        { role: 'front', label: '前置全部窗口' },
      ],
    },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

/* ── IPC ──────────────────────────────────────── */
function registerIpc() {
  ipcMain.handle('app:info', () => ({
    version: app.getVersion(),
    name: 'Freeport',
    platform: process.platform,
    electron: process.versions.electron,
    chrome: process.versions.chrome,
    node: process.versions.node,
    dev: isDev,
    userData: app.getPath('userData'),
  }));

  // v0.1 占位：网关进程在 M1 接入后由这里返回真实端口与状态
  ipcMain.handle('gateway:status', () => ({
    running: true,
    port: 8787,
    bind: '127.0.0.1',
    since: new Date().toISOString(),
    requests24h: 1284,
  }));

  ipcMain.handle('shell:open-external', (_e, url) => {
    if (typeof url === 'string' && /^https?:/i.test(url)) return shell.openExternal(url);
    return false;
  });

  ipcMain.handle('win:minimize', () => win?.minimize());
  ipcMain.handle('win:toggle-maximize', () => {
    if (!win) return false;
    if (win.isMaximized()) win.unmaximize();
    else win.maximize();
    return win.isMaximized();
  });
  ipcMain.handle('win:close', () => win?.close());
  ipcMain.handle('win:toggle-devtools', () => win?.webContents.toggleDevTools());
}

/* ── 生命周期 ─────────────────────────────────── */
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', showWindow);

  app.setAboutPanelOptions({
    applicationName: 'Freeport',
    applicationVersion: app.getVersion(),
    copyright: '本地免费大模型聚合网关',
  });

  app.whenReady().then(() => {
    registerIpc();
    buildAppMenu();
    createWindow();
    buildTray();

    app.on('activate', () => {
      if (BrowserWindow.getAllWindows().length === 0) createWindow();
      else showWindow();
    });
  });

  app.on('before-quit', () => {
    quitting = true;
    saveBounds();
  });

  // 托盘常驻：关掉所有窗口≠退出（macOS 与 Windows 行为一致）
  app.on('window-all-closed', () => {
    if (isMac) return;
    if (!quitting) return;
    app.quit();
  });
}
