# 集成评审：free-claude-code / free-router-proxy 源码分析

> 对象：`temp/free-claude-code`（Python，409 文件 / 72,465 行）、`temp/free-router-proxy`（Node，单文件 server.mjs 118KB）
> 视角：以 Freeport 现有代码为基线，判断「有没有集成需求、集成什么、落在哪」
> 方法：只读行为与契约。两个项目一为 AGPL-3.0、一无 LICENSE 文件，本文不复制代码，只记录可独立实现的判定规则与数据结构。

---

## 0. 结论

四条判断，按重要性排序：

1. **协议契约层有 7 处偏差，其中 2 处会直接导致 400 或功能失效。** Freeport 接管 Claude Code 只写了 3 个环境变量（`spec.rs` 的 `CLAUDE_KEYS`），而 Claude Code 依赖的一整套开关、模型发现约定、命名空间过滤全都没做。这不是体验问题，是能不能用的问题。
2. **接入面的正确维度是「谁决定模型列表」，不是 IDE 数量。** 四种模式（Redirect / Pull / Push / Host）里 Freeport 只覆盖第一种，而 `inject` 的 `Src` / `Strategy` 枚举从签名上就表达不出另外三种——缺目录、缺删除、缺 Push。所以工作量是 `O(模式) + O(条目)`，不是 `O(IDE)`；批次 A′ 也不能按「只补 Claude」来排。详见 §2.7。
3. **流式接力是本次最大收获。** Freeport 的 `hold.rs` 只覆盖「首个语义块之前」这一个窗口，commit 之后是纯 passthrough。FCC 的 `stream_recovery.py` 把失败后的动作拆成 **RESTART / CONTINUE / HANDOFF / STOP** 四档，条件清晰、与协议无关，可直接照做。
4. **路由侧值得替换的是「一次失败拆三个决策」**，不是它的排序算法。free-router 的 `applyProviderVerdict` 把一次 429 拆成「等多久 / 这个模型是否免费 / 真实日限额」，比 Freeport 现在的固定冷却阶梯（2m/10m/1h/24h）更贴近免费额度的真实形态。

不需要集成的：聊天 UI、浏览器 Code session、Discord/Telegram、语音、61 家 provider 目录。理由见 §7。

---

## 1. 阅读范围

| 项目 | 规模 | 本次精读 | 结论 |
|---|---|---|---|
| free-claude-code | 72k 行 Python，11 个 agent launcher + 6 个 IDE/桌面集成 | `application/routing.py`、`application/execution.py`、`core/stream_recovery.py`、`core/delivered_response.py`、`core/history_replay.py`、`harnesses/*`、`api/handlers/messages.py` | 协议契约与接力逻辑值得全量吸收；产品层（session/messaging/voice）不碰 |
| free-router-proxy | 单文件 118KB + 8 个 mjs | `server.mjs` 的候选/冷却/脱敏、`quota.mjs`、`thought-signature.mjs`、`config.mjs`、`ui.mjs` | 三个机制值得移植；排序哲学与 Freeport 不同，不替换 |

---

## 2. 协议契约：7 处偏差

| # | 契约 | Freeport 现状 | 后果 |
|---|---|---|---|
| 1 | Claude Code 环境变量全集 | `spec.rs:90-94` 的 `CLAUDE_KEYS` 只写 `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_MODEL` | 旁路流量未关；登录提示仍在；haiku 档无模型 |
| 2 | `/v1/models` 双形状 | `server.rs::list_models` 只返回 `{object:"list",data}` | Claude Code 的模型发现静默失败 |
| 3 | 模型 id 命名空间 | 暴露上游真实 id | 发现的模型被 `claude`/`anthropic` 前缀过滤器丢掉 |
| 4 | 会话头 | 只认 `x-freeport-session`/`x-session-id` | 放弃了一个官方免费信号 |
| 5 | 能力声明 | 无 | 发现的模型不带 effort/thinking，功能被降级 |
| 6 | 分档模型 | 无 `ANTHROPIC_DEFAULT_*_MODEL` | Claude Code 内部小任务（haiku 档）打空 |
| 7 | 非 CLI 入口 | 只有 8 个 agent 文件 | VS Code Chat / Claude Desktop / JetBrains 全缺 |

### 2.1 环境变量：FCC 的完整集合

`harnesses/claude.py:12-27` 的 `claude_proxy_values` 是它验证过的全套：

```python
ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN,
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY = "1",
CLAUDE_CODE_AUTO_MODE_SERVER = "0",        # 别让 Claude Code 打 Anthropic 的分类器
CLAUDE_CODE_DISABLE_ADVISOR_TOOL = "1",
CLAUDE_CODE_AUTO_COMPACT_WINDOW = "190000",
DISABLE_AUTOUPDATER / DISABLE_FEEDBACK_COMMAND / DISABLE_ERROR_REPORTING = "1"
```

并且 `build_claude_proxy_env` 会**删除所有 `ANTHROPIC_*` 前缀的旧变量**和 `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`——后者的注释很关键：「聚合流量开关也会压掉网关模型发现」。

