import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  // 打包后由 Tauri 以自定义协议加载，资源必须相对引用
  base: './',
  server: {
    host: '127.0.0.1',
    port: 5273,
    strictPort: false,
    open: false,
  },
  build: {
    outDir: 'dist',
    // 环境的安全中心会拦截整目录清空，改为覆盖写入
    emptyOutDir: false,
    sourcemap: true,
  },
});
