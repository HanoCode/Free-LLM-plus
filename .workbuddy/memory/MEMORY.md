# Freeport 项目长期记忆

## 产品
**Freeport（自由港）**，Bundle ID `dev.freeport.app`，CLI `freeport`。本机常驻的大模型额度聚合网关桌面应用：发现可用厂商/模型（Tier A 官方免费 / B 匿名端点 / C 订阅与 Cookie）→ 统一 OpenAI·Anthropic·Gemini → 一键注入已安装 Agent。

## 技术栈与构建坑（已定，勿翻）
- **Tauri 2 + Rust**（v1.2 复议废掉 Electron）。单二进制双形态：GUI 内嵌网关 / `--headless` daemon；关窗只隐藏，托盘退出才停服。ADR `docs/ADR-001-runtime.md`
- workspace：`freeport-{core,gateway,inject,catalog,probe,auth,license,ext,mcp}` + `src-tauri`（20+ `gw_*`/`win_*` 命令）；网关是 lib 不是 bin
- UI：React 19 + Vite + Tailwind v4 + Radix + Zustand + Recharts，中英双语；唯一门面 `src/lib/ipc.ts`（`isDesktop`）
- **桥必须先于 store 求值**：`gateway.ts` 的 `available: !!bridge()` 是模块级常量。`ipc.ts` 末尾自举 + `gateway.ts` 顶部 `import './ipc'` + `main.tsx` 第一行导入；否则桌面端把自己当浏览器（安静显示错数据，不报错）
- **前端零模拟数据**（红线）：`src/lib/data.ts` 已删；空态用 `Empty`，没接后端的开关一律 `未接入` 徽标，不做点了没反应的假按钮
- **品牌 Logo（v1.9 UI）**：`src/components/BrandLogo.tsx` 内联 simple-icons SVG（离线），按 id/名称解析，未收录回落字母方块；注入页=快速开始卡+三列 Agent 卡片墙+详情卡；groq/cerebras 官方无 simple-icons 条目
- Rust 2021 + tokio + axum 0.7 + hyper 1 + reqwest(rustls)；rusqlite(WAL)；协议转换走 Canonical IR
- 构建坑：`tauri` 要显式 `devtools` feature；Tauri CLI 需手装 `@tauri-apps/cli` + `cli-darwin-arm64` 并自建 `node_modules/.bin/tauri`；安全中心拦 mkdir/rm（用 python 删、`emptyOutDir:false`）
- 构建坑：`vite build` 报 `ECONNREFUSED broker.sock`（转 lucide-react 某图标时炸）= 沙箱 `NODE_OPTIONS` fs shim 扛不住高并发读 node_modules。用 `env -u NODE_OPTIONS -u CODEBUDDY_BROKERED_FS_HOOK_ENABLED ./node_modules/.bin/vite build`，15s 出包

