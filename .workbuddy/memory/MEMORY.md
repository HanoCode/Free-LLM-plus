# Freeport 项目长期记忆

## 产品
**Freeport（自由港）**，Bundle ID `dev.freeport.app`，CLI `freeport`。本机常驻的大模型额度聚合网关桌面应用：发现可用厂商/模型（Tier A 官方免费 / B 匿名端点 / C 订阅与 Cookie）→ 统一 OpenAI·Anthropic·Gemini → 一键注入已安装 Agent。

## 技术栈（已定，勿翻）
- **Tauri 2 + Rust**（v1.2 复议废掉 Electron）。单二进制双形态：GUI 内嵌网关 / `--headless` daemon；关窗只隐藏，托盘退出才停服。ADR 见 `docs/ADR-001-runtime.md`
- Cargo workspace：`freeport-{core,gateway,inject,catalog,probe,auth,license}` + `src-tauri`（20+ `gw_*`/`win_*` 命令）；网关是 lib 不是 bin
- UI：React 19 + Vite + Tailwind v4 + Radix + Zustand + TanStack Query + Recharts，中英双语；前端基本零改动，唯一门面 `src/lib/ipc.ts`（`isDesktop`），`html.is-desktop` + `data-tauri-drag-region`
- **桥必须先于 store 求值**：`gateway.ts` 的 `available: !!bridge()` 是模块级常量。`ipc.ts` 末尾自举 `installTauriBridge()` + `gateway.ts` 顶部 `import './ipc'` + `main.tsx` 第一行同样导入；否则桌面端把自己当浏览器（安静地显示错的数据，不报错）
- **前端零模拟数据**（红线）：`src/lib/data.ts` 已删。空态就用 `components/ui.tsx` 的 `Empty` 说「没有」；没接后端的开关一律 `未接入` 徽标 + 写明当前真实行为，不做点了没反应的假按钮
- Rust 2021 + tokio + axum 0.7 + hyper 1 + reqwest(rustls)；rusqlite(WAL)；协议转换走 Canonical IR（不做 N×M 直转）
- 发布：tauri bundler + tauri-action，mac Developer ID + notarytool，win Azure Trusted Signing；不上应用商店
- 数据目录 `dirs::data_dir()/dev.freeport.app`；**密钥不进钥匙串**：`secrets.rs` 自管 `vault.key`(32B,0600) + `secrets.json`(AES-256-GCM，account id 作 AAD)，配置只存 `keyMask`
- 构建坑：`tauri` 要显式 `devtools` feature；Tauri CLI 需手装 `@tauri-apps/cli` + `cli-darwin-arm64` 并自建 `node_modules/.bin/tauri`；本机安全中心拦 mkdir/rm（用 python 删目录、`emptyOutDir:false`）