> 落地改动：`spec.rs` 的 `CLAUDE_KEYS` 需要从 3 项扩到 8 项以上，并新增一个 `Src::Literal` 之外的 `Src::Delete(prefix)` 语义。当前 `KeySpec` 只能写不能删，这是结构缺口。

### 2.2 `/v1/models` 双形状

官方文档确认的发现契约：`GET /v1/models?limit=1000`，3 秒超时，**任何重定向都算失败**（防凭证外泄）。响应需要 `{data:[{id, display_name, created_at, type:"model"}], has_more, first_id, last_id}`。

OpenAI 形状与新形状字段不冲突，可以同一个响应里同时给：

```json
{ "object": "list", "type": "list",
  "data": [ { "id": "claude-freeport-sonnet", "object": "model",
              "display_name": "Freeport · Llama 3.3 70B", "type": "model",
              "created_at": "...", "created": 1700000000 } ],
  "has_more": false }
```

两端各自忽略不认识的字段。Freeport 的 `GwModel`（`dto.rs:247`）已经有 `freeport` 元数据，加 `display_name` 与 `type` 即可，无需新类型。

### 2.3 命名空间：必须造 `claude-*` 别名

Claude Code 的发现**只收以 `claude` 或 `anthropic` 开头的 id**。上游真实 id（`llama-3.3-70b`）会被静默过滤，用户看到的是一个空的选择器。

这条把「模型别名」从一个可选功能变成了**硬依赖**：没有别名层，模型发现就没有意义。别名同时解决了另外两件事——`free-best` 那类虚拟路由名、以及跨模型降级的命名统一。

### 2.4 会话头

`X-Claude-Code-Session-Id` 每请求都发，是比 Freeport 现在的「model + system + 首条 user 消息指纹」更准的会话键。`server.rs:1172` 的 `session_key()` 加一行即可，不改变现有 fingerprint 兜底逻辑。

### 2.5 能力声明

FCC 在 VS Code Chat 与 Codex 的模型目录里逐条写能力（`vscode_chat_integration.py:26-55`）：

```json
{"id": "...", "toolCalling": true, "vision": true,
 "maxInputTokens": 190000, "maxOutputTokens": 8192,
 "supportsReasoningEffort": ["low","medium","high"], "defaultReasoningEffort": "medium"}
```

Anthropic 侧对应 `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL_SUPPORTED_CAPABILITIES`（取值 `effort`/`xhigh_effort`/`max_effort`/`thinking`/`adaptive_thinking`/`interleaved_thinking`），以及 `_NAME`/`_DESCRIPTION` 覆盖 picker 标签。

Freeport 的 `caps` 已经由探针实测过（`caps_probed`），是这份数据最可靠的来源——比 FCC 从 models.dev 猜更准。

### 2.6 分档模型

Claude Code 内部按 sonnet/opus/haiku 三档发请求（还会打 `fable`）。FCC 的匹配是**子串**（`routing.py:263-269`）：

```python
normalized = model_name.lower()
return next((r for r in _ROUTE_SETTINGS if r[0] in normalized), None)
```

`claude-sonnet-4-5`、`claude-3-5-sonnet-20241022`、`claude-sonnet-4` 都会命中 `sonnet` 档。Freeport 只需三档，`fable` 是 Anthropic 内部代号，可归到 opus。

### 2.7 接入面：四种模式，不是一份 IDE 清单

把「适配所有 IDE」理解成「写 N 个配置文件」，会低估结构、高估条目。真正决定工作量的是**谁来决定模型列表**：

| 模式 | 谁决定模型列表 | 代表 | 对网关的要求 | Freeport 现状 |
|---|---|---|---|---|
| **Redirect** | 用户选，或客户端自拉 OpenAI 形状 `/v1/models` | Cline / Roo / Kilo / OpenCode / Aider / Goose / Crush / Continue / WorkBuddy | 只写 baseURL + key | 已有 `Strategy::JsonKeys` |
| **Pull** | 客户端启动后自己拉，**但只认自己认识的形状与命名** | Claude Code（Anthropic 形状 + 只收 `claude*`）、Claude Desktop（半 Push，见 2.7.1） | 双形状 + 别名 | 缺（§2.2、§2.3） |
| **Push** | **客户端不做发现**，模型表必须在注入时序列化进配置 | VS Code Chat、Codex catalog | 目录快照 + 能力字段 | 完全没有 |
| **Host** | 客户端只说自有协议，须第三方 adapter 进程 | JetBrains ACP | 只能注 env | 无 |

**Push 的实证**：`vscode_chat_integration.py:26-55` 写进 `chatLanguageModels.json` 的每个条目就是一份能力清单——

```json
{"id": "...", "name": "...", "url": "<gw>/v1/messages", "toolCalling": true,
 "vision": false, "maxInputTokens": 182000, "maxOutputTokens": 8000,
 "supportsReasoningEffort": ["low","medium","high"], "defaultReasoningEffort": "medium"}
```

