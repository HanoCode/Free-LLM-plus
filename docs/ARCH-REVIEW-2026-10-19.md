# 架构复核：免费 LLM 中心该长什么样

> 输入：公众号《magpie 和 CC Switch：网关派与配置派》（2026-10-09）+ 本仓库 `docs/ARCHITECTURE.md` v1.3 + 当前代码实测。
> 输出：一份「哪些设计已经对、哪些必须在 M2 前改」的判断。不改现有文档，供并入 v1.4 时取用。

---

## 0. 结论先行

| 问题 | 结论 |
|---|---|
| Freeport 该走文章里的哪条路 | **网关派（magpie）**。配置派的上限是「切 key 方便」，做不出「额度池」。现有选型正确，不必动摇。 |
| 现有架构最大的缺口 | **成功判定点错位**：流式请求在收到上游 200 头时就 `note_success` + `ok=true`（`server.rs:1117-1120`），而免费额度的失败大量发生在 200 之后。账本因此持续写入假阳性，越跑越偏。 |
| 第二缺口 | **流式是纯 passthrough**（`server.rs:1246-1283`），上游中途断流只能给客户端塞一个 SSE error，无法 failover。这是「免费额度一天崩八次」的直接来源。 |
| 第三缺口 | **缺 `freeport-inject` crate**。crate 只有 core / gateway / catalog / probe；注入没有代码边界，M2 一定会挤进 `server.rs` 或 `src-tauri`。 |
| 「中心」语义缺什么 | **虚拟 key（per-agent 配额）**。多 agent 共享一个池子时，一个 agent 能把当天额度吃光，且没有公平性可言。magpie 的 `gateway key 限额` 就是这一层。 |
| 最不该抄的 | magpie 的 Claude 订阅桥接（`--tools ""` + MCP helper，用真实 CLI 进程保住订阅身份）。文档红线已经排除，**这是相对 magpie 的结构性劣势，但要守住**。 |
| 一句话架构 | **账本先于路由，判定晚于响应头，注入是事务不是写入。** |

---

## 1. 文章给出的三个事实

不是观点，是可以直接约束设计的实证。

### 1.1 两条路线的真实分野不是「有没有代理」

| | CC Switch | magpie |
|---|---|---|
| 它守护的对象 | 你的**配置文件** | 你的**额度** |
| 配置的角色 | 数据库，逐字节保真 | 指针，只负责指向 `127.0.0.1:3425` |
| 代理的角色 | 按工具打开的增强，默认关闭 | 唯一入口，没有旁路 |
| 失败时换谁 | 按你排的 P1/P2/P3 队列 | 按每个账号的额度窗口自动决定 |
| 网关挂掉的后果 | 退回直连，还能用 | 所有 agent 全断 |

**对 Freeport 的意义**：产品定位是「额度聚合」，所以必须是网关派。但 magpie 那条「网关挂掉全断」的代价，Freeport 不能靠配置双写去补——那会让配置漂移，违背注入的可逆性。正解是**进程级自愈**（daemon + launchd/systemd KeepAlive，崩溃秒级拉起），把可用性押在「进程会被拉起来」而不是「配置里有备胎」。

### 1.2 两个产品在同一处收敛

两边都独立演化出了：三态熔断、失败按作用域降级、只在响应开始前切换、会话粘性。说明这四件事不是风格选择，是这类产品的**刚需结构**。ARCHITECTURE.md 的 §3.6 已经全部覆盖，方向正确。

### 1.3 magpie 的两个机制是免费额度场景的真解

**① holdWriter：把上游回复扣在缓冲区，确认正常才放行。**
- 15 秒未放行先发 SSE 注释行保活（防 Cloudflare 按空闲断连），此时仍可换候选
- 思考内容最多扣 4 分钟（模型可能想完再拒答）
- 最后一个候选不再扣

**② 失败分类是「状态码 + 错误文本正则」双判，不是单看状态码。**
文章直接给出了中文正则里的关键词：余额、欠费、额度、套餐、限额已用完。配套的停用时长：余额 30 分钟 / 配额用到窗口重置（最长 8 天）/ 限流按 Retry-After 翻倍 / 模型不在套餐只停这个 key 的这个模型 / 安全拒答谁都不停。

**为什么这对 Freeport 特别重要**：免费厂商最常见的失败形态是 `200 OK` + 一段正常 SSE + 中途一条 `{"error":{"message":"free tier exhausted"}}`，或者干脆 200 + 空 content。按状态码分类会把这一类全判成成功。

---

## 2. 现状对照

### 2.1 已经对齐的（不用改）

| 设计 | 位置 | 评价 |
|---|---|---|
| 单二进制双形态，网关是 lib | `crates/freeport-gateway` | 比 magpie 只做 GUI 更彻底，daemon 移交已落地 |
| 三层可解释路由 Gate → Order → Pick | `core/route.rs` | Gate 已实现；Order/Pick 留了函数边界，补算法不动调用方 |
| 半开只放一个试探 | `route.rs:139` | 与 cc-switch / magpie 一致 |
| 巡检分级 + 预算化 + 让路真实流量 | `probe/{budget,patrol}.rs` | 「探针烧额度」这个坑避开了，比多数同类产品强 |
| 注入六步 + CAS 重读比对 | ARCHITECTURE §3.8 | 比 CC Switch 更严（它在重读与 rename 之间承认有窗口） |
| 目录跟随 models.dev + 自建 L1 情报 | `catalog/modelsdev.rs` | 避免重复维护上千模型元数据 |

### 2.2 结构性缺口