## 路由与接力
- 三层可解释：① Gate（三态熔断 + 四窗口账本 + 能力门禁，硬过滤）→ ② Order（SWRR）→ ③ Pick（最早刷新 → 剩余最多 → 8% Thompson）
- 权重 = **tier(A1.0/B0.85/C0.6) × 余量(0.2+0.8·remaining) × 健康(healthy1.0/unknown0.95/degraded0.55/cooling0.3)**，三因子相乘可逐个解释；v1.1 连乘打分子公式已废
- 能力门禁三态：`Some(false)` 排除 / `None` 放行但 ×0.6 / `Some(true)` 正常（未知≠不支持）；400 自愈链：classify 给 capability → `note_fail` 写 `model_health.caps` → 下一单 Gate 拦
- 熔断 Closed/Open/HalfOpen，Open 带 `reason`+`resetAt`（渲染「预计 14:32 恢复 · 额度耗尽」），HalfOpen 只放一个试探；连续 8 次失败熔断
- **affinity 粘性**：会话键 = `x-freeport-session`/`x-session-id`，缺省 `sha(model+system+首条 user)` 指纹；命中则**跳过 SWRR**（否则轮转被拉偏），`stick_to_front` 返回值是「命中」不是「顺序变了」；余量 <10% 主动解粘
- `/v1/route?model=&session=&caps=` 返回 chat/weights/rejected/sticky，调试与真实请求共用 `build_plan(_opts)`
- 失败先判作用域（key / model / platform）；冷却键三级 `k:account` / `m:provider:model` / `p:provider`；5xx 只在本请求内跳平台；跳过集 `skipped:(provider_idx, account, model)`，**key 级失败只跳该凭据**
- 冷却阶梯 2m/10m/1h/24h；heuristic 冷却可被探针提前解除（≥50% & ≥60s）；lease 解决并发 check-then-act
- 45s 墙钟，attempt 0/1 必跑，首个有效语义块 disarmHedge；`hold.rs` 只有 `holdIdleMs`/`holdTotalMs` 两参（无 soft/hard/keepAlive 三参）
- 429 三分法：`limit:0` 无免费档 → 24h 移出；日额度用完 → 按 `retryDelay`；分钟限流 → 90s。`parse_quota_signal()`；**`free tier exhausted` 不算 no_free_tier**（有单测锁）
- 账本 `QuotaWindow{kind,used_user,used_probe,limit,reset_at,source}`，token/请求双记、分维度独立耗尽；RPD 耗尽拦用户但放行 liveness；`Ledger::learn` **只收紧**（先读旧值再 slot）；reserve 用 `input_est+max_tokens`，settle 按实值下调

## 多账号与凭据
- `Provider.accounts: Vec<Account{id,name,key_mask,enabled,added_at,quota,health}>`，`Provider.quota` 仅作旧配置迁移源；候选粒度 `(provider,account,model)`
- 迁移：`migrate_accounts` 让凭据 id **沿用 provider id**（secrets.json 不动）；老 `authToken` 迁成 `api_keys[0]=key_default`
- `Provider::sync_health()`：还有一条 healthy 就 healthy，全挂才取最近 error，没测过保持 unknown；`probe_account` 只写该凭据
- 命令：`gw_add_account` / `gw_remove_account` / `gw_patch_account` / `gw_probe_account`；`GwWindow`/`GwQuota` 必须 `rename_all="camelCase"`（否则前端额度条 NaN）

## 三类凭据形态（key / oauth / cookie）
- 密钥库三个槽位：`{account}`（key）、`{account}#oauth`、`{account}#cookie`；AAD 带槽位后缀，密文不可互换；`has_credential` = 任一可用（cookie 需**未全过期**）
- `gateway/src/cred.rs`：`Credential::{Bearer, Cookie{account_id, jar}}`，`load()` 优先级 **oauth > cookie > key**；cookie 全过期自动退回 key
- `providers::{list_models, create_chat}` 收 `&Credential`；**Cookie 时绝不带 Authorization**（会被真实站点判 401，还暴露一把它不认识的 token）
- `probe` 签名是 `AuthHeader<'a> = (&str, &str)`，只管「哪个头、什么值」，加新凭据形态不用再改探针
- `freeport-auth/src/cookie.rs`：RFC 6265 子集（域必须点对齐、path 必须斜杠对齐、secure 只走 https、path 长的先发、CR/LF 直接拒、absorb 学轮换）
- 会话**不自动续期**（模拟浏览行为=绕风控），401/403 文案是「web 会话已失效，请重新粘贴会话」
- Cookie 只做 OpenCode Zen 一家试点：前端 `COOKIE_PROVIDERS` 白名单，预置厂商 id `opencode`（`https://opencode.ai/zen/v1`）
- 写对 127.0.0.1 假上游的测试**必须用 `providers::client()`**：`reqwest::Client::new()` 会读 `HTTP_PROXY`，把回环当远端，请求永不返回
- **写锁必须困在 `{}` 里**：`let mut d = data.write().await; ... Ok(self.providers().await)` 会在同一任务里自锁（providers 要读锁），进程表现为「无声卡死」而非报错
- 设备码真机已验通（Copilot / HanoCode）：899s 设备码 → 402 字符 token + `ghu_` refresh → 48 模型；**续期也真验过**（拨 `expiresAt` 到过去 → `Broker::token` 换新 → 新 token 拉模型仍成功）

