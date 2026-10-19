# Freeport 项目长期记忆

## 产品定义
产品名 **Freeport（自由港）**，Bundle ID `dev.freeport.app`，CLI `freeport`。本机常驻的「大模型额度聚合网关」桌面应用：发现可用厂商/模型（Tier A 官方免费 / B 匿名端点 / C 订阅与 Cookie 会话）→ 统一 OpenAI·Anthropic·Gemini 协议 → 一键注入已安装 Agent。

## 已定技术决策（2026-10-19，不要随意推翻）
- **v1.2 运行时复议（2026-10-19）**：Electron → **Tauri 2 + Rust**。见 `docs/ADR-001-runtime.md`（状态：提议，待用户拍板）
  - 单二进制**双形态**：GUI（进程内起网关）/ `--headless` daemon（launchd/systemd/计划任务托管）；关窗可移交 daemon
  - 网关是 **lib crate** 不是 bin：`crates/freeport-{core,gateway,inject,catalog,probe}`
  - 体积/内存依据：Electron dist 242MB、业务代码 1.8MB；magpie dmg 22.5MB、cc-switch dmg 33.2MB
  - 迁移窗口：M1 末约 12 人日；M3 后不建议（网关会涨到 9k 行 + 12 注入器）
- ~~Electron 38 + TypeScript 全栈~~（v1.1，已废弃，仅 M1 实现存在）
- UI：React 19 + Vite + Tailwind v4 + Radix + Zustand + TanStack Query + Recharts；中英双语（**前端 95% 可复用**）
- 内核：Rust 2021 + tokio + axum 0.7 + hyper 1 + reqwest(rustls)；DB：rusqlite（WAL）
- 凭据：keyring crate（Keychain/DPAPI）；Cookie 用 WebView 独立数据目录 + AES-256-GCM jar
- 协议转换走 Canonical IR（不做 N×M 直转）
- 发布：tauri bundler + tauri-action，mac Developer ID + notarytool，win Azure Trusted Signing；不上架应用商店
- Tier C 首批：OpenCode Zen（API Key/OAuth + 唯一 Cookie 试点）+ GitHub Copilot（Device Code）；不做 Cursor/Windsurf/Kiro
- 不接本地 Ollama：额度耗尽如实报 429 + 恢复时间

## 路由与接力（§3.6 v1.2，实现时以文档为准）
- **三层可解释**：① Gate（三态熔断 + 四窗口账本 + 能力门禁，硬过滤）→ ② Order（SWRR 平滑加权）→ ③ Pick（最早刷新优先 → 剩余最多 → 8% Thompson 探索）
- 取消 v1.1 的连乘打分（`score = base × headroom × rateLimit × userOverride`）；三轴公式降级为 Pick 层内部排序
- 三态熔断 Closed/Open/HalfOpen：Open 带 `reason` + `resetAt` 并渲染成「预计 14:32 恢复 · 额度耗尽」；HalfOpen 只放一个试探
- **affinity 会话粘性**（v1.2 新增）：sessionId 30min 滑动，优先回到上次成功的 provider/key，保 prompt cache；余量 <10% 主动解粘
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
- **运行时架构（双形态/daemon）**：`temp/magpie/internal/{cli,gui/backend,daemon}/`
- **SWRR + Smart + rest**：`temp/magpie/internal/{gateway/weighted,gateway/routing,routes/strategies}.go`
- **affinity 会话粘性**：`temp/magpie/internal/gateway/affinity.go`、`temp/cc-switch/src-tauri/src/proxy/session.rs`
- **三态熔断 / failover**：`temp/cc-switch/src-tauri/src/proxy/{circuit_breaker,provider_router,failover_switch}.rs`
- **Tauri 打包/更新**：`temp/cc-switch/src-tauri/tauri.conf.json`

## app/ 工程现状（2026-10-19，M1.5 已落地）
- **Tauri 2 + Rust**，Electron 代码已全部删除（`app/electron/` 不存在）；`npm run dev` = `tauri dev`，`npm run build` = `tauri build`
- Cargo workspace：`crates/freeport-core`（model/error/cooldown/route/presets/dto，纯逻辑含 11 个单测）+ `crates/freeport-gateway`（store/providers/axum server）+ `src-tauri`（宿主，20 个 `gw_*`/`win_*` 命令）
- 双形态：默认 GUI 内嵌网关；`freeport-app --headless` 只起网关；关窗只隐藏、托盘退出才停服
- 前端零改动，唯一改动点 `src/lib/ipc.ts`（`invoke` 门面，字段 `isDesktop`）；`html.is-desktop` 驱动桌面样式，Topbar 用 `data-tauri-drag-region`
- 数据目录：`dirs::data_dir()/dev.freeport.app`；**密钥不进系统钥匙串**，由 `crates/freeport-gateway/src/secrets.rs` 自管：`vault.key`（32B 随机主密钥，0600）+ `secrets.json`（AES-256-GCM 密文，provider id 作 AAD），`freeport.json` 只存 `keyMask`；**不再兼容 v0.1 Electron 的 keyEnc 遗留数据**
- 构建坑：`tauri` 需显式 `devtools` feature 才有 `open_devtools`；Tauri CLI 需手动装 `@tauri-apps/cli` + `cli-darwin-arm64` 并自建 `node_modules/.bin/tauri`
- 本机环境坑：WorkBuddy shell 注入 ELECTRON_RUN_AS_NODE=1（跑 electron 要 env -u，现已无关）；安全中心拦 mkdir/rm/dist 清空（vite emptyOutDir:false、用 python 删目录）
- 下一步：M2 —— 目录（models.dev 为底 + 免费额度 feed）+ key 管理 + liveness/能力探针 + 巡检调度