这些字段只有模型目录知道，**客户端不会去问网关**。所以 Push 模式的前置条件不是「多写一个文件」，而是「注入器能读到目录」。

**Host 的实证**：`jetbrains_acp_integration.py` 并没有实现 ACP。它读 JetBrains 的 `installed.json` 找到已安装的 `@agentclientprotocol/claude-agent-acp`，把 `claude_proxy_values()` 塞进那个进程的 env，再往 `~/.jetbrains/acp.json` 写一条 `agent_servers` 条目。这条路的真实能力上限是「用户装了那个 adapter，且它读环境变量」。**不要在 UI 上承诺「支持 JetBrains」。**

**结构性缺口**：`inject/src/spec.rs` 现在只能表达 Redirect——

```rust
pub enum Src { Base, BaseV1, Key, Model, Literal(&'static str) }   // 无目录、无删除
pub enum Strategy { JsonKeys(&'static [KeySpec]), TomlBlock, Guide } // 无 Push、无 Host
```

且 `Strategy` 是 `&'static`，装不下「按当前目录序列化」这类运行时数据。这才是 `AGENTS` 只有 8 条、非 CLI 入口全缺的根本原因：不是没写条目，是**写不出来**。

**产品判断**：不要兑现成一份 20 项勾选清单。装了哪个显示哪个，其余折叠成「其他客户端」。注入的价值是省掉一次手动配置，不是覆盖平台榜；按检测到的排序比按覆盖率排序更符合用途。

#### 2.7.1 各入口落点

| 入口 | 目标文件 | 结构 | 模式 |
|---|---|---|---|
| Claude Code（VS Code 扩展） | `<Code>/User/settings.json` | `claudeCode.environmentVariables: [{name,value}]` + `claudeCode.disableLoginPrompt: true` | Pull（另一条注入路径） |
| VS Code Chat | `<Code>/User/chatLanguageModels.json` | 模型组数组，组上带自定义 marker（FCC 用 `fccIntegration: "vscode"`） | Push |
| Claude Desktop | `<AppSupport>/Claude-3p/` | 独立网关配置目录，非 `claude_desktop_config.json`；用 `X-FCC-Model-View` 头选模型视图 | Pull / 半 Push |
| JetBrains ACP | `~/.jetbrains/acp.json` | `agent_servers` 下一条自定义 agent，寄生已装的 adapter | Host |
| Codex | `~/.codex/config.toml` | 除 provider 块外还有**模型目录 JSON 路径** | Push |

VS Code 扩展这条尤其重要：用户装的是扩展时，`~/.claude/settings.json` 里的 `env` 不一定生效——FCC 专门区分了这两条路径（`claude_integration.py` 写 VS Code settings，`claude.py` 供 launcher 用）。

---

## 3. 流式接力：四档恢复

### 3.1 FCC 的候选循环

`execution.py:435-570`，要点四处：

1. **空闲预算，不是总墙钟**：每收到一个 chunk 就重置 `progress_deadline`（`:508`），上游一直有输出就不会被掐。Freeport 用 45s 总预算，长回答会被误杀。
2. **启动握手不占正文预算**：`progress_deadline += monotonic() - opening_started`（`:457`）。连上游的时间另算，避免「连接慢」被当成「生成慢」。
3. **commit 判定 = 第一个非空 chunk**（`:494-506`）。比 Freeport 的「text/reasoning/tool_call」粗，因为它不解析语义；Freeport 的判定更准确，保留。
4. **commit 之后仍然可以换**（见下）。这是 Freeport 完全没有的。

### 3.2 四档恢复判定

`core/stream_recovery.py:62-106`，判定顺序即优先级：

```text
normal_stop_seen            → STOP      （上游已正常收尾，别多事）
未释放任何内容               → RESTART   （整请求重发，等价于现在换下一跳）
已释放 + 有 tool_calls
    ├ 可交接 + 非结构化输出   → HANDOFF   （工具已闭合，换模型接着执行）
    └ 否则                   → STOP
已释放 + 结构化输出(json)     → STOP      （前缀不可拼接）
已释放 + 前缀 eligible       → CONTINUE  （带 text/thinking 前缀续写）
其余                        → STOP
```

进入判定前的前置条件（`delivered_response.py:384-450`）值得照抄成数据结构：

| 字段 | 含义 |
|---|---|
| `text` / `thinking` | 已释放的正文与思考 |
| `has_calls` | 是否有**已闭合**的 tool_call |
| `eligible` | `unsafe_reason is None`，前缀能否安全续写 |
| `can_handoff` | 所有 thinking 块是否已闭合 |
| `unsafe_reason` | `refusal` / `native_delta` / `unknown_event` / `missing_item` / `text_constraint` |

### 3.3 Freeport 的差距

`hold.rs` 只处理「首个语义块之前」，`HoldOutcome::Committed` 之后是 `rest` 直通。所以：

| 场景 | Freeport 现在 | FCC |
|---|---|---|
| 200 + 空 content | 已由 hold 修复 | 同 |
| 首块之后断流（无输出） | 客户端看到断流 | RESTART |
| 输出一半后断流（纯文本） | 客户端看到断流 | CONTINUE（续写） |
| 工具调用已闭合后断流 | 客户端失败 | HANDOFF |
| json mode 下断流 | 同 | STOP（正确：不可拼） |