## 配置持久化（store）
- `store::save` 返回 `bool`，失败 `tracing::error`；**不许 `let _ =` 吞 rename 错**——保存失败会变成静默丢配置
- 原子写 = tmp + `sync_all` + rename；**load 要比 mtime**：主文件合法但比 `freeport.json.tmp` 旧时用 tmp。只看「能否解析」会把旧配置当真相，还顺手覆盖掉 tmp（真机踩过，登录元数据丢一轮）
- tmp 只在「完整写完才 rename」，所以能做兜底；半截 tmp 解析失败自然被排除；恢复后立刻 `save_to` 固化，tmp 不该成为长期状态
- `load_from(dir)` / `save_to(dir, data)` 是给单测的入口，别用 `FREEPORT_DATA_DIR`（进程级，测试互相踩）
- 密钥库与配置可能不同步：`secrets.json` 有 `#oauth` 但没有 account 条目时用 `examples/repair_oauth.rs` 收养（回滚 / 迁移只带密文 / 保存失败都会造成孤儿）

## 协议与注入
- 已实现：`/v1/chat/completions`、`/v1/messages`(anthropic)、`/v1beta`(gemini)、`/api/*`(ollama NDJSON)、`/v1/responses`（不实现 `previous_response_id`、丢弃 hosted tools）；Codex 默认仍 `wire_api="chat"`
- `/v1/models` 双形状（OpenAI list + Claude 形状）+ 别名行 + `?limit=` + `GET /v1/models/:id`；`ALIAS_PREFIX` / `aliases()` / `resolve_alias()`
- 签名封装 `sig.rs`：`fp1:` + base64url 无状态，`thought_signature` ↔ `extra_content.google` 跨协议回放
- `freeport-inject`：`spec.rs` 静态表，两个正交维度 `Discovery{SelfServe,Pull,Push,Host}` × `Strategy{JsonKeys,TomlBlock,Guide,Push}`；`push.rs` 写 VS Code Chat 形状（按标记识别/整组替换）；`sweep()` 清扫、`revert` 支持整份快照
- 每个 Agent 一把 `agent:<id>` key；`Prune` 规则进 `AgentSpec.prune`
- **项目级 scope**：`Scope::{User,Project(root)}`；落点在 `spec::PROJECT_TARGETS` 侧表（不是 `AgentSpec` 字段），v1 只有 claude-code → `.claude/settings.local.json`。铁律「只写客户端自己认定的个人本地文件」——`opencode.json`/`.crush.json` 就是要提交的那份，明确拒绝
- 日志键 `id@<仓库根>` 区分 scope；备份名必须用 `id-<目录名>-<hash>`（直接用日志键会把斜杠写进 backups/）；`Entry.created` 决定回滚是删文件还是写回
- 项目级与用户级共用同一把 key：注入不轮换、撤出时用户级还在用就不吊销
- VS Code Chat 认领靠 `freeportVendor == push::MARK`（"freeport"），`vendor` 是 `customendpoint`；组级**不能有 url**（有则转自发现分支）
- e2e 覆盖闸：`list()` 里 `strategy != "guide"` 的 id 必须等于测试真跑过的那份清单，新增适配器不补测试就挂
- 日志双归因：`LogEntry.path`（内部头 `x-freeport-path`）、`key_id/key_name`、`account_id/account_name`（存快照）；`Attempt.account`