| # | 缺口 | 代码证据 | 后果 |
|---|---|---|---|
| 1 | 200 头即判成功 | `server.rs:1117-1120`，`stream` 分支之前就 `note_success` | 冷却永不触发；`model_health` 把死模型记成健康；路由持续把流量送进黑洞 |
| 2 | 流式无法 failover | `server.rs:1246-1283` 逐 chunk 转发，`Err` 只发 SSE error | 用户看到半截输出 + 报错；而这次失败甚至不算失败 |
| 3 | 无 Canonical IR | `server.rs:1090-1094` 直接改 `payload["model"]` 转发 | 加 Anthropic / Gemini 入口时必然写 N×M 转换器；request/response 的 quirk 无处安放 |
| 4 | 无额度账本 | 只有 `cooldowns` + `model_health` | 无法回答「今天还剩多少」；只能被动等 429，不能主动预算 |
| 5 | 无虚拟 key | 单 `Freeport Key`（`authorized()`） | 多 agent / 多机共享时无公平性与隔离 |
| 6 | 无 affinity | `plan()` 无 sessionId 入参 | prompt cache 命中率低；跨跳换模型导致上下文漂移 |
| 7 | 无 inject crate | `ls crates/` 只有 4 个 | 12 个注入器没有边界，会污染 `src-tauri` |
| 8 | 错误分类只看状态码 | `core/error.rs` + `classify(status, body, retry_after)` | 200 体里的错误、余额不足文案全漏判 |

> 注：`classify` 已经接收 `body`，说明设计上留了口子。缺的是文本分类规则与「200 + 错误体」这个分支。

---

## 3. 免费 LLM 中心的第一性原理

这三条决定了架构形状，不是实现细节。

### 3.1 额度是易腐商品，账本必须先于路由

免费额度的特征是**按窗口过期、不可结转**。所以最优策略不是「选最好的模型」，而是「优先花最先作废的额度」。magpie 的 smart 排序（最大窗口最早重置排第一）就是这个道理，ARCHITECTURE §3.6 的 Pick 层已经写了同样的规则。

**但账本必须是结构化的四窗口**（rpm/rpd/tpm/tpd × 每窗口 `used/limit/reset_at`），且在请求**发出前** reserve、**结算时** release。只靠 429 事后反应，等于每次都先把配额撞到墙上。

推论：**路由是账本的下游，不是并列关系**。`plan()` 的签名应该从「拿 AppData 算候选」变成「拿账本快照算候选」。

### 3.2 失败长在 200 之后，判定点必须后移

见第 0 节表格与首图。判定点的正确位置是**首个有效语义块**（text / reasoning / tool_call 任一），不是响应头。在它之前，一切都是可撤销的试探。

这条直接决定了 `server.rs` 里 `chat()` 的函数形状：现在是「拿到 upstream → 立刻记账 → 分流」，必须改成「拿到 upstream → 进 Hold → 判定 → 记账 → 分流或回环」。

### 3.3 配置是用户的资产，网关是自己的负债

CC Switch 的 `patch/mod.rs` 注释记着 v3.11.0 曾因解析失败从空文档写起、清空了用户配置。这类事故一次就毁掉信任。

ARCHITECTURE §3.8 的六步流水线（Detect → Plan → Backup → Lock → Write → Verify）已经比 CC Switch 更严。要保持，并且**不要为了「网关挂了还能用」去写第二份 provider 配置**——那会让「只改自己写的节点」这条不变量失效。可用性用进程自愈解决。

---

## 4. 目标架构

### 4.1 分层（不变的部分）

```
┌─ Host ────────────────────────────────────────┐
│  GUI (Tauri)        │  Daemon (--headless)     │
└─────────┬───────────┴──────────┬───────────────┘
          │ invoke               │ 同进程
┌─────────▼──────────────────────▼───────────────┐
│ Control Plane: catalog / probe / inject / creds │
└─────────────────────┬──────────────────────────┘
                      │ 共享状态（账本、目录、凭据）
┌─────────────────────▼──────────────────────────┐
│ Data Plane: ing → IR → gate → order → pick →    │
│             dispatch → hold → render → settle   │
└─────────────────────┬──────────────────────────┘
                      │ adapter (per provider quirk)
                 上游厂商
```

### 4.2 请求生命周期（有变化的部分，标 ★ 的为新增）

```
Ingress(virtual_key, path)
  ★ 0. Key Gate     虚拟 key 的 rpm / 模型白名单 / 份额
    1. Parse        按路径选协议 parser → CanonicalRequest
    2. Resolve      auto / provider:model / alias → 候选链
    3. Gate         能力门禁 + 凭据 + 账本余量 + 三态熔断  → 候选集 + 拒绝原因
    4. Order        SWRR（权重档）
  ★ 5. Affinity     命中 sessionId 且 cached_tokens ≥ 1024 且缓存未过期 → 提到队首
    6. Pick         档内：最早刷新 → 余量最多 → 8% Thompson
    7. Dispatch     adapter 构造 wire 请求 + reserve 配额
  ★ 8. Hold         缓冲到首个有效语义块
                       ├─ 15s 未就绪 → 发 SSE 注释保活，仍可换
                       ├─ 有效     → 放行，进入 9
                       └─ 无效     → 10
    9. Render       边转发边翻译成客户端协议；首个 chunk 后 disarmHedge
 ★ 10. Classify     状态码 + body 文本正则 + 响应形态（空流/中途断）
                  → scope(key/model/provider) + cooldown + 是否学限额
                  → 未提交则回 4（预算内），已提交则只能如实上报
   11. Settle       结算 tokens、更新 Beta 后验、清失败窗口、写 trail
```

**8 和 10 是这次要补的全部。** 其余节点在 ARCHITECTURE 里都有定义，只是代码没铺开。

### 4.3 判定矩阵（Hold 阶段的放行条件）

| 上游形态 | 判定 | 动作 |
|---|---|---|
| 200 + 文本增量 | 有效 | 放行 |
| 200 + `reasoning` 增量 | 有效 | 放行（思考也算已提交，但思考后拒答要能继续扣） |
| 200 + `tool_call` 增量 | 有效 | 放行 |
| 200 + 空 content + `finish_reason: stop` | 无效 | 换下一跳 |
| 200 + SSE 里夹 `error` 事件 | 无效 | 换下一跳 + 按文本分类 |
| 200 后连接断开，无任何语义块 | 无效 | 换下一跳 |
| 首个有效块后断开 | 已提交 | 如实上报 `stream_broken`，记失败但不重放（避免重复计费与重复工具调用） |
| 30s 内无任何 chunk | 视同未提交 | 换下一跳（预算内） |

---

## 5. 五项必改（按 ROI 排序）