改法：把 `hold.rs` 的 `Scan` 扩展成持续扫描，产出 `DeliveredPrefix`，失败时调 `assess_recovery`。**注意 Hold 的语义会从「提交门」变成「账本」**，`MAX_HEAD_BYTES` 需要改成可增长（上限按已释放 token 估）。

### 3.4 跨模型切换的隐藏地雷：reasoning 历史

一旦允许跨模型降级（CONTINUE / HANDOFF / 别名路由），立刻会撞上：A 家的 thinking 签名给 B 家必然 400。

FCC 的解法（`history_replay.py:40-47`、`425-506`）是**带内自描述**，不依赖网关存状态：

```text
signature 字段 = "fcc:history:v1:" + base64({origin:{provider,protocol,endpoint,connection,model}, native:<原始块>})
```

请求进来时 `prepare_history(body, destination)` 逐条投影：

- `destination.accepts(record.origin)` 为真 → 还原原生块
- 为假 → **降级成一段普通文本塞回上下文**（内容不丢，只是失去原生语义）
- 空 content 的 assistant 消息整条删除（某些厂商会 400）

free-router 走的是另一条路：网关内存 Map 缓存 `tool_call.id → thought_signature`（`thought-signature.mjs:43-60`，上限 5000 条，找不到就写 `skip_thought_signature_validator` 哨兵）。

**Freeport 应选 FCC 路线**：无状态、跨重启、跨多机（Pro 多机同步时不用同步缓存）。成本是一个 base64 编解码 + 一个 4 字段的 origin 比较。

---

## 4. 路由与配额

### 4.1 候选生成

`server.mjs:1716-1764`，两级：

```text
candidateModels = routeCandidates(routeName) ?? registry.directCandidates(model)
                → filterCandidates：免费？能力够？不在冷却？
```

两个细节值得抄：

- **全部候选都在冷却时，回退到忽略冷却按序重试**（`:1749-1760`），而不是直接 503。理由写在注释里：「别把临时冷却变成硬故障」。Freeport 的 Gate 是硬过滤，全冷却 → 空链 → 503。建议加一个开关：`exhaustedPolicy: fail | retry_cooling`。
- **能力门禁按请求内容算，不是按模型声明猜**（`:1592-1610`）：扫 messages 里的 `image_url`/`video_url`，再对 `architecture.input_modalities`。Freeport 的 Gate **完全没有能力门禁**（`route.rs` 只有 tombstone/quota/health/cooldown 四项），带图请求会打到不支持视觉的模型上。

`providers.mjs:39-61` 的哲学也值得记：「目录没说」≠「不支持」。只有目录明确列了 `supported_parameters` 才敢排除，否则放行。

### 4.2 一次失败拆三个决策

`server.mjs:1668-1714` 的 `applyProviderVerdict`，返回值直接当冷却时长用：

| 判定 | 依据（`quota.mjs`） | 动作 |
|---|---|---|
| `permanentRejection` | 404 且 message 含 `no longer available`；400 且 `only supports ... Interactions API` | 永久移出路由 |
| `zeroBalanceRejection` | 400/402/403 且 `balance: 0` **同时**含 insufficient/not enough | 该 key 无余额，换 key |
| `quota.noFreeTier` | 所有 free_tier 限额 `limit: 0` | **永久无免费档**，移出 |
| `quota.dailyRequestLimit` | 429 里 `limit: N > 0` 的日请求限额 | **回写账本上限** |
| `exhaustedWindow == "day"` | 日窗口耗尽 | 冷却到配额日边界 |
| `retryDelayMs` | `RetryInfo.retryDelay` 或 `retry in Xs` | 短冷却 |

`parseQuotaFailure`（`quota.mjs:1-110`）的实现细节：`limit` 在 `error.message` 的自由文本里，窗口在 `details[].QuotaFailure.violations[].quotaId` 里，两半必须配对；条数不匹配时**丢弃窗口而不是猜**。

> 这条与 Freeport 已定的「不做响应头学限额」不冲突：那是指 `x-ratelimit-*`（免费厂商基本不返），这里读的是 429 body 里厂商主动报的配额数字。而且它是唯一能区分「这个模型永远没有免费额度」和「今天用完了」的信号——单靠状态码分不出来。

### 4.3 配额日边界

`quota.mjs:151-170`：用 `Intl.DateTimeFormat` 按配置时区算出距离本地午夜的毫秒数，`+60s` 避免卡在边界上重试。Freeport 的冷却阶梯是固定值，RPD 耗尽后会按 2m/10m/1h/24h 一路试，半夜白打好几次。

---

## 5. 配置与安全

### 5.1 分层 + 墓碑 + 加法式迁移

`config.mjs:343-374`：

