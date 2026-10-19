# Freeport 项目长期记忆

## 产品定义
产品名 **Freeport（自由港）**，Bundle ID `dev.freeport.app`，CLI `freeport`。本机常驻的「大模型额度聚合网关」桌面应用：发现可用厂商/模型（Tier A 官方免费 / B 匿名端点 / C 订阅与 Cookie 会话）→ 统一 OpenAI·Anthropic·Gemini 协议 → 一键注入已安装 Agent。

## 已定技术决策（2026-10-19，不要随意推翻）
- Electron 38 + TypeScript 全栈；Hono 网关跑在主进程，仅监听 127.0.0.1（不做局域网共享）
- UI：React 19 + Vite + Tailwind v4 + Radix + Zustand + TanStack Query + Recharts；中英双语
- DB：better-sqlite3（WAL，主进程单例）
- 凭据：keytar（Keychain/DPAPI）；Cookie 用 Electron `persist:` partition + AES-256-GCM jar
- 协议转换走 Canonical IR（不做 N×M 直转）
- 发布：electron-builder，mac Developer ID + notarytool，win Azure Trusted Signing；不上架应用商店
- Tier C 首批：OpenCode Zen（API Key/OAuth + 唯一 Cookie 试点）+ GitHub Copilot（Device Code）；不做 Cursor/Windsurf/Kiro
- 不接本地 Ollama：额度耗尽如实报 429 + 恢复时间

## 路由与接力（§3.6，实现时以文档为准）
- score = (wR·R + wS·S + wI·I) × headroom × rateLimit × userOverride；权重和恒为 1
- 失败先判作用域（key / model / platform），再决定冷却与惩罚；5xx 只在本请求内跳平台
- 45s 墙钟预算，attempt 0/1 必跑，首字节即 disarmHedge，连续 8 次失败熔断
- 冷却阶梯 2m/10m/1h/24h；heuristic 冷却可被探针提前解除（≥50% & ≥60s）
- lease 解决并发 check-then-act；学限额只收紧；耗尽状态码诚实，永不返回 500

## 巡检与目录运营（§3.4.1 / §3.5.1 / §3.5.2）
- 发现 = 五源（上游 /models、OpenRouter 免费雷达、models.dev、厂商 changelog、社区清单）+ 候选池；社区清单只作线索，必须实测
- 巡检两级：`liveness`（3–10 token，高频）与 `capability`（50–500 token，入库/版本/手动）；**每日体检只跑 liveness**
- 探针预算 `clamp(日配额×1%, 60, 800)`；真实流量优先；预算耗尽只探路由链前 20 个模型
- 健康状态机 unknown→healthy→degraded→cooling→unavailable→retired；**连续 2 次失败**才判 unavailable，单次失败不改状态
- 目录 feed：Ed25519 签名 + JCS + sha256 + RFC6902 patch + `?since=seq`；三策略 auto/notify/pinned
- 新模型必须 canary：0.3 倍权重 → 连续 20 次成功 → 转正；单次删除 >15% 自动熔断
- chat2api = 账号池 + 逆向图片 sidecar，**不进主链路**；v1 走自定义 OpenAI 兼容端点，v1.5 走 Extension Host

## 商业化
开源核心 + Pro（¥29/月、¥199/年）：免费版功能完整；Pro 卖实时目录、额度情报、多机配置同步。license 用 Ed25519 签名、离线可验、失效只降级不罢工。不做卖 key / 转售额度 / 按 token 计费。

## 不可违背的产品红线
- 不代注册账号、不共享账号、不绕过付费墙、不伪造设备指纹
- 不读取系统浏览器 Cookie 库（只用内置登录窗口）
- OAuth 不内嵌 client_secret，只用 public client + PKCE
- 免费判定只看价格字段为 0 + 目录显式标注，禁止用名称含 "free" 判断
- 注入只改自己写入的节点，必须备份、幂等、可回滚、退出可清理

## 参考项目坐标
- 路由/配额/失败分类：`temp/freellmapi/server/src/services/`
- provider 注册表（138 个）：`temp/9router/open-sse/providers/registry/`
- 协议转换：`temp/9router/open-sse/translator/`
- Agent 注入（结构化合并）：`temp/9router/cli/src/cli/commands/connectTools.js`
- Agent 注入（托管块/卸载）：`temp/router/install/install.sh`
- Electron 内嵌 server + 托盘：`temp/freellmapi/desktop/src/`
- 最小完整转发 + 生命周期清理：`temp/RuiC-oc-ferry/src/`

## app/ 工程现状（2026-10-19）
- Electron 33.4.11 + Vite 6 + React 18 + zustand；`npm run dev`（vite+electron 并起）/ `npm run build && npm start`
- 主进程 `electron/main.cjs`：hiddenInset、托盘、单实例、外链拦截、窗口位置记忆、FP_SMOKE 自检（capturePage 截图退出）
- 渲染层 10 页全通，tsc 0 错误，dev 与 file:// 两种模式冒烟截图验证过
- 本机环境坑：WorkBuddy shell 注入 ELECTRON_RUN_AS_NODE=1（跑 electron 要 env -u）；安全中心拦 mkdir/rm/dist 清空（vite emptyOutDir:false、python 建目录）；electron 二进制手动装（npmmirror zip + scripts/unzip-electron.py 重建 symlink）
- 下一步：M1 —— Hono 网关进主进程 :8787，/v1/chat/completions 打通 Groq