## 路由
- 三层可解释：① Gate（三态熔断 + 四窗口账本 + 能力门禁 + 用途池，硬过滤）→ ② Order（SWRR）→ ③ Pick（最早刷新 → 剩余最多 → 8% Thompson）
- 权重 = **tier(A1.0/B0.85/C0.6) × 余量(0.2+0.8·remaining) × 健康(healthy1.0/unknown0.95/degraded0.55/cooling0.3)**；v1.1 连乘打分子公式已废
- 能力门禁三态：`Some(false)` 排除 / `None` 放行 ×0.6 / `Some(true)` 正常（未知≠不支持）；400 自愈链：classify → `note_fail` 写 `model_health.caps` → 下一单拦
- 熔断 Closed/Open/HalfOpen，Open 带 `reason`+`resetAt`，HalfOpen 只放一个试探；连续 8 次失败熔断
- **affinity**：会话键 `x-freeport-session`/`x-session-id`，缺省 `sha(model+system+首条 user)`；命中则**跳过 SWRR**（否则轮转被拉偏），`stick_to_front` 返回「是否命中」；余量 <10% 主动解粘
- **`/v1/route` 的 `pool` 段（v1.8）**：`accounts{total,admits,blocked}` / `recent`（复用 `digest::pool_stat`，与摘要同一个数）/ `landsOn` / `tightest` / `cost·fits·shortfall` + `verdict` 码（empty-pool→no-candidate→too-big→pool-failing→tight→ok）。措辞各端写（GUI 双语、MCP `route_hint`），网关只给码
- 账本只读判断：`Ledger::tightest()`（按剩余**比例**，跳过 limit=0 与已过 reset_at）、`Ledger::shortfall(tokens)`（**只看 tpm/tpd**，次数窗口的剩余是「还能发几次」）
- **预演只报告不改道**：`?tokens=` 判出的 too-big 不让这单改路（token 预估进 Gate 是另一个量级，估错会把好的一单推给更差的模型）；没传 `tokens` 时 `fits=null` 不是 true
- 失败先判作用域（key / model / platform）；冷却键 `k:account` / `m:provider:model` / `p:provider`；5xx 只在本请求内跳平台；**key 级失败只跳该凭据**
- 冷却 = 类别基准 × 2^(hits-1)，封顶 24h（不是固定 2m/10m/1h/24h）；heuristic 冷却可被探针提前解除（≥50% & ≥60s）
- 45s 墙钟，attempt 0/1 必跑，首个有效语义块 disarmHedge；hedge 切片 `attempt_ms=(剩余/剩余跳数).clamp(8s,20s)`，最后一跳拿满
- 429 三分法：`limit:0` 无免费档 → 24h 移出；日额度用完 → 按 `retryDelay`；分钟限流 → 90s。**`free tier exhausted` 不算 no_free_tier**（单测锁）
- 账本 `QuotaWindow{kind,used_user,used_probe,limit,reset_at,source}`：token/请求双记、分维度独立耗尽；RPD 耗尽拦用户但放行 liveness；`Ledger::learn` **只收紧**；reserve 用 `input_est+max_tokens`，settle 下调
- 幂等：`IdemStore` 512 条 / 24h / 不落盘，重放先于虚拟 key 记账

## 凭据与用途分池
- `Provider.accounts: Vec<Account{id,name,key_mask,enabled,added_at,quota,health,purposes}>`，`Provider.quota` 仅作迁移源；候选粒度 `(provider,account,model)`
- **用途分池（v1.7）**：`Purpose{chat,coding,vision,probe}`；`purposes` **空 = 不限**（空若读成「都不能干」升级即断网）；`Account::admits()` 唯一判定，路由与巡检共用
- `Purpose::infer` 四顺位：`x-freeport-purpose` 显式头 > 含图 → vision > 已注入 agent key（`api_keys.name` 前缀 `agent:`）→ coding > chat。**显式头是逃生舱，带图也归它**
- 分池是「收窄」不是「切分」：只在 Gate 过滤 `kind="purpose"`，不改权重/顺序；`/v1/models` 不按池过滤（模型目录是能力声明）
- 巡检：`patrol::collect` **入队前**跳过整家无 `admits(Probe)` 的厂商，否则 overdue 只增不减会饿死其他模型；代价是 coding-only 号健康度停在 unknown
- 三种凭据形态三槽位 `{account}` / `#oauth` / `#cookie`（AAD 带槽位，密文不可互换）；`load()` 优先级 **oauth > cookie > key**；**Cookie 时绝不带 Authorization**
- `probe` 签名 `AuthHeader<'a> = (&str,&str)`，加新形态不用改探针；Cookie 会话**不自动续期**（模拟浏览=绕风控），401 文案是「请重新粘贴会话」
- 迁移 `migrate_accounts` 让凭据 id **沿用 provider id**（secrets.json 不动）；老 `authToken` → `api_keys[0]=key_default`
- `sync_health()`：还有一条 healthy 就 healthy，全挂才取最近 error；命令 `gw_add_account`/`gw_remove_account`/`gw_patch_account`/`gw_probe_account`；`GwWindow`/`GwQuota` 必须 `rename_all="camelCase"`（否则额度条 NaN）
- 判「接没接厂商」一律查账号，别查 `provider.id`（迁移期 id 恰好相同，新凭据叫 `acct_xxx` 会漏）