- `deepMerge(base, overlay)`：对象递归、数组整体替换
- `addMissingKeys(target, defaults)`：只补缺失骨架，**从不覆盖用户值**——结构性升级不破坏配置
- `buildLiveConfig`（`:499-520`）：叠加后按 `_removedRoutes`/`_removedProviders` 删一遍。删除是**墓碑**，上游加回来也不会复活用户删掉的东西

Freeport 现在是单文件 `freeport.json`。单机够用，但 Pro 多机同步时会直接冲突——没有「默认值 / 用户覆盖」的边界，就没有合并语义。

### 5.2 写入校验

`ui.mjs:94-140`，四条：

- 拒 `[\r\n\0]` —— `.env` 会被 `set -a` source，换行等于下次启动执行任意命令
- 只改指定 key，保留注释与顺序
- 临时文件 `mode: 0o600` + rename 原子替换
- **rename 后补 `chmod 0600`** —— 原文件可能是 0644，rename 不继承权限

Freeport 的 `secrets.rs` 已经做了原子写 + 0600 + AES-GCM，前三条等价。缺的是「只改指定键、保留注释」这种面向用户可见文件的写法——目前写的是 JSON，不需要。

### 5.3 脱敏器

`redact.mjs`：从环境变量名匹配（`*_API_KEY|SECRET|TOKEN|PASSWORD|...`）收集值，按长度倒序替换；同时处理 `NAME=value` 形式的整行；代理 URL 里的 `user:pass@` 也被拆出来脱敏；深度遍历请求体。

Freeport 的 `keyMask` 只做展示层，上游把 key 回显在错误体里时，原样进日志。

### 5.4 loopback 防护

`server.mjs:2296` 的 `uiGuardFailure`：校验 `remoteAddress` 是回环、`Host` 头是 localhost/回环、`Sec-Fetch-Site` 不是 cross-site、`Origin` 是 localhost。防的是浏览器里的恶意页面通过 DNS rebinding 打到 `127.0.0.1` 的管理 API。

Freeport 管理面走 Tauri IPC，风险低；但网关本身有虚拟 key 鉴权，加了 Web 控制台或任何 GET 端点后这条就必须有。

---

## 6. 可移植清单

| # | 能力 | 参考实现 | Freeport 落点 | 价值 | 工作量 |
|---|---|---|---|---|---|
| 1 | Claude Code env 全集 + 旧变量清理 | `harnesses/claude.py:12-27` | `inject/src/spec.rs` + `Src::Delete` | **阻塞级** | 0.5d |
| 2 | 分档模型（sonnet/opus/haiku） | `application/routing.py:263-269` | `spec.rs` 的 `CLAUDE_KEYS` | **阻塞级** | 0.5d |
| 3 | `/v1/models` 双形状 | `claude_integration.py` + 官方契约 | `gateway/src/server.rs::list_models`、`dto.rs::GwModel` | 高 | 0.5d |
| 4 | 模型别名层（含 `claude-*` 命名空间） | `routing.py`、`model_refs.py` | `core/src/route.rs::plan` 前置解析 | **高**（3 依赖它） | 2d |
| 5 | `X-Claude-Code-Session-Id` | 官方契约 | `server.rs:1172` `session_key()` | 中 | 0.2d |
| 6 | 能力声明写入模型目录 | `vscode_chat_integration.py:26-55` | 注入器 + `GwModel` | 中 | 1d |
| 7 | 四档流式恢复 | `stream_recovery.py`、`delivered_response.py` | `gateway/src/hold.rs` | **高** | 3d |
| 8 | reasoning 历史带内封装 | `history_replay.py` | 新增 `core/history_replay.rs` | 高（4、7 的前置） | 2d |
| 9 | 429 三分法 + 配额回填 | `quota.mjs` 全文（约 170 行） | `core/src/error.rs` + `core/src/quota.rs` | **高** | 1.5d |
| 10 | 配额日边界冷却 | `quota.mjs:151-170` | `core/src/cooldown.rs` | 中 | 0.3d |
| 11 | 能力门禁（按请求内容） | `server.mjs:1592-1610` | `core/src/route.rs` Gate 增一段 | 中 | 0.5d |
| 12 | 全冷却回退 | `server.mjs:1749-1760` | `route.rs` + 配置开关 | 低 | 0.3d |
| 13 | thought_signature 哨兵 | `thought-signature.mjs` | `gateway/src/gemini.rs` | 中（若不做 8 则必需） | 0.5d |
| 14 | 出站清洗（剥内部字段 + 脱敏） | `server.mjs:1766-1782` | `gateway/src/providers.rs` | 中 | 0.5d |
| 15 | **接入模式抽象 + `inject` 接 `Catalog`** | 四种模式（§2.7） | `inject/src/spec.rs` 的 `Strategy` / `Src` | **高**（17–20 的前置） | 2d |
| 16 | 配置分层 + 墓碑 | `config.mjs:343-374,499-520` | `store.rs` | 低（Pro 前） | 1d |
| 17 | Push 模式：VS Code Chat | `vscode_chat_integration.py:26-55` | `Strategy::Push`（序列化目录） | 中 | 1.5d |
| 18 | Pull 模式：Claude Desktop 独立目录 | `claude_desktop_integration.py` | `Strategy::Push` + 视图头 | 中 | 1d |
| 19 | Host 模式：JetBrains ACP（只注 env） | `jetbrains_acp_integration.py` | `Strategy::Host` | 低（依赖第三方 adapter） | 0.5d |
| 20 | Redirect 条目补齐 9 个 | `harnesses/*` | `spec.rs` 的 `AGENTS` | 中 | 1.5d |

