# 功能差距分析：Freeport vs `temp/` 15 个参考项目

> 日期：2026-10-20 · 口径：**用户能做什么**，不是「代码里有没有这个词」
> 方法：读 `app/` 全部 crate 的路由表、命令表、数据模型、测试名；读 15 个参考项目的 README 与目录结构。参考项目的能力数字均为其 README 自称，未逐条跑通。

---

## 2026-10-25 补齐记录（发布前冲刺：P0 工程项 + 两个结构性缺口）

| 项 | 改动 | 落点 | 测试 |
|---|---|---|---|
| **Gemini 原生上游** | `Wire::Gemini`：OpenAI ⇄ `generateContent` 双向转换（systemInstruction、functionCall/Response 按**函数名**回填、proto Schema 清洗——`type` 大写、剥 `additionalProperties`、`properties` 键原样保留）、流式 SSE 逐帧转写、hold 提交判定加 Gemini 分支。线格式从「按 id 推断」升级为 **`Provider.wire` 字段**（老 id 推断仅作存量兼容）。认证走 `x-goog-api-key`，绝不同时发 Bearer | `gateway/upwire.rs`、`providers.rs`、`hold.rs`、`core/model.rs`（wire 字段）、`core/presets.rs`（新增 `google-gemini` 预设，`WIRED` 表） | 单测 9 条（URL/system/tools/schema/图片/响应/拦截/流式/工具）+ `tests/gemini_upstream_e2e.rs`（桩上游讲 Gemini 协议，验双向转换、工具往返、usage 含思考 token） |
| **OTLP 导出** | 一请求一 span（OTLP/HTTP + **JSON** 编码，零新依赖）：属性带 provider/model/purpose/attempts/token/折算成本，每跳失败是 event；批量推送、队列 2048 满丢最旧并计数、**绝不反压**；默认关，配置 `gateway.otlp = { enabled, endpoint, intervalSecs, batchSize, headers }`，端点两种写法都收。刻意只做 trace 不做 metrics：4 窗口账本就是 metrics，再推一份只会得到两个对不上的数 | `gateway/otlp.rs`（新模块）、`server.rs`（`record()` 挂钩 + start() 起导出循环 + `GwStatus` 带 otlp 三字段）、`core/model.rs`（OtlpConfig）、`core/dto.rs`（ConfigPatch 三字段） | 单测 4 条（span 结构/64 位时间戳必须字符串/错误事件/URL 两种写法）+ `tests/otlp_e2e.rs`（桩 collector，验证自定义 Bearer 头与路由元数据） |
| **单实例锁** | `tauri-plugin-single-instance` 第一个注册：第二个实例当场退出，只把已有窗口拉到前台。没有它，双开 = 两个进程抢同一份账本文件 | `src-tauri/src/lib.rs`、`src-tauri/Cargo.toml` | 编译期 + 手工双开验证 |
| **自动更新** | `tauri-plugin-updater` 全链路：签名密钥对已生成（私钥 `app/.secrets/` 已 gitignore，公钥提交在 `tauri.conf.json`）；`updater_check` / `updater_install` 命令（下载进度经 `updater://progress` 事件，装完自动重启）；设置页「检查更新」优先走签名通道，未配置/不可达回落 GitHub API + 浏览器；release.yml 改为只看 `TAURI_SIGNING_PRIVATE_KEY`（`TAURI_UPDATER_PUBKEY` secret 废弃，公钥单一来源是 tauri.conf.json） | `src-tauri/`、`src/lib/{ipc,bridge,store}.ts`、`pages/Settings.tsx`、`.github/workflows/release.yml`、`docs/RELEASE.md` | 本地构建 + 发版时 CI 端到端 |
| **i18n 收口** | 约 30 处硬编码中文全部双语化：Accounts（凭据增删/登录/会话/额度提示）、Discover（目录同步/启用/时间格式 `ago()`）、Providers（接入/校验/移除）、Transfer（迁移结果/口令/**覆盖确认词按语言**：zh=覆盖、en=REPLACE，界面提示词与校验同源）、Settings（智能过滤/缓存/限速/冷却） | 上述页面 + `components/Transfer.tsx` | `tsc --noEmit` 通过 |
| **托盘双语** | 托盘菜单文本跟 `DesktopPrefs.uiLang` 走；前端 `setLang` 时同步写 prefs，Rust 侧收到后立即 `tray.set_menu` 重建。语言权威源仍是前端 localStorage，托盘只是消费者 | `src-tauri/{lib,prefs}.rs`、`src/lib/store.ts` | 编译期 + 手工切换验证 |
| **目录基线** | 离线冷启动从 15 厂商/10 模型补到 **15 厂商/88 模型**（全部 `free: true`、零重复、id 用各厂商真实 serving id 以便在线合并精确命中） | `freeport-catalog/src/baseline.json`（+baseline.rs 三个新断言测试） | `cargo test -p freeport-catalog` 37 通过 |

至此 §2 差距总表里的 **#7 部署（更新通道）、#3 观测（OTLP + 已有 JSONL）、#1 上游协议（Anthropic/Responses/Gemini 三线齐）** 全部关闭。仍按决策不做的：fusion / decision 路由 / 60 语言 / 多租户 Web UI / Web 版 daemon 控制台。

---

## 2026-10-24 补齐记录（#1 流式接力 · #5 启发式意图路由）

| 项 | 改动 | 落点 | 测试 |
|---|---|---|---|
| 流式 CONTINUE 续写 | 上游流断在半截（干净闭合无 `[DONE]`/`finish_reason`，或连接报错）且**已有正文、未碰工具调用**时，把已收到的正文作为 assistant prefill 接回同一家上游续写一次；一单只续一次，预算剩 <3s 不开；续写失败回落 STOP（行为同旧版）。工具调用已下发仍 STOP（半截工具参数续写=让客户端执行没写完的工具）——HANDOFF 不做 | `freeport-gateway/src/continuation.rs`（`ContentAcc` 攒正文 + `resume`）、`server.rs::stream_response` 改 pump 双段转发，日志记 `stream_continued` attempt | 单测 5 条（跨 chunk 攒帧/注释行/工具标记/prefill 构造）+ `tests/continuation_e2e.rs`（续写全量正文无 error 帧；工具调用断流报 `stream_truncated` 且不发续写请求） |
| 零成本启发式意图路由 | 三信号（header/图/agent key）都没命中时，按请求特征打分判编码：tools 定义 +2、代码围栏 +2、`role:"tool"` +1、编码类 system（中英文各 +1），≥3 判 coding；判不出回落 chat。不调小模型、不烧额度、不加延迟 | `freeport-core/src/model.rs`（`Purpose::heuristic` + `infer_deep_with_source` + `PurposeSource::Heuristic`）、`server.rs::purpose_of` 带 payload、`Logs.tsx` 依据文案「请求特征识别」 | 单测 4 条（agent 请求识别/纯聊天不误判/heuristic 排序位/序列化） |

---

## 2026-10-21 补齐记录（对照 freellmapi 核心逻辑）

| 项 | 改动 | 落点 | 测试 |
|---|---|---|---|
| 冷却来源分级 | `heuristic / authoritative / credit / tier` 四档，只有猜的能被探针推翻 | `core/cooldown.rs`、`core/error.rs` | `probe_only_recovers_heuristic_cooldowns` 等 4 条 |
| 探针提前解除 | 歇够一半且剩 >60s 才解；保留翻车次数（下次仍翻倍） | `server.rs` 巡检成功分支 | 同上 |
| 空回复细分类 | `empty_completion` / `empty_reasoning`；前 3 次免罚，第 4 次冷却 90s | `core/error.rs`、`server.rs` | `empty_completion_distinguishes_reasoning_truncation` |
| 格式违约 | 要了 JSON 却回散文 → 只跳本请求，不改能力门禁 | `core/error.rs`、`server.rs` | `format_violation_only_when_json_was_requested` |
| 租约收尾 | 200 里的错误体原先漏结算，token 预扣会永久泄漏——已补 | `server.rs` body_error 分支 | 编译期 + 现有 e2e |
| 凭据并发上限 | `gateway.maxInflightPerAccount`（默认 0 不限），流式由后台任务持有名额 | `core/model.rs`、`server.rs` | `try_slot` / `AcctSlot` |
| 衰减统计 | 7 天样本、2 天半衰期的 Beta 后验；样本不足退回状态机 | `core/score.rs`、`core/route.rs` | 4 条（半衰、陈旧样本、抽样均值） |
| 观测 | p50/p95/最长、token、按目录价折算的美元、按厂商/用途拆分 | `core/observe.rs`、`inspect` 的 `window` 段 | 4 条 |
| 观测收口 | `inspect` 与桌面端 `gw_window` 走同一个 `observed_of`，不各算一套 | `server.rs`、`core/dto.rs`（`GwObserved`） | 单测在 `observe.rs`；口径由函数复用保证 |
| 前端消费 | 概览页「近 24 小时」卡（p50/p95/折算美元/未定价单数/按用途）；路由页显示冷却出处与能力折价 | `src/pages/Overview.tsx`、`src/pages/Routing.tsx`、`src/lib/{bridge,ipc,gateway}.ts` | `tsc --noEmit` 通过 |
| 旧版补全协议 | `/v1/completions`：`prompt`→单条 user 消息，响应与流式都改回 `choices[].text`；`echo` 本地实现；批量 prompt / token id / `suffix` 明确 400 | `completions.rs` + `server.rs::completions_entry` | 6 条单测 + `completions_e2e` |
| Pro 数据面 | `crates/freeport-catalogd`：按 Bearer 选 live/monthly、tier 进签名、304、只发下一环补丁；客户端 `due_for` 按档位定节奏 | `freeport-catalogd`、`FeedState::due_for` | 5 条 + 既有 `feed_tier_e2e` |
| 统一模型组 | 裸模型名 = 组：声明它的每一家（含全部凭据）都入链；对外别名 ≥2 家收敛成 `claude-freeport-model`，`/v1/models` 带 `freeport.providers`、模型列表带 `alsoOn`；显式点名不再补别的模型兜底（503 说清原因） | `core/route.rs`（`group_of`）、`core/dto.rs`（`alsoOn`）、`server.rs` | `bare_model_name_covers_every_provider_that_offers_it` 等 4 条 + `model_groups_show_also_on` |
| per-model 窗口第二维 | **没有配置入口**：429 文案点名模型（`Rate limit reached for model \`x\` (TPM): Limit N`）才学进 `account\|model` 窗口，只收紧；Gate 在账号级之后追加模型级检查；记账/结算/探针同步写，凭据删除一并清 | `core/quota.rs`（`learn_model` 等）、`core/error.rs`（`QuotaSignal.model` + TPM/TPD）、`route.rs` Gate、`server.rs` | `model_window_only_exists_once_upstream_states_it` 等 4 条 + `model_named_rate_limit_is_scoped_to_that_model` + `learned_model_quota_excludes_only_that_model` |
| `/v1/embeddings` | 真实转发（不走 chat 接力链，复用 Gate/账本/失败分类）；只走 OpenAI 兼容线，订阅号记 `no_embeddings_endpoint` 一次上游都不打；不代选模型，无路 503；`input` 三形状，未知字段不透传 | `providers.rs::create_embeddings`、`server.rs::embeddings_entry` | `embeddings_e2e`（转发/裁剪/跳过/400/记账） |
| `feat` 字段级下发 | 字段包是**旁路文档**（快照字节不动，摘要与补丁链照旧）：`manifest.feat[]` 名字+摘要进签名，服务端只按令牌 `feat` 挑，`/v1/feat/{name}` 与 `/v1/snapshot/live.json` 都验令牌；客户端先确认自己认识再拉，不认识的记 `featsSkipped`。首发能力 `quota.intel`（厂商 RPM/RPD），发布侧 `sign_feed --feats` | `catalog/feed.rs`（`FeatRef`/`apply_feat`）、`catalogd/{lib,http}.rs`、`core/model.rs`（`served_feats`）、`server.rs`、`Settings.tsx` | catalog 3 条 + catalogd 3 条 + `feat_e2e`（真实 catalogd 对打真实网关） |

仍未做，且**不建议照抄**：fusion 虚拟模型、decision 路由（单机没有第二模型预算）、60 语言、多租户、Web UI。
per-model 窗口、统一模型组、`/v1/embeddings`、`feat` 字段级下发均已于 2026-10-21 落地（见上表）。
`feat` 这条只差数据：`model.scores` 的传输已就绪（发布 `feats/model.scores.json` 即生效），
缺的是「有实测样本才有分」；客户端现在会如实记「本版本不支持」，不会假装面板是满的。

---

## 进度（2026-10-20 晚更新）

下表的编号与 §2 一致。✅ = 已实现并有测试；⬜ = 未做（原因写在括号里）。

| # | 项 | 状态 | 落在哪 |
|---|---|---|---|
| 1 | 流式中途接力 | 部分 ✅ | hedge 已覆盖「首个有效语义块前」的并行接力（attempt 0/1 必跑）；**中途**（已有内容）不接力——语义上是重复输出，不做 |
| 2 | 协议面（completions/embeddings/videos/speech/rerank） | 大部分 ✅ | speech / videos / rerank 扩展端点 + `/v1/completions` + `/v1/embeddings` 已通；batch / files 未做 |
| 3 | 成本与观测 | ✅ | `core/observe.rs`（p50/p95/token/目录价折算美元/按厂商·用途分桶）；`inspect` 的 `window` 段与桌面端 `gw_window` 同一实现 |
| 4 | 上游协议 | ✅ | `freeport-gateway/src/upwire.rs`：Anthropic Messages / OpenAI Responses 双向（含 SSE 重写、hold 提交判定） |
| 5 | 路由策略 | ✅ | `route::Strategy` 八档（含最快/最稳定/最智能）+ 命名链 + 用途映射 + 可调三维权重；UI 在「路由」页 |
| 6 | 多租户 key | ✅ | token 上限 / 模型白名单 / 过期 / `x-freeport-identifier` 限速 |
| 7 | 部署 | 部分 ✅ | 容器化完成（`freeport` 无界面二进制 + Dockerfile + compose + `docs/DEPLOY.md`）；**Web UI 未做** |
| 8 | Agent 周边 | ⬜ | 五个子功能（MCP 面板 / Prompts / Skills / Sessions / CLI 版本）本轮未做 |
| 9 | 厂商广度 | ✅ | 预设 8 → 42 家（`presets.rs`） |
| 10 | 账号形态 | ✅ | Claude / Codex 订阅登录（授权码 + PKCE），回环回调与手工粘贴两条路 |
| 11 | Token 节省 | ✅ | `compress::smart`：diff / grep / 重复日志按结构裁，认不出回落头尾 |
| 12 | 响应缓存 | ✅ | `cache.rs` 精确匹配，只缓存正常收尾且无 tool_calls 的响应 |
| 13 | 媒体能力 | ✅ | speech / video / rerank 三个签名内置扩展 |
| 14 | 多云同步 | ⬜ | 仍只有一次性口令加密迁移码 |
| 15 | 目录质量 | ⬜ | 仍无评测分数 |
| 16 | 官方额度查询 | ✅ | `usage.rs`：Claude `/api/oauth/usage`、Codex `/backend-api/wham/usage`；UI 在凭据行 |
| 17 | 多语言 | ⬜ | 按用户决定：**维持中英双语**，不做 |
| 18 | 客户端形态 | ⬜ | 本轮不做 |

同一条用户决定改变了 #7 的口径：**保持只绑回环**。因此「局域网/远程共享」不做，
容器方案只提供数据面（`/v1/*` + MCP），配置仍要在桌面版改。

## 0. 结论

Freeport 在四个地方已经**领先**参考项目，不在差距清单里：

| 维度 | Freeport 现状 | 参考项目普遍水平 |
|---|---|---|
| 失败作用域 | key / model / platform 三档 + 冷却键分离 | 多数只按 provider 熔断 |
| 额度账本 | 4 窗口（rpm/rpd/tpm/tpd）× 用户/探针分离 × 只收紧学习 | 多为单一计数或纯内存 |
| 能力门禁 | 三态 `Some(true)/None/Some(false)` + 400 自愈写回 | 多数没有能力声明 |
| 注入工程化 | 备份 / 幂等 / 可回滚 / 项目级 scope / 只改自己写的节点 | magpie、cc-switch 同级，其余不及 |

**缺口集中在四块：协议面宽度、成本与观测、Agent 周边管理、多用户与部署。** 下面按影响排序。

---

## 1. 现状清点（可核对）

| 能力 | 实测量 | 位置 |
|---|---|---|
| 下游协议面 | 5 个原生 + 2 个扩展：`/v1/chat/completions`、`/v1/responses`、`/v1/messages`(+`count_tokens`)、`/v1beta`、`/api/*`(Ollama NDJSON)、`/v1/audio/transcriptions`、`/v1/images/generations` | `server.rs:474-490`、`exts.rs` |
| 上游协议面 | **只有 OpenAI 兼容 `/chat/completions`** | `providers.rs:249` |
| 厂商预设 | 8（groq/cerebras/openrouter/mistral/nvidia/copilot/opencode/custom） | `presets.rs` |
| 目录基线 | 15 厂商 / **10 个模型**（仅 pollinations、llm7 有模型）；其余靠 models.dev 在线增量 | `catalog/baseline.json` |
| Agent 适配器 | 18 个 id，其中**只有 9 个真写配置**，另 9 个是 `Strategy::Guide` 只给引导 | `inject/spec.rs:455-690` |
| MCP | 6 个只读工具（status/models/route/quota/digest/recent_errors） | `mcp/tools.rs` |
| 内置扩展 | 2（asr、image） | `ext/manifest.rs` |
| 路由策略 | **单一 `balanced`** + 单一全局 `chain` | `model.rs:903` |
| 额度窗口 | 4 个，`learn()` 仅由 429 响应体回填 | `quota.rs:152` |
| 请求日志 | 内存环 **300 条**；落盘的只有累计计数器 `Stats{requests,tokensIn,tokensOut,errors,since}`，无明细、无时序、无导出 | `server.rs:54`、`model.rs:943` |
| 虚拟 key | 只有 `rpmLimit` + `dailyLimit`（请求数） | `model.rs:131` |
| 成本核算 | **无**。`costIn/costOut` 只用于判「是否免费」 | `types.rs:114` |
| 存储 | JSON 配置 + AES-256-GCM secrets，无 SQLite | `store.rs`、`secrets.rs` |
| 部署形态 | 桌面 GUI + `--headless` daemon；无容器、无 Web UI、只绑回环 | `src-tauri`、ADR-001 |

---

## 2. 差距总表

| # | 域 | 参考项目已有 | Freeport | 影响 | 优先级 |
|---|---|---|---|---|---|
| 1 | 流式中途接力 | FCC 四档 RESTART/CONTINUE/HANDOFF/STOP | `hold.rs` 只护住「首个语义块之前」，commit 后纯 passthrough | 长回合断一次=整轮重来 | **P0** |
| 2 | 协议面 | freellmapi：completions / embeddings / videos / speech + fusion | 无 completions / embeddings / speech / videos / rerank / batch / files | Continue 补全、RAG、Zed/JetBrains 索引直接不可用 | **P0** |
| 3 | 成本与观测 | magpie、router、cc-switch、9router 全有 token/费用/缓存命中/TTFT | 只有毫秒 + token 计数，无价格、无会话成本、无 p50/p95、无导出 | 「免费额度」产品最该回答的问题答不了 | **P0** |
| 4 | 上游协议 | freellmapi / 9router / cc-switch：Anthropic、Gemini、Responses 原生上游 | 全部压成 OpenAI `/chat/completions` | 接不了官方订阅端点、Bedrock/Vertex、非 OpenAI 兼容厂商 | **P0** |
| 5 | 路由策略 | magpie 5 种模式；freellmapi 6 策略 + `auto:<profile>`；router 按动作 HMM | 1 种策略、1 条全局链 | 无法「coding 走强模型、补全走快模型」 | P1 |
| 6 | 多租户 | freellm：token 上限/模型白名单/过期/per-identifier；magpie 局域网 key 带 token 与成本上限 | 只有请求数限流 | 无法安全暴露给应用/团队 | P1 |
| 7 | 部署与共享 | 9/15 有 Docker + Web UI；magpie/cc-switch 有远程或同步 | 桌面 + 回环 | 不能放 NAS/服务器，不能多机 | P1 |
| 8 | Agent 周边 | cc-switch/magpie：MCP 面板、Prompts、Skills、Sessions、CLI 版本 | 只有「注入连接信息」 | 用户仍要手工维护 MCP/skills | P1 |
| 9 | 厂商广度 | 90+ 预设(cc-switch) / 138(9router) / 180+(FreeLLM-Gateway) / 61(FCC) | 8 预设，目录基线近乎空 | 冷启动就得手动加 base_url | P1 |
| 10 | 账号形态 | cc-switch/9router：Claude/Codex/Gemini/GLM/Kimi 订阅号登录 | 仅 GitHub Copilot 设备码 + 手贴 Cookie | 订阅额度这条最大价值线只通了一半 | P1 |
| 11 | Token 节省 | 9router/FCC：RTK 按命令类型过滤 + 去重；freellmapi 压缩管线 | 头 30%/尾 15% 截断，默认关 | 省 20-40% 与省 8% 的差别 | P2 |
| 12 | 响应缓存 | freellm ~23ms 命中、freellmapi opt-in | 只有 `Idempotency-Key` 重放 | 重复 prompt 仍烧额度 | P2 |
| 13 | 媒体能力 | freellmapi video/speech；llm4free TTS；magpie 图像 MCP 工具 | ASR + 图像生成两个扩展 | 语音/视频类 agent 用不了 | P2 |
| 14 | 多云同步 | magpie WebDAV/S3；cc-switch WebDAV/S3；9router cloud sync | 只有口令加密迁移码（一次性） | 多机要手动搬 | P2 |
| 15 | 目录质量 | FreeLLM-Gateway 830+ 模型 + benchmark matrix；freellmapi 速度/可靠性评分 | 有签名 feed + 补丁，但无评测分数 | 路由只能按余量，不能按「谁更靠谱」 | P2 |
| 16 | 官方额度查询 | cc-switch 查 5h/周/月额度与余额，可自定义脚本 | 只从 429 反推 | 用户看不到「还剩多少」的权威值 | P2 |
| 17 | 多语言 | freellmapi 60 语言；free-router 12 | 内联 zh/en 两条字符串 | 出海受限 | P2 |
| 18 | 客户端形态 | FCC 浏览器 Code session、Discord/Telegram/语音；magpie TUI | GUI + headless + MCP | 场景窄（可接受） | P2 |

---

## 3. 逐项说明

### 3.1 流式中途接力（P0，最该先补）

`hold.rs` 的边界是「首个有效语义块之前」：一旦 disarmHedge，后面就是纯转发，上游断流等于客户端收到半截回答后报错。

FCC 的 `core/stream_recovery.py` 把失败后的动作拆成四档，条件与协议无关，可直接照做：

| 档 | 触发 | 动作 |
|---|---|---|
| RESTART | 还没有任何语义块 | 换下一跳重发整段 |
| CONTINUE | 已有文本但无 tool_call | 用已产出内容做前缀续写 |
| HANDOFF | tool_call 已下发 | 换模型接管并声明已执行的工具 |
| STOP | 不可恢复 | 如实结束，附原因 |

Freeport 现在只有 RESTART（hedge 窗口内）与 STOP。缺 CONTINUE/HANDOFF 意味着**长工具回合的失败成本是整轮**，而这恰好是 coding agent 最贵的场景。

### 3.2 协议面（P0）

| 端点 | 谁有 | Freeport | 谁在用 |
|---|---|---|---|
| `POST /v1/completions` | freellmapi | ✗ | Continue / 编辑器 ghost text |
| `POST /v1/embeddings` | freellmapi、Ollama 模拟 | ✗ | RAG、代码索引、Zed/JetBrains |
| `POST /v1/audio/speech` | freellmapi、llm4free | ✗ | TTS 类客户端 |
| `POST /v1/videos/generations` | freellmapi | ✗ | 视频生成 |
| `POST /v1/rerank` | 少见 | ✗ | 检索重排 |
| `POST /v1/batch`、`/v1/files` | 少见 | ✗ | 批量任务 |
| fusion 虚拟模型 | freellmapi | ✗ | 多模型并行 + judge 合成 |

其中 **completions 与 embeddings 是「客户端直接 404」级别的硬缺失**：Continue 的 tab 补全、Zed 的语义索引都靠它们，不是体验问题。

### 3.3 成本与观测（P0）

已确认的代码事实：

- `CatalogModel` 有 `cost_in` / `cost_out`，唯一消费者是 `model_is_free()`（`types.rs:114`），**没有任何地方用它算钱**。
- `LogEntry` 有 `usage`（token 数）与 `ms`，无成本字段（`model.rs`）；持久化的 `Stats` 只有四个累计整数。
- 日志环 300 条在内存，重启即失；`Digest` 只出 24h 池聚合。
- 无 OTel / Prometheus 依赖（全仓 grep 为空）。

对比：magpie 有「tokens / cache reads / reasoning / cost / 每会话成本 / OTLP 导出」；router 在响应头回 `x-router-cost-usd`、`x-router-cache-read-tokens`，并开放 `/v1/sessions/:id/cost` 与 NDJSON 分析导出；cc-switch 能扫各 CLI 会话日志补出全年热力图。

**结论：Freeport 的账本是「额度」账本，不是「钱」账本，也不是「时序」账本。** 免费额度产品最终要回答「这次调用值不值、哪个号在烧钱、今天比昨天差在哪」，现在只能给出总额。

### 3.4 路由策略（P1）

| 能力 | 参考实现 | Freeport |
|---|---|---|
| 多模式 | magpie：`smart` / `order` / `rotate` / `usage` / `pace` | ✅ 八档：balanced / order / rotate / usage / pace + fastest / stablest / smartest |
| 三维权重 | freellmapi：可靠性 70% · 速度 15% · 智能 15%，可调 | ✅ `routing.weights`（启用后第三因子换成合成评分；关闭即旧公式） |
| 命名链 / 档位 | freellmapi `auto:<profile>`、freellm 三个元模型 `free-fast/free-smart/free` | ✅ 命名链 + 用途映射 |
| 按用途分链 | magpie 分组可嵌套 | ✅ 用途池只做**过滤**，另由 `purposeChain` 决定先走哪条链 |
| 意图路由 | magpie / router：小模型判动作再选模型 | 无 |
| 成本感知 | router / magpie 按价格选 | 无（只按余量） |
| 严格模式 | freellm：宁可失败不替换 | 无（总会替换） |
| 隐私路由 | freellm：跳过会拿免费档训模型的厂商 | 无（目录无该字段） |

### 3.5 多租户与共享（P1）

`ApiKey` 现有字段：`rpmLimit`、`dailyLimit`（请求数）、`enabled`、`usage{day,dayCount,minute,minuteCount}`。

缺：token/成本上限、`allowedModels` 白名单、`expiresAt`、按调用方标识（`x-*-identifier`）的独立滑窗、浏览器用短时签名 token。

再加「只绑回环」这条产品决策，结果是：**Freeport 不能被安全地共享**。magpie 的做法值得抄一半——局域网开关 + 每客户端命名 key + token/成本双上限，风险可控且不破坏「默认只本机」。

### 3.6 Agent 周边（P1）

cc-switch 的 MCP / Prompts / Skills / Sessions 四件套，magpie 的 Library（instructions / MCP / skills / subagents 一次写进各 agent 格式），Freeport 全无。

更硬的一条：**18 个适配器里 9 个是引导式**（continue、gemini-cli、roo、cursor、zed、aider、kilo、goose、workbuddy），实际自动写入只有 claude-code、claude-code-vscode、codex、cline、opencode、vscode-chat、crush、jetbrains、claude-desktop。cc-switch 是 10 个工具全写入，magpie 是 60+。

### 3.7 厂商与账号（P1）

- 预设 8 家 vs 90+/138/180+/61。
- 登录路径只有 GitHub 设备码（`server.rs:411` 里 client_id 写死 `copilot::CLIENT_ID`）；Claude Pro/Max、ChatGPT/Codex、Gemini、GLM Coding Plan、Kimi 这些「订阅当额度」的入口都没有。Cookie 只做 OpenCode 试点且**不自动续期**（设计选择，合理）。
- 无「向厂商查余额/窗口」的能力，只能等 429 反推。

### 3.8 Token 节省与缓存（P2）

`compress.rs` 是「超长就掐头去尾」，四条自我约束写得很好，但能力边界明确：不认内容类型。9router 的 RTK 先看 1KB 判 `git-diff`/`grep`/`ls`/`tree`/`dedup-log` 再选过滤器，官方称省 20-40%；FCC 同样接了 RTK + Headroom。

响应缓存完全没有（只有 `Idempotency-Key` 重放），同类重复 prompt 继续烧额度。

### 3.9 部署与运维（P1/P2）

无 Dockerfile（`temp/` 里 9 个项目有）、网关不托管 Web UI（无 `ServeDir`）、无 OTLP/Prometheus。自动更新（`check_update`）与本地 crash 目录已有，够用。

### 3.10 目录质量（P2）

签名 feed + 增量补丁（32.8KB → 445B）是 Freeport 的强项，但 `baseline.json` 实际只有 10 个模型，离线冷启动接近空目录；且目录里没有速度/可靠性评分，路由无法回答「这两家都能用，谁更靠谱」。FreeLLM-Gateway 的 benchmark matrix、freellmapi 的 live speed/reliability 分数是这一层的参照。

---

## 4. 明确不建议抄

| 项 | 原因 |
|---|---|
| chatgpt2api 的官网逆向图片链路 | 与「不绕付费墙 / 不伪造指纹」红线冲突，法律风险不可转移 |
| 读系统浏览器 Cookie 库 | 红线明令禁止；现有「手贴 Cookie」是正确折中 |
| 共享账号 / 代注册 | 红线 |
| 本地 Ollama 作为上游 | 已决策不做（对外**模拟** Ollama 是另一件事，已实现且应保留） |
| Fusion 多模型合成 | 免费池质量参差，judge 模型本身也要额度，性价比不成立 |
| 无鉴权的局域网共享 | magpie 是可借鉴形态，但必须带命名 key + 上限才做 |

---

## 5. 建议顺序

**第一批（补「能不能用」）**
1. 流式 CONTINUE / HANDOFF 接力（`hold.rs` 扩展）
2. `/v1/embeddings` + `/v1/completions`（走现有 ext 或新 entry）
3. 成本核算：`usage × costIn/costOut` → `LogEntry.cost` + 响应头 + Digest
4. 日志落盘（滚动文件即可，不必上 SQLite）

**第二批（补「值不值」）**
5. 命名链 / 每用途一条链（`RoutingConfig` 加 `chains: {purpose: [...]}`）
6. 虚拟 key 四件套：token 上限、模型白名单、过期、per-identifier
7. 厂商预设扩到 40+，至少补 Claude / Codex / Gemini 三种订阅登录
8. 观测导出：OTLP 或 NDJSON（二选一，别都做）

**第三批（补「能不能给别人用」）**
9. 容器化 + 网关托管只读 Web UI（可选，需先解决鉴权）
10. MCP / Prompts / Skills 管理（先只做 MCP 面板，收益最高）
11. RTK 类过滤器 + 响应缓存（都要 opt-in）
12. 媒体端点（speech / video）与 i18n 扩展

---

## 6. 自证清单（10 分钟可复现）

| 断言 | 验证方式 | 预期 |
|---|---|---|
| 缺 completions | `curl -X POST :port/v1/completions` | 404 |
| 缺 embeddings | 同上 `/v1/embeddings` | 404 |
| 无成本 | `grep -rn "cost_in" crates/` | 只命中 `types.rs` 判免费 |
| 中途断流整轮失败 | 假上游发 3 个块后断开 | 客户端收到半截 + 错误，无续写 |
| 日志不持久 | 重启 GUI 后看日志页 | 清空 |
| key 无 token 上限 | 建 key 时只有 rpm / 每日次数 | UI 无 token 字段 |
| 9 个适配器仅引导 | `grep "Strategy::Guide" inject/spec.rs` | 9 处 |
| 单策略 | `grep -n strategy model.rs` | 只有 `balanced` 默认值 |