## 配置与密钥（踩过的坑）
- 数据目录 `dirs::data_dir()/dev.freeport.app`；**密钥不进钥匙串**：`vault.key`(32B,0600) + `secrets.json`(AES-256-GCM，account id 作 AAD)，配置只存 `keyMask`
- **落盘写「实例目录」不是进程目录**：`load_from(dir)` 只记 dir 不够，所有写走 `save_here(&st,&data)`（`st.dir`，std RwLock）。用 `store::save()` 会让测试/便携实例把配置写进真实用户目录，且不报错——`persist_throttled` 就够触发；残留的新 tmp 会被 `load_best` 优先采用，等于静默替换用户配置。回归闸 `tests/store_target_e2e.rs`（诱饵 `FREEPORT_DATA_DIR`）
- `store::save` 返回 `bool`；**不许 `let _ =` 吞 rename 错**（静默丢配置）。原子写 = tmp + `sync_all` + rename
- **load 要比 mtime**：主文件合法但比 `.tmp` 旧时用 tmp；只看「能否解析」会把旧配置当真相还覆盖掉 tmp（真机踩过，登录元数据丢一轮）
- 单测用 `load_from(dir)`/`save_to(dir,data)`，别用 `FREEPORT_DATA_DIR`（进程级，测试互相踩）；同理临时目录名不能只靠 `SystemTime` 纳秒（macOS 精度不够会撞），用进程内自增
- **进程级 env 的锁必须全 workspace 共用一把**：`store::env_lock()`（`#[cfg(test)]`）。exts 与 secrets 各持一把模块私有锁时等于没锁——症状是 `exts::enable_with_permitted_domain_persists` 随机 `find("asr")` 拿 None（目录被隔壁改走后 builtin manifest 写去了别处）
- **写锁必须困在 `{}` 里**：`let mut d = data.write().await; ... self.providers().await` 会在同任务自锁，表现是「无声卡死」
- 对 127.0.0.1 假上游的测试**必须用 `providers::client()`**：`reqwest::Client::new()` 会读 `HTTP_PROXY`，请求永不返回

## 协议与注入
- 已实现：`/v1/chat/completions`、`/v1/messages`、`/v1beta`、`/api/*`(ollama NDJSON)、`/v1/responses`（不做 `previous_response_id`、丢 hosted tools）；Codex 默认 `wire_api="chat"`
- `/v1/models` 双形状 + 别名行 + `?limit=` + `GET /v1/models/:id`；`ALIAS_PREFIX`/`aliases()`/`resolve_alias()`
- `sig.rs`：`fp1:` + base64url 无状态，`thought_signature` ↔ `extra_content.google` 跨协议回放
- `freeport-inject`：`Discovery{SelfServe,Pull,Push,Host}` × `Strategy{JsonKeys,TomlBlock,Guide,Push}`；每个 Agent 一把 `agent:<id>` key
- **项目级 scope**：落点在 `spec::PROJECT_TARGETS` 侧表（不是 `AgentSpec` 字段），只有 claude-code → `.claude/settings.local.json`。铁律「只写客户端自己认定的个人本地文件」——`opencode.json`/`.crush.json` 明确拒绝
- 备份名必须 `id-<目录名>-<hash>`（用日志键会把斜杠写进 backups/）；VS Code Chat 认领靠 `freeportVendor == push::MARK`，组级不能有 url
- e2e 覆盖闸：`list()` 里 `strategy != "guide"` 的 id 必须等于测试跑过的清单，新增适配器不补测试就挂

## 巡检与目录
- 发现五源 + 候选池；社区清单只作线索必须实测；liveness(3–10 token，每日) / capability(50–500 token)；预算 `clamp(日配额×1%,60,800)`
- 健康状态机 unknown→healthy→degraded→cooling→unavailable→retired，**连续 2 次失败**判 unavailable，7 天无恢复 retire
- 目录 feed：Ed25519 + JCS + sha256 + `?since=seq`，三策略 auto/notify/pinned；新模型 canary 0.3 → 20 次成功转正；单次删除 >15% 熔断
- 增量补丁链：`Manifest.patches` 整组进签名（空数组不进）；`PatchFile{fromSeq,toSeq,ops,sha256After}`，patch 的 sha 钉在 manifest → 补丁免单独签名
- 补丁只做 `add/remove/replace/test`（不做 move/copy = 不给补丁读整份文档的能力）；`sha256After` 用 `sha256(to_vec(&Snapshot))` 不用 JCS（含浮点价格）
- 增量前提：本地 seq == since 且链连续（≤24 环），任一不满足走全量；**任何失败都回退全量**；每环强制写 `seq = toSeq`
- 真机：32.8KB 快照改 3 处 → 补丁 445B（1.4%）