合计约 20.8 人日（原 17.3）。差额 +3.5 全部落在接入面：抽象 2d + 四种实现 4.5d − 原计划的 3d。

两条链：

- **1 → 2 → 3 → 4**：没有别名层，模型发现拿不到东西；不做 3，别名也没处暴露。
- **15 → 17 / 18 / 19 / 20**：不先改签名，每加一个客户端都要回头改网关。反过来说，15 做完之后，20 那 9 个条目每个只要约半小时。

---

## 7. 明确不做

| 不集成 | 理由 |
|---|---|
| 聊天 UI、浏览器 Code session（`runtime/code_sessions_sqlite.py` 833 行） | 违反「v1 不做聊天产品」；且这部分是 FCC 的产品护城河，不是网关能力 |
| Discord / Telegram（`messaging/` 约 4k 行）+ Whisper 语音 | 超出额度网关定位 |
| 61 家 provider 的静态目录 | Freeport 的五源发现 + 签名 feed 是更强的方案；静态目录的维护成本会反噬 |
| 11 个 agent launcher | 常驻网关不需要拉起 agent |
| 本地 Ollama / LM Studio provider | 红线：不用本地推理掩盖额度耗尽 |
| RTK / Headroom 压缩 | 已列 P2，按「调外部进程」实现，不内联 |
| free-router 的简单有序列表排序 | Freeport 的三层 Gate/Order/Pick + SWRR + affinity 更强，不要回退 |

### 一个例外：`freeport run <agent>`

FCC 的 launcher（`cli/launchers/runner.py`）本质是「确保网关在跑 → 构造 env → exec → 退出清理」。Freeport 若加一个 `freeport run claude`，就等于提供了一个**不改用户全局配置的侵入模式**——对「只想试一下」的用户比注入更有吸引力。属于 P2，不阻塞。

---

## 8. 实施顺序

### 批次 A′：接入面（5.5 人日）

0. **`Strategy` 从「写哪个文件」升级为「接入模式」，`inject` 增加 `Catalog` 入参**（清单 15）— 2d
1. Claude Code env 全集 + 旧变量清理 + 分档模型（清单 1、2）— 1d
2. `/v1/models` 双形状 + `claude-*` 别名（清单 3、4 的最小闭环）— 2.5d

**为什么 0 排在最前**：Push 模式要写的 `vision` / `toolCalling` / `maxInputTokens` / `maxOutputTokens` 只有目录知道，而 `inject` 现有的 5 个 `Src` 变体一个都拿不到；`Strategy` 还是 `&'static`，装不下运行时序列化。先改签名，此后每加一个客户端只是加一条数据；先铺条目，每加一个 Push 类客户端（VS Code Chat、Codex）都得回头改网关。

1 和 2 仍然必须一起：不做 1 只做 2，400 会从 haiku 档挪到模型名上；不做 2 只做 1，模型选择器是空的。

### 批次 B：可用（8.8 人日）

3. 429 三分法 + 配额回填 + 配额日边界（清单 9、10）— 1.8d
4. reasoning 历史带内封装 + 四档流式恢复（清单 8、7）— 5d
5. 能力门禁 + 出站清洗（清单 11、14）— 1d
6. 会话头 + 全冷却回退（清单 5、12）— 0.5d
7. thought_signature 哨兵（清单 13，仅当 8 未覆盖 Gemini 时）— 0.5d

### 批次 C：覆盖（6.5 人日）

8. Push：VS Code Chat（清单 17）+ 能力声明写入目录（清单 6）— 2.5d
9. Pull：Claude Desktop 独立目录（清单 18）— 1d
10. Host：JetBrains ACP，只做「有则注 env、无则给指引」（清单 19）— 0.5d
11. Redirect 条目补齐 9 个（清单 20）— 1.5d
12. 配置分层 + 墓碑（清单 16，Pro 前置）— 1d

批次 C 的顺序按「依赖模式」排，不按 IDE 知名度排：第 8 项做完，第 11 项那 9 个条目每个约半小时；第 10 项永远只值 0.5 天，因为 ACP 的能力上限由第三方 adapter 决定，Freeport 做什么都突破不了。

---

## 附：参考坐标

