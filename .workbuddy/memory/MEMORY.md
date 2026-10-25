# Freeport 项目长期记忆

## 产品
**Freeport（自由港）**，`dev.freeport.app`，CLI `freeport`。本机大模型额度聚合网关桌面应用：发现免费厂商/模型 → 统一 OpenAI·Anthropic·Gemini·Ollama 协议 → 一键注入已装 Agent。详细踩坑史见 .workbuddy/memory/ 各日日志。

## 技术栈（已定勿翻）
- Tauri 2 + Rust（非 Electron）；单二进制双形态：GUI 内嵌网关 / `--headless`；托盘退出才停服。ADR `docs/ADR-001-runtime.md`
- workspace：`freeport-{core,gateway,inject,catalog,probe,auth,license,ext,mcp}` + `src-tauri`；网关是 lib
- UI：React 19 + Vite + Tailwind v4 + Radix + Zustand；唯一门面 `src/lib/ipc.ts`
- 桥必须先于 store 求值（`gateway.ts` 的 `available: !!bridge()` 是模块级常量，导入顺序错会安静显示错数据）
- 前端零模拟数据（红线）：没接后端的开关标「未接入」徽标

## 路由核心
- 三层：Gate（熔断+四窗口账本+能力门禁+用途池）→ Order（SWRR）→ Pick（最早刷新→剩余最多→8% Thompson）
- 权重 = tier × 余量 × 健康；失败作用域分 key/model/platform；冷却=类别基准×2^(hits-1) 封顶 24h
- affinity：会话键命中跳过 SWRR；余量<10% 解粘
- 429 三分法：无免费档→24h 移出；日额度→按 retryDelay；分钟限流→90s
- 账本 `Ledger::learn` 只收紧；reserve 用 input_est+max_tokens；`IdemStore` 512条/24h/不落盘
- 策略八档（v1.10）：balanced/order/rotate/usage/pace/fastest/stablest/smartest；三维权重只作用 balanced，没测过速度给 0.6

## 凭据与分池
- 候选粒度 (provider,account,model)；`Purpose{chat,coding,vision,probe}`，`purposes` 空=不限
- 凭据三槽位 `{account}`/#oauth/#cookie，AAD 带槽位；Cookie 时绝不带 Authorization，不自动续期
- 判「接没接厂商」查账号别查 provider.id

## 配置与密钥坑
- 密钥不进钥匙串：`vault.key`(0600)+`secrets.json`(AES-256-GCM)
- 落盘写实例目录（`save_here`）不是进程目录；`store::save` 返回 bool 不许吞错；load 要比 mtime
- 进程级 env 测试锁全 workspace 共用一把 `store::env_lock()`；写锁困在 `{}` 里（同任务自锁=无声卡死）
- 测试假上游必须用 `providers::client()`（`Client::new()` 读 HTTP_PROXY）

## 协议与注入
- 已实现：/v1/chat/completions、/v1/messages、/v1beta、/api/*(ollama)、/v1/responses（不做 previous_response_id）
- `freeport-inject`：Discovery×Strategy；项目级 scope 落点在 `spec::PROJECT_TARGETS` 侧表；备份名 `id-<目录名>-<hash>`
- e2e 覆盖闸：`list()` 非 guide 策略 id 必须等于测试清单

## 巡检与目录
- liveness(每日) / capability 探针；健康状态机连续 2 次失败判 unavailable，7 天 retire
- 目录 feed：Ed25519+JCS+sha256+`?since=seq`，增量补丁链 `PatchFile{fromSeq,toSeq,ops,sha256After}`；任何失败回退全量

## MCP / 视图
- `--mcp` stdio JSON-RPC 不起网关；6 个只读工具不吐 authToken；必须 `with_writer(stderr)`
- `/v1/freeport/inspect` 双闸=回环 peer + control.key；扩展 manifest 落盘必须 tmp+rename（并发读半截=静默消失）

## 商业化
- 开源核心 + Pro（¥29/月、¥199/年）；`is_pro()` 只经 `gateway::pro::allow()` 收口；路由核心永久零 gate
- Pro 本机功能：intel/reports/backup/巡检预算(免费 5 次/天，Pro clamp(配额×1%,60,2400))/策略档/粘性 TTL/日志环 300↔2000/远程接入(非回环强制 require_auth)
- `freeport-license`：JWT EdDSA，7 天宽限；私钥仅 `--features issuer`
- `transfer.rs` 迁移码：Argon2id+AES-GCM，不搬台账/冷却/健康/目录

## 产品红线
不代注册、不共享账号、不绕付费墙、不伪造设备指纹；不读系统浏览器 Cookie 库；OAuth 只 public client+PKCE；免费判定只看价格 0+目录标注；注入只改自己写入的节点且可回滚；不接本地 Ollama；chat2api 不进主链路

## 构建坑
- `tauri` 要 devtools feature；Tauri CLI 手装 + 自建 `node_modules/.bin/tauri`
- `vite build` 报 ECONNREFUSED broker.sock = 沙箱 fs shim 问题：`env -u NODE_OPTIONS -u CODEBUDDY_BROKERED_FS_HOOK_ENABLED ./node_modules/.bin/vite build`
- [Pro 授权加固与专有化](freeport-pro-hardening.md) — 2026-10-25 弃 AGPL 转专有；HWM/设备密钥/离线上限/feat 门禁四层防破解