| P | 改动 | 落点 | 为什么现在做 |
|---|---|---|---|
| **P0** | **Hold + 判定点后移** | `server.rs` 拆出 `hold.rs` | 不改，账本与冷却全部建立在假数据上，后面所有算法都白做 |
| **P0** | **错误分类加文本规则 + 200 体分支** | `core/error.rs` 扩 `classify` | 免费额度的主要失败形态就是 200 + 错误文本；成本最低、收益最大 |
| **P1** | **四窗口账本 + reserve/release** | `core/quota/`（新模块） | 「免费中心」的核心资产；没有它就无法解释「今天还能用多少」 |
| **P1** | **拆出 IR + translate 目录** | `gateway/translate/{openai,anthropic,gemini}.rs` | `server.rs` 已 1383 行，再塞 Anthropic 入口会失控；IR 是唯一能压住 N×M 的结构 |
| **P2** | **虚拟 key + 份额** | `server.rs` 鉴权层 + `keys` 表 | 多 agent 共享的前提；也是将来多机同步的前置 |

延后但不遗忘：`freeport-inject` crate（M2 建，哪怕只有 `claude_code.rs` 一个文件，也要先有边界）、affinity（依赖 sessionId 提取，需要 IR 先落地）。

---

## 6. 明确不做的

| 不做 | 理由 |
|---|---|
| Claude Pro/Max 订阅桥接 | 文章对 magpie 这块的定性是「有意绕开平台的订阅资格检查」。与红线冲突；且这种做法的寿命取决于上游风控，不适合做常驻产品的地基 |
| 用「配置双写」兜网关故障 | 破坏注入不变量；用 daemon + KeepAlive 解决可用性更干净 |
| 追求 provider 数量 | 9router 的 85 个 apikey 里真正可用的不到 30 个。候选池 + canary（§3.5.2）比 registry 广度重要一个量级 |
| 全局 default 覆盖目录字段 | magpie 曾为修一个 DeepSeek 问题给所有模型设 272K 上限，把 Claude 的 1M 一起卡住。目录覆盖必须**按 provider 逐条**，禁止广播式默认值 |
| 局域网共享 | 现有决策已排除。虚拟 key 落地后再回来看，成本会低很多 |

---

## 7. 三项决策（2026-10-19 已定）

### 7.1 Hold 的缓冲上限：不设固定值，按「还有没有下家」+ 两段预算

**第一原则：只有后面还有候选时才 Hold。** 当前是最后一个候选，直接放行——没得换的时候扣住只是白等，magpie 自己也是这么处理的。

两段预算，不是一个超时：

| 参数 | 值 | 触发 | 动作 |
|---|---|---|---|
| `holdSoftMs` | 8000 | 未拿到语义块 | 发 SSE 注释保活 + **启动 hedge**（并发发下一跳） |
| `holdHardMs` | 45000 | 仍未提交 | 放弃本跳，转下一跳（与现有 `request_budget_ms` 对齐） |
| `keepAliveMs` | 12000 | 未提交期间 | 每 12s 发 `: freeport-keepalive`，防 Cloudflare 100s 空闲断连 |
| `commitOn` | text / reasoning / tool_call | 任一出现 | 立即 disarm，之后零缓冲开销 |
| `holdBufferMax` | 64 KiB | 提交前累积超限 | 直接放行（能疯狂输出说明不是错误） |
| 非流式 | — | — | 不 Hold，读全 body 判定（现有逻辑正确，保持） |

**为什么不抄 magpie 的 4 分钟**：magpie 的场景是几十个订阅账号，换号成本≈0，扣住 4 分钟换一次「悄悄重试」是划算的。Freeport 的场景是免费额度——另一家大概率一样慢，扣 4 分钟换来的是用户干等 4 分钟。**静默 45s 就换家是对的，但必须先 hedge 再等，不能干等。**

**为什么是 hedge 而不是「等到 8s 才发下一跳」**：45s 全用来等一个静默上游，等于把整条链的机会成本全赔进去。8s 启动第二路，先到者胜，另一路 abort。代价是可能的重复计费——所以 hedge 只对**幂等的 chat 请求**启用，带 `tools` 且已有 `tool_result` 的回合禁用（避免重复执行工具）。

**对现有文档的修正**：ARCHITECTURE §3.6 写的「首字节即 disarmHedge」应改为「**首个有效语义块**即 disarm」。首字节可能只是一个空的 `data: {"choices":[{"delta":{}}]}`。

### 7.2 虚拟 key：进 v1，但只做三字段最小形态

**理由**：它是「中心」这个词的最小可实现语义。没有它，网关就是「本机加速器」——多 agent 共享时互相抢额度且无法归因。成本约 1 天（一张表 + 鉴权查表 + 日志加一列）。同时它是 Pro「多机同步」的数据前置：请求无法归因到 key，同步过去的池子也没法分配。

**只做**：

```sql
api_keys(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,              -- "claude-code" / "codex" / "default"
  key_hash TEXT NOT NULL UNIQUE,   -- sha256(明文)，不存明文
  prefix TEXT NOT NULL,            -- 前 8 位，UI 只显示这个
  rpm_limit INTEGER,               -- NULL = 不限
  daily_req_limit INTEGER,         -- NULL = 不限
  model_allow_json TEXT,           -- NULL = 全部；["groq:*","gemini:gemini-2.5-*"]
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  last_used_at TEXT
)
```

**明确不做**：key 的权重份额、跨 key 的公平调度、key 级账单。那是团队版的事，现在没有第二个用户。

**迁移**：把现有的 `gateway.auth_token` 转成一条 `name='default'` 的记录，保留 `auth_token` 字段读兼容一个版本，之后删。

**鉴权中间件的位置**：`server.rs:988` 的 `authorized()` 换成 `resolve_key()`，返回 `Option<KeyCtx>`，`KeyCtx` 带 `id/rpm_limit/model_allow`，下沉到 `plan()` 的入参。`LogEntry` 加 `key_id: Option<String>`。

### 7.3 账本：token 与请求数分开记，且**分维度独立耗尽**

已定：两者都记。关键不在「记两个数」，而在**它们是两个独立的耗尽维度**。

```rust
struct QuotaWindow {
    kind: WindowKind,      // Rpm | Rpd | Tpm | Tpd | MonthlyTokens | Credits
    used_user: f64,        // 真实流量
    used_probe: f64,       // 巡检，单列
    limit: f64,
    reset_at: i64,
    source: LimitSource,   // Config | HeaderObserved | Learned
}
```