```text
free-claude-code
  application/routing.py:36-41,263-269      档位子串匹配
  application/execution.py:435-570          候选循环 + 空闲预算
  core/stream_recovery.py:62-106            四档恢复判定
  core/delivered_response.py:384-450        DeliveredPrefix
  core/history_replay.py:40-47,425-506      origin.accepts + prepare_history
  harnesses/claude.py:12-27                 环境变量全集
  harnesses/claude_integration.py:15,73     VS Code settings 注入
  harnesses/vscode_chat_integration.py:26-55 模型组结构（Push 模式的能力字段）
  harnesses/claude_desktop_integration.py:24-47  Claude-3p 目录 + X-FCC-Model-View
  harnesses/jetbrains_acp_integration.py    ACP：寄生已装 adapter、只注 env
  config/model_refs.py:24-37                ModelFallback 结构

free-router-proxy
  server.mjs:1592-1764                      能力门禁 + 候选过滤
  server.mjs:1668-1714                      一次失败三个决策
  server.mjs:1766-1782                      出站清洗
  quota.mjs:1-170                           配额解析 / 永久拒绝 / 日边界
  thought-signature.mjs:43-60               签名缓存 + 哨兵
  config.mjs:343-374,499-520                分层合并 + 墓碑
  ui.mjs:94-140                             写入校验 + 原子 0600
```

## 9. 落地进度（2026-10-19）

清单已经开工，按依赖顺序推进。代码为准，与上文设计有出入的地方都写在备注里。

| 清单 | 状态 | 实际落点 | 与设计的差异 |
|---|---|---|---|
| 1、2 | 完成 | `inject/src/spec.rs` 的 `CLAUDE_KEYS` 3 → 13 项；新增 `Prune` 断舍离 | 顺手清了 `ANTHROPIC_` 前缀下的旧键——留着会和 `ANTHROPIC_AUTH_TOKEN` 打架 |
| 3、4 | 完成 | `gateway/server.rs::list_models` 双形状；`route.rs` 的 `ALIAS_PREFIX` / `aliases()` / `resolve_alias()` | 别名保留 `provider:model` 的冒号，脱壳后正好是 `plan` 的显式点名语法，省掉一层反查表 |
| 8 | 完成 | 新增 `gateway/src/sig.rs`：`fp1:` + base64url，无状态带内封装 | 不做 FCC 的 `destination.accepts(origin)`：上游一律 OpenAI 兼容，带上 `extra_content` 不会被拒，丢了才是 400 |
| 9 | 完成 | `core/src/error.rs::parse_quota_signal`；`quota.rs::learn` 只收紧 | 不读响应头（免费厂商基本不返 `x-ratelimit-*`），只读 429 body 里的 quota 事实 |
| 11 | 完成 | `route.rs::plan_with` + `Needs::of_chat` + `caps_of`；`probe::health::deny_cap` | 三态而非二态：声明 `false` 排除、`None` 放行但 0.6 折价、`true` 正常。未知不等于不支持，一律排除会让大多免费模型没法用 |
| 15 | 完成 | `inject/src/spec.rs` 的 `Strategy` / `Discovery` 两个正交维度；`push.rs` | `Push` 单列而不塞进 `Strategy`，否则会出现 `TomlBlock` 与 `Redirect` 并列的分类错位（Codex 是 TomlBlock + Push） |
| 5 | 完成 | `server.rs::session_key` 增加 `x-claude-code-session-id` | 与两个自有头平级：Claude Code 自己会带，等于白送一个会话信号，不用就只能退回内容指纹 |
| 12 | 完成 | `route.rs` 全冷却回退；`RoutingConfig.fallback_when_all_cooling` / `fallback_horizon_ms` | 三条收窄：**只回退窗口内**（默认 30s，日额度耗尽那几小时撞了白撞）、**只放一个**、**标 half_open**（成功即解锁，失败按一次正常熔断）。账本耗尽与能力不匹配不走这条路——它们是硬事实，不是「等会儿就好」 |
| 14 | 完成 | 新增 `gateway/src/sanitize.rs`；`providers::{create_chat,list_models}` 出口统一过一遍 | 脱敏只收**当前凭据**的字面量（扫全库会给无关内容打码）；`<8` 字符的值不参与，否则 cookie 里的 `zh`/`true` 会把正文打成马赛克；先算 `retry-after` 再脱敏，展示与判定分开 |
| 17 | 完成 | `push.rs` 补 `apiType` + 逐模型 `requestHeaders`；`spec.rs` 新增 `vscode-chat` 条目 | 组级 `url` **不写**——写了 VS Code 会走「自己发现模型」分支，这组就白写；组级 `apiKey` 也不写，它被当成 secret-store 引用。目录为空时直接报错，不写一组 0 个模型进去 |
| 7 | 部分（只做 STOP） | `server.rs::stream_response`：收尾判定 `stream_complete` + 断流补一帧 SSE error | RESTART / CONTINUE / HANDOFF 三档都要维护「已交付前缀」，与「网关无状态」正面冲突，暂不做。判定双保险：`[DONE]` 或非空 `finish_reason`；`finish_reason:null` 是「还在生成」，不算收尾 |
| 19 | 完成 | 新增 `inject/src/envs.rs::acp_servers`；`Strategy::AcpEnv`；`spec.rs` 增加 `jetbrains` 条目（`Discovery::Host`） | 只注 env、**不建条目**：`command` / `args` 指向每台机器不同的 node 与 adapter 脚本，猜一个写进去，IDE 只会把错误吞进日志。挑条目三档收敛（我们的标记 → 名字/args 含 `claude` → 报错并列出已有条目）；名字带 `/` `~` 的条目直接跳过——`json_edit` 不做 Pointer 转义 |
| 2.7.1 | 完成 | `envs.rs::vscode_extension` + `claude-code-vscode` 条目（`Strategy::EnvList`） | 一份 `CLAUDE_KEYS` 复用到三个落点（CLI 的 `env` 对象、扩展的 `{name,value}` 数组、ACP 的 `env` 映射），以后加变量只改一处。数组按 `name` 认领，用户自己加的 `HTTP_PROXY` 留在原位 |
| 20 | 完成（9/9） | `kilo` / `goose` / `workbuddy`（Guide）补齐 §6 Redirect 九客户端：Cline / Roo / Kilo / OpenCode / Aider / Goose / Crush / Continue / WorkBuddy | Kilo 的配置是 `kilo.jsonc`——JSONC 带注释，`serde_json` 解析会丢注释，不能代写；但它的 Base URL 填了就会自己拉 `/v1/models`，步骤只剩「Providers → Custom provider」两步。Goose 干净的路是 `custom_providers/*.json`（`api_key_env` 引用环境变量，key 不落盘），但 `models` 数组必须写死——写死就脏，与 Crush 只写 `discover_models: true` 的取舍一致，给步骤。WorkBuddy 走应用内连接器配置，无可改写文件 |
| 18 | 完成 | 新增 `inject/src/desktop.rs`（`Strategy::Desktop`）；`journal::Entry.files` + `FileState` | **多文件**是这一项的真难点：profile / `_meta.json` / `deploymentMode` 三处必须一致，单文件的 `prev` 表达不了，于是给日志加了「每文件一份 before + 备份」；重复注入沿用第一次的 before（否则回滚会回到「已注入」）；`_meta.json` 结构不符、或我们的 id 被别家档占用时**直接停手**；`claude_desktop_config.json` 里还存着 MCP，回滚按备份字节还原，不重新序列化 |
| 16 | 完成 | `SCHEMA_VERSION=2` + `AppData::migrate()`；`Tombstones{providers,accounts}`；`transfer::apply` 合并时跳过墓碑 | 迁移是**加法式**：只修「有值但没意义」的字段（`fallback_horizon_ms=0`）与版本号，用户值一律不动；配置版本高于本程序时**不迁移**、只警告（迁移表只会往前走，往回跑等于把新版字段抹掉）。墓碑存时间是为了以后能比「谁删得更晚」；实体与墓碑同时存在时以实体为准（「删了又加回来」的残留会让下次同步把它再删一遍） |
| 6 | 完成 | `src-tauri::model_cards` 把目录能力（`freeport.caps`）折成 `ModelCard`，`push.rs` 再序列化进模型组 | 三态沿用门禁那套：`tools` 只在明确 `false` 时关（未知放行），`vision` / `reasoning` 只在明确 `true` 时开。客户端不会来问网关，写错就是「模型在选择器里消失」或者「用不了却看得见」 |
| 17 补充 | 完成 | `Agents.tsx` 展示 `Discovery` 四模式标签；`bridge.ts` 类型补 `discovery` / 新 strategy | `· json` / `· acp` 是给开发者看的，用户只关心「它自己发现模型，还是要我写表」 |