## 巡检与目录
- 发现五源（上游 /models、OpenRouter 免费雷达、models.dev、厂商 changelog、社区清单）+ 候选池；社区清单只作线索必须实测
- 巡检两级 liveness(3–10 token，每日只跑这个) / capability(50–500 token)；预算 `clamp(日配额×1%,60,800)`，真实流量优先，耗尽只探前 20
- 健康状态机 unknown→healthy→degraded→cooling→unavailable→retired，**连续 2 次失败**才判 unavailable
- 目录 feed：Ed25519 + JCS + sha256 + `?since=seq`，三策略 auto/notify/pinned；新模型 canary 0.3 权重 → 20 次成功转正；单次删除 >15% 熔断
- **增量补丁链**：`Manifest.patches` 整组进签名（空数组不进，老 manifest 不受影响）；`PatchFile{fromSeq,toSeq,ops,sha256After}`，patch 文件 sha 钉在 manifest 里 → 补丁不必单独签名
- 补丁只做 `add/remove/replace/test`，**不做 move/copy**（等于给补丁读整份文档的能力）
- `sha256After` 不能用 JCS（快照含浮点价格），用 `sha256(to_vec(&Snapshot))`：字段顺序由结构体定，语义同则字节同
- 增量前提：本地 `snapshot.json` 的 seq == `since` 且 `plan_chain` 连续走到 manifest.seq（≤24 环）；任一不满足走全量。**链上任何失败都回退全量**，增量只省流量不是真相来源
- 每环强制写 `seq = toSeq`，不指望发布端在 ops 里写
- 发布端 `diff()` 带自检（套不回就整体 replace），数组按 `id` 匹配；顺序变化表达不了，自检会退化
- 真机：32.8KB 快照改 3 处 → 补丁 445B（1.4%）；删掉全量只留补丁仍能更新到 seq 2
- chat2api = 账号池 + 逆向图片 sidecar，**不进主链路**

## 商业化 / 迁移 / License
- 开源核心 + Pro（¥29/月、¥199/年）：免费版功能完整；Pro 卖实时目录、额度情报、多机同步。**红线：网关里没有 `if !is_pro()`**，Pro 只影响服务端给什么数据
- Pro 数据面身份：feed 请求带 `x-freeport-tier`（free/pro，宽限期算 pro）+ 仅 Pro 带 `Authorization: Bearer <JWT>`；**Bearer 只在 https 或本机回环发，且快照须与 manifest 同源**；manifest 的 `tier` 进签名载荷；`servedTier` 只做展示。客户端只验签、不判档
- `freeport-license`：标准 JWT `alg=EdDSA`，状态 missing/valid/grace/expired/invalid/wrong_machine，7 天宽限仍算 Pro，每次现算；`tier` 与 `licensedTier` 分开；`is_pro()` 直判（走 `tier()` 会递归）；私钥只在 `--features issuer`，发布前必须重新 keygen 换 `PROD_PUBKEY`
- `transfer.rs` 迁移码：Argon2id(19MiB,t2,p1) → AES-256-GCM，AAD=格式|版本|kdf|m|t|p|salt|nonce，解密前校验 m≤256MiB/t≤16/p≤8；搬 providers/accounts/secrets/api_keys(key_hash)/routing/patrol，不搬台账/冷却/健康/目录；`merge` 只补缺失，`replace` 先删多余密文

## 产品红线
不代注册、不共享账号、不绕付费墙、不伪造设备指纹；不读系统浏览器 Cookie 库；OAuth 只用 public client + PKCE；免费判定只看价格字段为 0 + 目录显式标注（禁止看名称含 "free"）；注入只改自己写入的节点，必须备份/幂等/可回滚/退出可清理；不接本地 Ollama，耗尽如实报 429 + 恢复时间

## 参考坐标（temp/）
路由配额失败分类 `freellmapi/server/src/services/`；provider 注册表 `9router/open-sse/providers/registry/`；协议转换 `9router/open-sse/translator/`；注入 `9router/cli/src/cli/commands/connectTools.js`、`router/install/install.sh`；最小转发 `RuiC-oc-ferry/src/`；双形态 daemon `magpie/internal/{cli,gui/backend,daemon}/`；SWRR `magpie/internal/gateway/weighted|routing`、`routes/strategies.go`；affinity `magpie/internal/gateway/affinity.go`、`cc-switch/src-tauri/src/proxy/session.rs`；熔断 `cc-switch/.../proxy/{circuit_breaker,provider_router,failover_switch}.rs`；打包 `cc-switch/src-tauri/tauri.conf.json`