三个来自「分开记」的具体收益，混记拿不到：

1. **RPD 耗尽但 TPM 富裕的 provider，不能服务用户，但可以巡检。** liveness 只花 3–10 token，不占新的请求数吗？占——所以规则是：RPD 耗尽 → 用户请求 Gate 掉；若 `used_probe < limit × 5%` 则仍允许一次 liveness（探针预算独立于用户额度）。这需要一个独立的探针计数器，混在 `used` 里就做不到。
2. **UI 能解释「我只发了 3 次请求，额度怎么没了」**：因为巡检花了 137 次。`used_probe` 直接展示。
3. **学限额时能分辨是哪种限额**：上游返回 429 时，用 `retry_after` 与窗口长度比对，判断是 RPM 还是 RPD 打的墙——两者的冷却策略完全不同（60s vs 到次日 0 点）。

**结算流程**：

```
dispatch 前  reserve(key, model): RPM+1, RPD+1, TPM += (input_est + max_tokens)
settle 成功  release(actual):      TPM 按 input+output 实值下调，TPD 同
settle 失败  按 scope 决定是否回滚：本请求确实被上游受理 → 不回滚（额度真花了）
窗口跨界    惰性归零（读时比对 reset_at，不做定时任务）
```

`reserve` 用 `input_est + max_tokens` 上限是刻意的：并发下不会超卖，代价是偏保守。与「学限额只收紧」一致。

**`LimitSource` 优先级**：`HeaderObserved`（上游 `x-ratelimit-remaining-*`）> `Learned` > `Config`。上游给了头就以头为准，这是唯一可信的实时值。

---

## 8. 与现有代码的接点

| 改动 | 文件 | 具体动作 |
|---|---|---|
| Hold 参数 | `core/model.rs:52` `GatewayConfig` | 加 `hold_soft_ms`/`hold_hard_ms`/`keep_alive_ms`，带 `#[serde(default)]` |
| Hold 实现 | `gateway/src/hold.rs`（新） | `enum Commit { Yes, No(reason) }`；`hold(upstream, cfg) -> Result<BufferedStart>` |
| 判定点后移 | `gateway/src/server.rs:1117-1120` | `note_success` / `ok=true` 移到 Hold 返回 `Commit::Yes` 之后 |
| 流式改为过 Hold | `server.rs:1139-1148` | `stream_response` 的入参从 `upstream` 改为 `BufferedStart` |
| 错误文本分类 | `core/error.rs` `classify` | 已有 `body` 参数未用；加 `BODY_RULES`（余额/欠费/额度/套餐/限额已用完）+ 200 体分支 |
| 账本 | `core/src/quota/`（新） | `QuotaWindow` + `reserve`/`settle`/`gate_snapshot` |
| 账本入 Gate | `core/src/route.rs:41` `plan()` | 入参加 `&QuotaSnapshot`，剔除耗尽项并给 `Rejected.reason = "quota exhausted"` |
| 虚拟 key | `gateway/src/keys.rs`（新） | `resolve_key()` 取代 `authorized()`；`Plan` 加 `key_id` |
| 日志归因 | `core/model.rs:416` `LogEntry` | 加 `key_id`、`tokens_cached` |
| 注入边界 | `crates/freeport-inject/`（新） | M2 建，哪怕只有 `claude_code.rs` |

**开工顺序**：错误文本分类（半天，零风险）→ Hold（2–3 天，收益最大）→ 账本（3 天）→ 虚拟 key（1 天）。IR 与注入边界并行，不阻塞。

---

## 9. 落地记录（2026-10-19 已完成）

按 §8 的顺序全部实现并验证，`cargo test --workspace` 全绿，`vite build` 通过。

| # | 项 | 落地文件 | 测试 |
|---|---|---|---|
| 1 | 错误文本分类 | `core/error.rs`：`BODY_RULES` + `body_error()` + `ErrorClass::soft()`；200 体、中文关键词（余额/欠费/额度/套餐/限额）、安全拒答，`status = 0` 归 Provider 作用域 | `error.rs` 单测 |
| 2 | Hold 缓冲门 | `gateway/hold.rs`（新）：`wait_commit()` 返回 `Committed{head,rest}` / `Rejected{kind,detail}`；只认 text/reasoning/tool_call 三种语义块；64 KiB 上限；最后一跳直接放行 | `tests/hold_failover.rs`（空流 + 200 藏 error 均换家成功） |
| 3 | 四窗口账本 | `core/quota.rs`（新）：`Window{used_user,used_probe,limit,reset_at}`、`reserve`/`settle`、惰性归零、`has_room`/`remaining_ratio`/`next_reset_at`；进 `plan()` 的 Gate，`Rejected.reason = "quota exhausted"` | `tests/quota_e2e.rs`（预扣 4096 被真实 12 覆盖；RPD=2 第三次被拦）+ 5 个单测 |
| 4 | 虚拟 key | `gateway/keys.rs`（新）：sha256 存储、`fp-` 前缀明文、rpm/每日限额、惰性计数；`resolve_key()` 取代 `authorized()`；`load()` 时把老 `authToken` 迁成 `key_default` | `tests/keys_e2e.rs`（老 token 仍通、每 key 配额独立）+ 4 个单测 |

**UI**：`components/QuotaPanel.tsx`（Overview 页「额度账本」，双维度进度条 + 巡检单列 + 恢复时刻）、`components/ApiKeys.tsx`（Settings 页 key 管理，明文只显示一次）。

**与文档的偏差（有意为之）**：
1. Hold 参数落到 `holdIdleMs` / `holdTotalMs` 两个（原计划的 soft/hard/keepAlive 三参数里，keepAlive 由 idle 超时后发 SSE 注释实现，未独立成参数）。
2. 账本上限用 `ProviderQuota{rpm,rpd,tpm,tpd}` 显式配置，**不做上游响应头学习**——`LimitSource` 留到 M3，因为免费厂商几乎不返回 `x-ratelimit-*`，先做会写一堆空分支。
3. 虚拟 key **未做模型白名单**：`auto` 路由下白名单语义要靠 Gate 过滤，属独立一层，v1 不做。
4. `LogEntry` 暂未加 `key_id`：请求当前按进程归因已够用，等支持多机同步（Pro）时再加。——**已在第四批补上**（见 §12）。