能力门禁的自我修正链路值得单独记一笔：真实请求被 400 拒 → `classify` 给出 `capability` → `note_fail` 写进 `model_health.caps` → 下一单同类请求在 Gate 就被拦。**声明会过期，实测不会**，这是唯一能自愈的那条路。

顺带修了一个会让门禁失效的旧 bug：`gemini.rs::to_openai` 以前把 `inlineData` / `fileData` 整段丢掉，图片静默消失——上游只收到一句「这张图里是什么」，门禁也看不见图。现在转成 `image_url` 分块（`inlineData` → data URL，`fileData` → URI），非图片的 `fileData`（PDF / 音视频）仍然丢弃，不冒充图片。

### 收官（2026-10-19 深夜）

20 项清单全部处置完毕：**18 项完成，2 项有意不做**——

- **清单 7 只做 STOP 档**：RESTART / CONTINUE / HANDOFF 都要维护「已交付前缀」，与网关无状态原则正面冲突（详见 §9 该行）。
- **清单 13 不做**：thought_signature 哨兵的全部职责已被清单 8 的 `fp1:` 带内封装覆盖（无状态、跨协议回放），再做一层哨兵是重复状态。

18 项「完成」里有 5 项最终形态是 Guide（给步骤不代写）：Zed / Aider / Continue / Kilo / Goose / WorkBuddy 中的一部分。这不是偷懒——JSONC 注释、YAML 锚点、GUI-only、写死模型表会脏，每一行的备注里都写了不能代写的具体理由。可代写的都代写了：Claude Code 三条路径（CLI / VS Code 扩展 / ACP）、Codex、Cline、OpenCode、Crush、VS Code Chat、Claude Desktop。