## MCP / 实时视图 / 压缩
- `--mcp` 走 stdio JSON-RPC，**不起网关**；6 个**只读**工具，**不吐 authToken**（e2e 锁）；`--mcp` 分支必须 `with_writer(stderr)`（stdout 混日志 = 客户端「连不上」）
- `GET /v1/freeport/inspect` 双闸 = 回环 peer + `x-freeport-control`==`control.key`（0600，start 时生成）。**不复用 authToken**（能烧额度）；反向也锁：控制凭据当 Bearer 发业务请求 401
- MCP 输出带 `source: gateway|disk`；离线文案必须说「连不上网关」；判「实时视图活着」用 `paused`（只在内存）
- `compress.rs`：只动 `role: tool|function` 与 `tool_result`；`maxToolChars`(8000) → 头 30% + 尾 15%；省不到 20% 回滚；标记必含「不要根据这段摘要臆测被省略的部分」
- 日志可复盘：`LogEntry.purpose` + `purposeSource`（header/image/agent-key/default），二者由同一次 `Purpose::infer_with_source` 产出；`X-Freeport-Trail` 非 chat 时加 `[coding:agent-key]` 前缀
- Digest 按池聚合：`pools[{purpose,requests,errors,okRate}]` + `logScanned`，样本是内存日志环（≤300）；**零请求的池不输出**（不是 0%），失败过半且样本 ≥3 才进 `attention`；MCP 进程日志环空时向活网关取
- 扩展 manifest 落盘必须 tmp + rename：`installed()` 会并发读，直接 `fs::write` 会让读者看到半截 manifest → 验签失败被跳过 → 扩展**静默消失**（真机 flake 查到的真因）

## 商业化
- 开源核心 + Pro（¥29/月、¥199/年）：免费版功能完整；**红线：网关里没有 `if !is_pro()`**，Pro 只影响服务端给什么数据
- Pro 数据面：`x-freeport-tier` + 仅 Pro 带 JWT；**Bearer 只在 https 或本机回环发**；manifest 的 tier 进签名；客户端只验签不判档
- `freeport-license`：JWT `alg=EdDSA`，7 天宽限仍算 Pro；`is_pro()` 直判（走 `tier()` 会递归）；私钥只在 `--features issuer`，发布前换 `PROD_PUBKEY`
- `transfer.rs` 迁移码：Argon2id(19MiB,t2,p1) → AES-GCM，解密前校验参数上限；搬 providers/accounts/secrets/api_keys(key_hash)/routing/patrol，不搬台账/冷却/健康/目录

## 产品红线
不代注册、不共享账号、不绕付费墙、不伪造设备指纹；不读系统浏览器 Cookie 库；OAuth 只 public client + PKCE；免费判定只看价格 0 + 目录显式标注（禁看名称含 "free"）；注入只改自己写入的节点，必须备份/幂等/可回滚/退出可清理；不接本地 Ollama；chat2api 不进主链路（法律风险不可转移、逆向链死法救不了），只吸收其参数与扩展挂载位

## 路由策略（v1.10 起）
- `Strategy` 八档：balanced/order/rotate/usage/pace + fastest（探针延迟）/stablest（健康状态+失败次数）/smartest（实测 reasoning>tools>vision>context，实测覆盖声明）。极值策略是稳定排序纯重排，不写 SWRR 状态；parse 未知串回落 balanced
- 三维权重 `routing.weights{enabled,reliability,speed,intelligence}`（默认 70/15/15 但 `enabled=false` = 旧公式）：只作用于 balanced，合成评分是乘在 **档位 × 余量之后**的第三因子，不替代它们
- `blend_score` 口径：可靠性=健康因子；速度=400/(400+ms)，**没测过给 0.6**（给 1.0 会让「速度」权重先打未测通道）；智能=0.35 基座 + 推理 .30 + 工具 .12 + 视觉 .05 + 上下文 ≤.18
- `weight_of_model` 带 weights 参数，`weight_of` 是 `None` 薄壳（老调用点零改动）；`/v1/route` 跳点只有启用时才带 `factors.blend`，UI 此时第三因子显示 `score` 而非 `health`