---

## 10. 落地记录（第二批：协议与注入）

这一批补的是「说了没做」的两块：Agents 页全 mock、Claude Code 连不进来。

| # | 项 | 落地文件 | 验证 |
|---|---|---|---|
| 5 | Anthropic 协议入口 | `gateway/anthropic.rs`（新 734 行）+ `server.rs::messages`：请求侧 `system`→messages、`tool_use/tool_result` 双向、`input_schema`→`parameters`；响应侧块化、`stop_reason`、`thinking` 块；流式按 `content_block_start/delta/stop` 重写 | `tests/messages_e2e.rs`（非流式 tool_call、流式两段文本、上游 body 形态） |
| 6 | Gemini 协议入口 | `gateway/gemini.rs`（新）+ `server.rs::gemini/gemini_models`：`contents`↔messages、`systemInstruction`、`functionDeclarations`、`generationConfig` 映射；`x-goog-api-key` 当鉴权；`:countTokens` 本地估算；`:streamGenerateContent` 转 `data:` 帧 | `tests/gemini_e2e.rs`（模型列表、非流式 functionCall、流式结束帧、countTokens 不打上游）+ 4 个单测 |
| 7 | `freeport-inject` crate | `spec.rs`（8 个 Agent 静态表）/ `json_edit.rs` / `toml_block.rs` / `diff.rs` / `journal.rs` / `lib.rs`：预览、注入、回滚、幂等、备份、原子写 | `tests/inject_e2e.rs` |
| 8 | 注入接入 UI | `src-tauri` 四个命令（`gw_agents` / `gw_agent_preview` / `gw_agent_apply` / `gw_agent_revert`）+ `Agents.tsx` 重写，删除 mock 依赖 | `tsc --noEmit` + `vite build` |

**注入里踩到的两个真问题（都已修，值得记住）**

1. **根键撞车**：Codex 的 `config.toml` 里用户可能有 `model = "..."`，而托管块也要写 `model`，TOML 直接判重复键非法。修法是把根段的 `model` / `model_provider` 暂时注释成 `#freeport-off: model = ...`，回滚时原样还原。**这一类问题只有真拿用户配置跑一遍才会暴露。**
2. **幂等时把「原值」覆盖成「我们的值」**：第二次注入读到的文件里已经有我们写的键，若照抄进 journal，回滚后就是「回滚到注入后的状态」。修法是 journal 里已有 `prev` 就只追加新指针、不覆盖旧值；备份永远保留第一次注入前那份。

**两个刻意的决定**

- **每个 Agent 单独签发一把 key**（`agent:<id>`），注入前先吊销同名的旧 key。这样 UI 能按 Agent 看用量，卸载时能精确吊销，不会攒一堆历史 key。
- **Gemini CLI 仍是 `Guide` 不代写**：它读 `GOOGLE_GEMINI_BASE_URL` / `GEMINI_API_KEY` 环境变量，写 `~/.gemini/.env` 会和它自己的 OAuth 登录态打架。协议已支持，但落盘这一步留给人。

---

## 11. 落地记录（第三批：去掉最后两处假数据 + Ollama 入口）

前两批之后，界面上还剩两个「整页假数据」：账号页的 10 条虚构账号、扩展页的两个虚构已安装插件。它们比缺功能更糟——用户会以为已经在用。

| # | 项 | 做法 |
|---|---|---|
| 9 | 账号页去 mock | 改成「一厂商一凭据」的真实视图：`providers` + `quota` + `status.cooldowns` 三份数据合成行；状态由「有没有 key / 是否冷却 / 哪个窗口耗尽 / 健康度」推出，不猜。校验按钮直接调 `probe()`。页面里明写「同一厂商多账号还没做」及原因（密文按 provider 建索引，要改就得连路由候选一起改）。 |
| 10 | 扩展页去 mock | Extension Host 在 v1.5，所以「已安装」清空为计划中状态，删掉虚构的 96M / 2.1G 资源数字；只保留安装四规则与能力边界（这两部分是已定约束，不是实现）。 |
| 11 | 侧边栏计数 | 删掉 `14 / 11 / 10 / 4 / 2` 五个硬编码数字，改成 models / providers / 凭据数 / 已注入数实时计算；没有数据就不显示，不拿占位数字充数。 |
| 12 | Ollama 兼容 | `gateway/ollama.rs`（新）：`/api/tags`、`/api/chat`、`/api/generate`、`/api/version`。NDJSON 流式、`stream` 默认 true、`done: true` 收尾；`options.num_predict`→`max_tokens`，`format: json`→`response_format`，`images`→`image_url`。 |

**为什么 Ollama 值得做**：它是唯一「不发鉴权头、不说 OpenAI 协议」的入口（Zed、部分 JetBrains 插件）。`/api/tags` 复用 `/v1/models` 的聚合结果，模型名回传就是 `provider:model`，能原样喂回路由，不需要额外映射表。

**顺带删掉的死数据**：`lib/data.ts` 里的 `Account` / `accounts` / `Agent` / `agents` / `Extension` / `extensions` 共约 240 行。留下的只有 `providers` / `models` / `logs` 三份——它们仅用于浏览器预览的兜底，桌面端不会读到。

**这一批之后，界面上不再有「显示但不存在」的东西。** 剩下的都是明确标注的「计划中」（扩展、多账号）。

---

## 12. 落地记录（第四批：Responses 入口，协议矩阵补齐）

上一批结尾写着「剩下的是明确标注的未实现（Responses）」。这一批把它补掉——它是矩阵里唯一缺的协议入口。

| # | 项 | 做法 |
|---|---|---|
| 13 | `POST /v1/responses` | `gateway/responses.rs`（新 700 行）+ `server.rs::responses_entry`：请求侧 `instructions`→system、`input` item 数组摊平成 messages（`function_call` 连续多个合并进同一个 assistant 轮次、`function_call_output`→tool 消息、`reasoning` 的 `summary[].text` 挂到下一个 assistant 的 `reasoning_content`）、`tools` 从扁平 `{type,name,parameters}` 换成 `{type,function:{...}}`；响应侧组装 `output` 数组并逐块发语义事件 |
| 14 | 日志按真实入口归因 | 四个 adapter 都往内部头 `x-freeport-path` 写自己的路径，`chat()` 用它填 `LogEntry.path`。之前 Anthropic / Gemini / Ollama 进来的请求全被记成 `/v1/chat/completions` |
| 15 | 日志按 key 归因 | `LogEntry` 加 `key_id` + `key_name`（**名字随日志存快照**，key 以后被删这条记录仍读得懂）；日志展开行把「协议入口 + key 名」显示出来，替掉原来那个写死的 `X-Freeport-Trail` 标签 |

**流式事件的顺序是有讲究的**：`response.created` → `response.in_progress` → `output_item.added` / `content_part.added` → `output_text.delta` → `output_text.done` / `content_part.done` / `output_item.done` → `response.completed`。收尾时按 `output_index` 升序关块，客户端才能把交错的 reasoning / text / function_call 拼回正确顺序。上游在流里塞 `error` 时发 `response.failed`，不是把错误当内容透传。

**四条保守决定**

1. **不实现 `previous_response_id`**：网关无状态，Codex 每轮都会把完整历史放进 `input`；假装接住这个字段会让下一轮悄悄丢上下文，比报错更糟。
2. **hosted tools 直接丢弃**：`web_search` / `file_search` / `local_shell` 没有 `name`，透传会让上游收到无名函数，Gemini 这类严格校验的厂商直接 400。
3. **不透传 `reasoning.effort` / `parallel_tool_calls`**：Codex 每次都会带这两个字段，而严格校验的兼容端点会因为多一个不认识的键直接 400——400 会触发换家、白烧一次额度。少一个旋钮比多一次失败划算。
4. **Codex 注入默认仍写 `wire_api = "chat"`**：chat 链路已实机验证，Responses 入口先给手动配置的用户；改默认可逆，等 Responses 在真实 Codex 会话跑通再切。

验证：`tests/responses_e2e.rs`（非流式 tool_call 组装、流式 reasoning + text 事件、hosted tool 被丢弃、日志 path 归因）+ 5 个单测。

## 13. 落地记录（第五批：一厂商多凭据，额度按凭据算）

前一批把协议矩阵补齐了，剩下最大的一块是**多账号**。它不是加一行 UI 的事：v1 的密文按 provider id 建索引，冷却键是 `p:provider`，账本键也是 provider，三处都把「厂商」当成了记账单位。所以这轮把粒度整体下沉到**凭据（Account）**。

| # | 项 | 做法 |
|---|---|---|
| 16 | `Provider.accounts: Vec<Account>` | 每条凭据 `{id, name, key_mask, enabled, added_at, quota}`。`Provider.quota` 降级为「旧配置迁移来源」 |
| 17 | 旧配置无损升级 | `secrets::migrate_accounts`：**凭据 id 直接沿用 provider id**，`quota` 从 provider 搬到 account。`secrets.json` 里的条目一个字节都不用动，老用户升级后 key 还在 |
| 18 | 候选粒度 = 凭据 | `Candidate { provider, account, model_id, half_open }`；`plan()` 的 `has_key` 闭包从 `&Provider` 改成 `&Account`，在 `live[idx]` 里按凭据摊开 |
| 19 | 冷却三级键都带凭据 | `k:account`（限流/欠费/key 失效，**只挡自己**）、`m:provider:model`、`p:provider`（5xx/连不上，全家连坐）。单测 `key_scope_is_isolated_between_accounts` 专门钉住这条 |
| 20 | 账本按凭据 | `Ledger` 的键就是 account id；删凭据时 `Ledger::forget(account)` 顺手清四个窗口，不留幽灵账 |
| 21 | 粘性精确到凭据 | `Stick { provider, account, model, at }`；`stick_to_front` 三级回退：同凭据同模型 → 同凭据任意模型 → 同厂商 |
| 22 | SWRR 轮转键 | 从 `provider:model` 改成 `account:model`。同一厂商的两条 key 是两条独立通道，各自参与加权 |
| 23 | 跳过粒度 | `skipped: HashSet<(usize, String, String)>`。**Key 级失败只跳过该凭据，Provider 级失败才跳过整家**——这一条写错的表现是「A 被 429 之后 B 明明可用却也被跳过」 |
| 24 | 日志归因 | `LogEntry` 加 `account_id` / `account_name`（同样是快照），`Attempt.account` 记录每一跳用的哪条 key |
| 25 | 命令层与界面 | `gw_add_account` / `gw_remove_account` / `gw_patch_account`；Accounts 页重写成「按厂商分组、按凭据出行」，每行有自己的四窗口条、开关、限额、删除 |

**顺手修掉的序列化 bug**：`GwWindow` / `GwQuota` 缺 `rename_all = "camelCase"`，后端吐的是 `used_user` / `reset_at`，前端读的是 `usedUser` / `resetAt`，额度条一直在算 `NaN`——因为「没配限额时进度条本来就留空」，这个 bug 一直没被看见。多凭据后每行都要画四条，躲不掉了。

**为什么不是「按厂商加总」**：同一家两把 key 在上游是**两本独立的账**，429 也是分开给的。按厂商加总会让「还剩一半」变成一句假话：A 已耗尽、B 全新，加总后看起来余量充足，Gate 放行，然后请求打到 A 上继续 429。

验证：`tests/accounts_e2e.rs`（A 被 429 → 同轮换 B；第二次只一跳直奔 B；断言 `account_id == "acct_b"`）+ `tests/accounts_quota_e2e.rs`（两条 key 各 `rpd = 1`，各自打满后本地 503）+ `cargo test -p freeport-core -p freeport-gateway` 全绿。

## 14. 落地记录（第六批：校验下沉到凭据）

上一批把额度、冷却、账本、粘性都切到了凭据粒度，但**校验**还留在厂商粒度：`probe(provider)` 只挑第一条有 key 的凭据打 `/models`。于是界面上出现一个自相矛盾的状态——每行凭据都有自己的额度条，却共用一个校验按钮。

| # | 项 | 做法 |
|---|---|---|
| 26 | `Account::health` | 新增逐条凭据的健康快照（`status / latency_ms / checked_at / model_count / error`） |
| 27 | `probe_account(provider, account)` | 用**指定那条 key** 打 `/models`，结果只写它自己的 `health`；`probe(provider)` 退化成「挑第一条启用且有条目」的特例 |
| 28 | 厂商级健康改为汇总 | `Provider::sync_health()`：还有一条 healthy 就是 healthy，全挂才取最近一条的 `error`。三个单测钉住：一条挂不影响另一条、全挂才报错、从没测过就保持 `unknown` |
| 29 | 新建凭据自带健康 | `add_account` 本来就为「校验通过才入库」打过一次 `/models`，顺手把这次结果和延迟记进 `health`——新行不该显示「待探测」 |
| 30 | 界面 | 每行一个校验按钮（图标）；卡片头改成「校验全部」，逐条跑完报 `ok/tried`。`stateOf()` 优先读 `a.health`，没有才退回厂商级 |

**为什么这条必须补**：探针的失败作用域如果不跟请求的失败作用域一致（`k:` vs `p:`），UI 上读到的状态和路由的真实行为就会对不上。具体表现是——给一个健康厂商补第二把已吊销的 key，点一下「校验」，整家变红、流量绕开，而第一把其实一直好着。用户看到的是「这软件坏了」。

验证：`tests/probe_account_e2e.rs`（mock 上游只认 `Bearer good`；先校验好 key，再校验坏 key；断言坏 key `Error`、好 key 仍 `Healthy`、厂商级仍 `Healthy`）+ `cargo test -p freeport-core -p freeport-gateway` 全绿（core 45、gateway 32 单测 + 12 个 e2e）。

文档同步：`ARCHITECTURE.md` §3.5.1 补「校验的主体是凭据」、§3.8.2 由 Tier C 的 Pool 设想改写为「已落地的凭据模型 + 推迟的分池」。

## 15. 落地记录（第七批：迁移码 / 多机同步 v1）

商业化路线里「Pro 多机配置同步」一直悬着，因为它听起来必须有个服务端。其实换机最痛的那一步不需要服务器：**把十几个厂商的 key 再抄一遍**。所以这一批先做离线迁移码，把 bundle 格式定下来，将来 Pro 只是换传输层。

| # | 项 | 做法 |
|---|---|---|
| 31 | `transfer.rs` | `export(AppData, 口令) -> 迁移码`、`open(迁移码, 口令) -> Body`、`apply(Body, &mut AppData, mode) -> Summary` 三段分离。`open` 纯粹解密校验、不写任何文件，用户可以先看到「里面有什么」再决定导不导 |
| 32 | 加密 | Argon2id（m=19MiB, t=2, p=1）派生 32B → AES-256-GCM；信封自带 KDF 参数与 salt/nonce，AAD 就是这些参数本身 |
| 33 | 参数防炸 | 解密前校验 `m ≤ 256MiB / t ≤ 16 / p ≤ 8`——否则一个手写 JSON 就能让 App 申请 4GiB |
| 34 | 搬什么 | providers（**去掉模型目录与健康度**）、accounts（限额/启停）、secrets、api_keys（只搬 `key_hash`，原客户端继续可用）、routing、patrol |
| 35 | 不搬什么 | 用量台账、冷却、健康、目录、端口与本地鉴权 token、OAuth token（设备绑定） |
| 36 | 两种导入 | `merge` 只补缺失、不覆盖同名凭据的 key；`replace` 整体替换并**先清掉本机多余凭据的密文**，避免「以为覆盖了，旧 key 还在盘上」 |
| 37 | 不卡 runtime | Argon2 是 CPU-bound，`export/open` 都扔进 `spawn_blocking`——导出时网关可能正在转发对话 |
| 38 | 命令与界面 | `gw_export_bundle(passphrase)` / `gw_import_bundle(blob, passphrase, mode)`；设置页新增「迁移与备份」，口令两次确认、迁移码一键复制、覆盖模式必须勾选确认 |

**顺带修掉的一处旧文案**：设置页「协议入口」还写着「Anthropic / Gemini 在 M6」，实际五个入口早就全部在线。界面上留着一句和事实不符的话，比缺一个功能更伤信任。

验证：`tests/transfer_e2e.rs` 一条用例覆盖八件事——密文里搜不到明文 key 与凭据 id、口令错拒绝、ct 改一个字节拒绝、KDF 参数被放大拒绝、导入后限额/启停/哈希都在、重复导入幂等、覆盖模式清掉本地独有凭据的密文、口令过短直接拒绝。`cargo test -p freeport-core -p freeport-gateway` 全绿。

## 16. 落地记录（第八批：离线 Pro license）

商业化文档里 license 一直是「设计好了但没做」。这一批把它做成能签、能验、能激活的实物，并且守住一条红线：**网关里不存在任何 `if !is_pro()`**。

| # | 项 | 做法 |
|---|---|---|
| 39 | 新 crate `freeport-license` | 纯逻辑、零 I/O、零网络；只回答「现在是什么身份」，不参与准入 |
| 40 | 格式 | 标准 JWT，`alg=EdDSA`，签名覆盖 `header.payload`；`alg` 白名单校验（否则 `alg=none` 可自签） |
| 41 | 私钥隔离 | 签发能力在 `--features issuer` 后，App 依赖图里没有这个 feature；`keygen` / `issue` 两个 example 只在本地跑 |
| 42 | 状态机 | `missing / valid / grace / expired / invalid / wrong_machine`；过期后 7 天宽限期**仍算 Pro** |
| 43 | 两个 tier | `tier` 是生效档位（过期即 free），`licensedTier` 是买过的档位——UI 才能解释「买过但已过期」 |
| 44 | 不缓存状态 | 每次现算（~50µs）。缓存会让「把系统时间调回去」变成续期手段 |
| 45 | 绑定机器可选 | `mid` claim 对 `machine_id(install_id|host|os)`；不填就不绑（换主板/重装不废） |
| 46 | 激活的拒绝策略 | 签名不对 / 机器不符 / 已过宽限期 → 拒绝且**不落盘**，错误原文透给用户 |
| 47 | 界面 | 设置页「许可证」卡：激活框、到期日、剩余天数、能力标签、安装码复制、解除激活；侧边栏 Pro 卡在有 license 后变成「已激活 · 还剩 N 天」 |

**为什么坚持不做本地功能锁**：本地判权限拦不住破解（改个 flag 就过），却一定会误伤付费用户（时钟错、迁移没带 license、宽限期边界）。真正该拦的是**服务端数据**——实时目录、额度情报按 tier 返回，客户端只应用拿到的数据。这也是文档 §7.2「tier 只影响数据源，不影响功能开关」的落地。

**密钥现状**：本仓库内置的是开发用公钥，私钥在 `app/.dev/license-issuer.key`（已 gitignore）。正式发布前必须重新 `keygen`，把新公钥填进 `PROD_PUBKEY`，并轮换掉这把开发密钥。

验证：`freeport-license` 14 个单测（有效/宽限期/过期/改 payload/换密钥/`alg=none`/机器不符/时钟回拨/小幅偏差/free tier/垃圾输入/安装码稳定）+ `tests/license_e2e.rs` 一条用例九段（missing→垃圾拒绝→激活→重启仍在→过期拒绝→机器不符→绑本机→解除→宽限期仍 Pro）。

## 17. 落地记录（第九批：Pro 数据面 —— 身份随请求走）

上一批把 license 做成了「能验、能激活」，但它只回答「我是不是 Pro」，没有任何消费方。这一批接上数据面：feed 请求开始自报身份，服务端据此决定下发什么。

| # | 项 | 做法 |
|---|---|---|
| 48 | 身份声明 | `license::identity()` 现算出 `{tier, token}`：宽限期仍报 `pro`，过期立刻退回 `free`，不缓存 |
| 49 | 两个头 | `x-freeport-tier: free|pro` 说明声明；`Authorization: Bearer <jwt>` 证明声明。免费档**不带** Authorization |
| 50 | 只加身份不加判断 | 客户端没有「免费档不应用某快照」这类分支；收不收只看签名。服务端多给了数据是它的 bug，客户端不替它扣用户的数据 |
| 51 | 凭据只在可信链路发 | 仅 `https://` 或本机回环（`localhost` / `127.0.0.1` / `[::1]`）才带 Bearer；公网明文上带 JWT 等于广播凭据 |
| 52 | 快照跨源不带凭据 | 快照地址虽在签名里，但配置由用户填；与 manifest 不同源时不带 Bearer，避免本机 license 被送到第三方域名 |
| 53 | manifest 标注档位 | 新增可选 `tier`，**进签名载荷**（存在才进，老 manifest 照样能验）。不签的话中间人能把免费包标成 Pro 包，让界面说谎 |
| 54 | 状态只加展示 | `FeedState.servedTier` 记录「服务端说这份目录是哪个档」，Discover 的 feed 卡片显示「Pro 目录 / 公共目录」，不参与任何本地分支；换地址即清空 |

**为什么不让客户端判断档位**：一旦客户端写「不是 Pro 就别应用 Pro 包」，等于在本地复制一份权限逻辑——破解者改一行就绕过，而真正的付费用户会被时钟、迁移、宽限期边界误伤。把判断留在服务端，客户端只做两件确定的事：验签、如实上报身份。

**顺带说明**：`Features` 清单仍然只做界面展示。等服务端真的按 `feat` 裁剪字段时，它才需要被消费；提前在客户端按 `feat` 开关功能，就是回到功能锁的老路。

验证：`tests/feed_tier_e2e.rs` 一条用例四段——①免费档两次请求都只有 `tier: free`、无 Authorization；②激活 Pro 后 manifest 与快照都带 `Bearer <token>`，`servedTier == "pro"`；③快照放到另一端口（跨源）时仍报档位但不带凭据；④解除许可证后下一次检查立刻退回 `free`。`freeport-catalog` 另加三个单测：`tier` 参与签名、明文公网不放行 Bearer、同源判定。

## 18. 落地记录（第十批：迁移包文件 `.fpk`）

迁移码能用了，但它是「一段 1–20KB 的 JSON」——这个形态在真实换机场景里很脆弱：粘进微信可能被截断，邮件客户端会折行，用户也不知道该把它存哪。这一批把同一份信封落成文件。

| # | 项 | 做法 |
|---|---|---|
| 55 | 内容不变 | `.fpk` 里就是迁移码原文。格式没改一个字节，`open()` 照旧能解；改的只是「怎么交到另一台机器」 |
| 56 | 原子写 | 先写同目录 `.<name>.tmp`，`sync_all` 之后 rename。拔盘/断电时要么完整、要么没有——最坏不是「保存失败」，是留下一个看起来能导入、实际解不开的半截文件 |
| 57 | 权限 0600 | 内容是密文，但也不该让同机其他用户随手拷走 |
| 58 | 读取闸门 | 4 MiB 上限 + 空文件/非文本的专门报错。正常包几十 KB，超过就是选错文件，没必要整个读进内存 |
| 59 | 先验口令再弹框 | 否则用户挑完位置才被告知「口令太短」，白挑一次 |
| 60 | 对话框在 Rust 侧 | `tauri-plugin-dialog` 的 `blocking_save_file/blocking_pick_file`（async 命令线程，非主线程）。前端拿不到也不需要文件系统权限，路径不经过 IPC |
| 61 | 取消不是错误 | 命令返回 `Option<String>` / `Option<Summary>`，`None` 直接静默返回，不弹红字 |

**为什么开头就写死「先临时文件再 rename」**：这个文件一旦半截，用户看到的现象是「导入时提示迁移包损坏」，而不是「保存失败」——错误归因完全跑偏，会让人以为是加密坏了。磁盘写入的原子性不是一个性能细节，是错误信息的可信度。

验证：`tests/transfer_file_e2e.rs` 一条用例覆盖——导出后文件内容即迁移码原文且不含明文 key、权限 0600、不留 `.tmp`、目标机导入后 key 真的能取回、口令错报「口令」、覆盖写后旧口令失效、空/非文本/超大文件分别给人话错误、目录不存在与口令过短时不产生文件、`replace` 模式语义。`tsc --noEmit` 0 错，`cargo check -p freeport-app` 通过。
