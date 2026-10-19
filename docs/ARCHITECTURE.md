# Freeport 架构设计（v1.1 · 已定稿）

> Freeport（自由港）：本机常驻的「大模型额度聚合网关」桌面应用。自动发现可用厂商/模型（含订阅额度与 Cookie 会话）→ 统一 OpenAI / Anthropic / Gemini 协议 → 一键注入已安装的 Agent。

## 决策记录（2026-10-19）

| 决策项 | 结论 | 影响 |
|---|---|---|
| 产品名 | **Freeport（自由港）** | Bundle ID `dev.freeport.app`；CLI 命令 `freeport` |
| 技术栈 | **Electron 38 + TypeScript 全栈** | 可直接移植 temp 中全部 TS 协议/适配器代码；包体 ~150MB 为已知代价 |
| 合规边界 | **Tier A/B/C 全量支持（含 webCookie）** | 新增 Auth Broker 子系统；不上架应用商店；需法务文案与风控 |
| Tier C 首批 | **OpenCode Zen（主）+ GitHub Copilot（备）** | 两家都有官方 OAuth/设备码路径，不依赖浏览器 Cookie 库；Cookie 类只做 **OpenCode Zen 的 web 会话**作为探路 |
| 注入范围 | **12 个 Agent + 2 个引导式** | 注入器必须做成配置驱动的插件框架，而非硬编码 |
| 发行 | **发布给他人（macOS + Windows）** | 需签名/公证/自动更新/崩溃上报/**中英双语** |
| 局域网共享 | **不做** | 只绑 `127.0.0.1`；预留 `--bind` 开关但不开放 UI |
| 本地 Ollama | **不接** | 额度耗尽时如实报 429 + 最近恢复时间，不用本地模型兜底 |
| 商业化 | **开源核心 + Pro 订阅** | 免费版功能完整；Pro 卖「实时目录 + 额度情报 + 多机配置同步」，详见 §7.3 |

---

## 0. 结论先行

| 问题 | 结论 |
|---|---|
| 这个产品本质上是什么 | **本地反向代理 + 能力探测 + 配置注入器**，不是聊天客户端 |
| 最难的不是「聚合」 | 是 **工具调用（tool calling）能不能跑通**、**429/额度账本**、**流式跨协议转换** |
| 参考项目里最该抄谁 | 路由/配额/协议 → `freellmapi`；provider 广度 → `9router`；一键注入 → `router` + `freellmapi`；托盘与本地转发最小实现 → `RuiC-oc-ferry` |
| 最该避免的 | 「名字里带 free 就当免费」「无脑轮询所有 provider」「把逆向订阅当免费额度」 |
| 稳定性的本质 | 不是「支持多少家」，而是 **失败作用域判定 + 时间预算 + 配额账本** 三件事（§3.6） |
| MVP 边界 | 本机回环、单用户；Tier A 全量 + Tier C 两家（OpenCode Zen / Copilot）；注入先做 5 个核心 Agent |
| 商业边界 | 免费版功能完整；Pro 只卖「目录与情报的时效性」，不锁功能（§7.1） |
| 免费清单一定会腐烂 | 唯一解是「五源发现 + 候选池 + 每日 liveness 巡检 + 签名 feed 热更新」（§3.4.1 / §3.5.1 / §3.5.2），不是发版时硬编码 |
| 巡检最大的坑 | **探针自己会烧掉免费额度**。必须分级（liveness vs capability）、预算化、与真实流量竞争时让路 |
| chat2api 的定位 | 不是 provider，是「账号池 + 逆向图片」的 sidecar。v1 用自定义端点接，v1.5 走扩展宿主（§3.12） |

---

## 1. 参考项目严格分析

### 1.1 全景对比

| 项目 | 形态 | 技术栈 | 规模 | 真正解决的问题 | 对本项目价值 |
|---|---|---|---|---|---|
| **9router** | Web 网关 + CLI + 托盘 | Next.js 16 / React 19 / Node, SQLite | 1396 文件, 138 provider registry | 40+ 厂商、100+ 模型统一到 `:20128/v1`，Claude↔OpenAI↔Gemini 转换，订阅额度优先 + 自动回退 | ★★★★★ provider 声明式注册表、协议转换层、9 个 agent 注入器 |
| **freellmapi** | Web + Electron 菜单栏 + CLI + 移动端 | TS monorepo: Fastify + React + better-sqlite3 | 876 文件, 34 厂商/635 endpoint | 免费额度聚合、bandit 路由、四窗口配额、签名目录热更新、20 个 agent 注入 | ★★★★★ 路由/配额/失败分类/目录同步的工程范本 |
| **router (Weave)** | Go 网关 + Next 前端 + npx 安装器 | Go 1.25 / Gin / Postgres / ONNX embedder | 1681 文件 | 按「动作」语义路由（非轮询），三协议原生，`--claude/--codex/--opencode/--pi` 一键接管 | ★★★★☆ 注入器工程化（managed block / jq 结构化合并 / uninstall）、per-action 路由思想 |
| **FreeLLM-Gateway** | Fastify + React 19 + Electron + Docker | TS, SQLite, AES-256-GCM | 151 文件 | 多端口协议（8788/8789/8790）、目标驱动 set-cover 选池、密钥保险库 | ★★★☆☆ 多端口隔离、加密与日志脱敏思路；「set-cover 求解」偏营销，实际价值待验证 |
| **freellm** | 纯网关 + Dashboard | TS monorepo: Hono? + Vite React | 112 文件 | 8 家**正规**免费厂商、3 个元模型（free-fast/smart）、多 key 轮转、严格模式、隐私路由 | ★★★★☆ 最干净的 MVP 骨架；「元模型」命名值得抄 |
| **RuiC-oc-ferry** | Electron 托盘（无窗口框架） | 纯 JS + Electron 44, 85 tests | 33 文件 | 把 OpenCode 免费模型**摆渡**给 WorkBuddy：隔离运行时、并发探测、判定缓存、写 models.json、本机转发口 | ★★★★★ 「一键注入单个 agent + 生命周期清理」的最小完整实现；诚实交代的边界说明 |
| **llm4free** | Python 库 + OpenAI 兼容 server | Python 3.10+ | 278 文件 | 40+ 模型统一 SDK，含 webai/逆向能力 | ★★☆☆☆ 大量逆向/网页会话，合规风险高；仅参考「统一 client 抽象」 |
| **awesome-free-llm-apis** | 静态目录 | JSON + Node 校验脚本 | 16 厂商/123 模型 | 免费厂商结构化数据 + 每日可用性校验 | ★★★★☆ **目录数据格式**可直接借鉴；校验脚本思路可复用 |
| **Free-LLM / awesome-freellm-apis** | 静态目录 | Markdown | 47 厂商/210 模型 | 社区维护的免费 API 索引，含 credit card 透明度标注 | ★★★☆☆ 冷启动数据源；需人工核验时效 |
| **chatgpt2api** | Python 服务 + Web 面板 + Docker | Python / FastAPI + React | 1 个上游（ChatGPT 官网），图片为主 | OpenAI 账号池 + OAuth token 生命周期 + 官网图片链路逆向 + 代理链 + 任务队列 | ★★★☆☆ **不接主链路**：只吸收「凭据保活/刷新退避/状态机/代理优先级」四组实战参数；作为 v1.5 扩展宿主（Extension）的样板。逆向图片链路风险过高，不进 v1 |

### 1.2 9router 的 provider 分类（实测统计）

```
apikey   85   ← 官方 API key（含免费 tier）
oauth    25   ← 借用已登录客户端（Kiro / Cursor / Copilot / Codex ...）
freeTier 20   ← 明确免费额度
free      5   ← 无鉴权
webCookie 2   ← 网页 cookie 逆向
```

**关键判断**：85 个 apikey 里真正「零成本、稳定、合规」的不到 30 个。其余是转售 relay、试用额度、或需要绑卡。**产品必须显式分层，否则用户会遇到「列表 100 个模型，能用的 8 个」。**

### 1.3 freellmapi 的路由工程（值得整体借鉴）

```
base      = w_rel·reliability + w_speed·speed + w_intel·intelligence
effective = base × headroomFactor × rateLimitFactor
```

- 可靠性：Beta 后验 + 7 天窗口 2 天半衰期，Thompson 采样（探索自动）
- 速度：吞吐 `1-exp(-tok/s/60)` × 0.6 + TTFB 线性 × 0.4
- 守卫：月度预算余量、RPM/RPD/TPM/TPD 实时窗口余量、429 惩罚衰减
- 失败分类决定「跳 key / 跳模型 / 跳平台 / 冷却多久 / 是否学习限额」
- 粘性会话（首条 user message SHA-1）防止中途换模型导致幻觉
- 切模型时注入 context handoff system message
- `X-Fallback-Trail` 暴露每次决策

**这几个机制直接决定了「用户感觉稳不稳」，优先级高于支持多少厂商。**

### 1.4 三个必须避开的坑（参考项目里踩过的）

| 坑 | 证据 | 我们的对策 |
|---|---|---|
| 用名字判断免费 | `RuiC-oc-ferry` 明确写「名字里带 free 不算数，只看价格字段是否为 0」 | 免费判定 = 上游价格字段全 0 **或** 目录显式标注 + 人工核验 |
| 没有工具能力的模型进 agent | `OC Ferry` 必须做「5 工具体检」，只回文本的降级为「仅对话」 | 探针分级：连通 / 流式 / tools / json / vision，能力标签进入路由门禁 |
| 单端口混多协议导致路径冲突 | `FreeLLM-Gateway` 用 8787/8788/8789/8790 四端口隔离 | 单端口路径前缀隔离（`/v1`、`/v1/messages`、`/v1beta`），避免多端口防火墙问题；端口仍可配 |

---

## 2. 产品定位与范围

### 2.1 一句话

> 装上就有一个 `http://127.0.0.1:PORT/v1`，背后是你所有免费额度；任何 Agent 改一行配置就能用，且能一键改回来。

### 2.2 三层价值（按用户感知排序）

1. **不用管 key 和额度**：一处录入，全局可用；429 自动换，不中断
2. **不用手改配置**：检测到已安装的 Agent，勾选即注入，带 diff 预览与回滚
3. **知道什么能用**：模型列表带「工具调用/视觉/流式/JSON/上下文」能力徽章与实测延迟

### 2.3 明确不做（v1）

- 不做聊天 UI（可选做「连通性测试」小面板，不做会话产品）
- 不做公网托管 / 多租户 / 按 token 计费
- 不做模型权重下载与本地推理；**v1 不接 Ollama**（额度耗尽就如实报 429，不用本地模型掩盖问题）
- 不做局域网共享（只绑 `127.0.0.1`）
- 不承诺 SLA：免费额度由上游决定，产品只保证「优雅降级 + 如实告知」

---

## 3. 系统架构

### 3.1 进程模型

```
┌──────────────────────────────────────────────────────────────┐
│  Desktop Shell（托盘 + 主窗口 + 自启 + 更新）                   │
│  ┌────────────────┐   ┌──────────────────────────────────┐   │
│  │  UI (React)    │←→ │  Control Plane (RPC / IPC)        │   │
│  └────────────────┘   │  · 目录同步  · 凭据保险库          │   │
│                       │  · 探针引擎  · Agent 注入器        │   │
│                       │  · 配置备份/回滚                   │   │
│                       └──────────────┬───────────────────┘   │
│                                      │ 同进程 / 共享 DB       │
│  ┌───────────────────────────────────▼───────────────────┐   │
│  │  Data Plane：HTTP Server  127.0.0.1:PORT               │   │
│  │  Ingress → Auth → Resolve → Gate → Route → Adapter     │   │
│  │           → Translate(SSE) → Account → Log             │   │
│  └───────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
                              │ HTTPS
                    ┌─────────┴──────────┐
                    ▼                    ▼
              官方免费 API          扩展 sidecar（可选）
                              （图片 / 音频，独立进程）
```

**关键决策：Data Plane 与 UI 同进程、共享同一个 DB 连接。**

理由：单用户本机场景，无需跨进程；避免 `freellmapi` 遇到的「bundle 后 DB 单例出现两份」问题；省掉 IPC 序列化与端口二次分配。托盘退出时一起退出。

### 3.2 分层职责

| 层 | 职责 | 不做什么 |
|---|---|---|
| Shell | 生命周期、托盘、自启、单实例锁、更新、崩溃重启 | 不写业务逻辑 |
| UI | 展示、配置编辑、diff 预览、日志检索 | 不直接发上游请求 |
| Control Plane | 目录、凭据、探针、注入、备份 | 不处理业务流量 |
| Data Plane | 鉴权、模型解析、门禁、路由、协议转换、流式、记账 | 不弹 UI、不写用户配置 |
| Adapter | 单一 provider 的 wire 细节与 quirk | 不做路由决策 |
| Translator | 三协议 ⇄ Canonical IR | 不感知 provider |

### 3.3 核心抽象：Canonical IR（这是整个架构的支点）

不做 N×M 的协议互转，做 `N + M`：

```
Client wire ──parse──▶ CanonicalRequest ──build──▶ Provider wire
Client wire ◀─render─ CanonicalStream  ◀─parse── Provider stream
```

```ts
type CanonicalRequest = {
  model: string
  system?: string
  messages: Array<{ role: 'user'|'assistant'|'tool'; content: Part[]; toolCallId?: string }>
  tools?: ToolDef[]
  toolChoice?: 'auto'|'none'|'required'|{ name: string }
  temperature?, topP?, maxTokens?, stop?, seed?
  responseFormat?: { type: 'text'|'json_object'|'json_schema'; schema?: unknown }
  reasoning?: { effort?: 'low'|'medium'|'high'; budgetTokens?: number }
  stream: boolean
  metadata: { sessionId?, clientId?, requestedModel? }
}

type CanonicalStreamEvent =
  | { t: 'start'; model: string }
  | { t: 'text'; delta: string }
  | { t: 'reasoning'; delta: string }
  | { t: 'tool_call'; index: number; id?: string; name?: string; argsDelta?: string }
  | { t: 'usage'; input: number; output: number; cached?: number }
  | { t: 'error'; code: string; message: string; retryable: boolean }
  | { t: 'done'; finishReason: 'stop'|'length'|'tool_calls'|'error' }
```

**为什么值得单独设计**：
- 新增一个入口协议 = 1 个 parser + 1 个 renderer
- 新增一个厂商 = 1 个 adapter
- 探针、日志、缓存、压缩、脱敏全部只面向 IR，写一次

### 3.4 目录与发现（Discover）

三层数据，优先级从高到低：

| 层 | 来源 | 更新 | 作用 |
|---|---|---|---|
| L1 内置基线 | 随版本打包的 `catalog.json` | 发版 | 离线可用、冷启动不空 |
| L2 远程 feed | 自建静态 JSON + Ed25519 签名 | 每日/手动 | 厂商上下线、限额变更、quirk 公告 |
| L3 运行时探测 | 上游 `/models` + 探针请求 | 6h / 手动 | 修正实际可用模型与能力 |

**L2 签名校验**（借鉴 freellmapi）：固定公钥，校验失败直接丢弃并告警，防止 CDN/MITM 注入模型。

**免费判定规则（硬规则，不可放宽）**

```
isFree(model) =
    catalog.freeTier === true
  AND (
        all(price.input, price.output, price.cachedRead, price.cachedWrite) == 0
     OR upstream 明确返回 "free"/"0" 的价格对象
  )
  AND catalog.deprecated !== true
  AND probe.status != 'unavailable'
```

- 名称含 `free` **不作为依据**
- 需要绑卡才能用的额度标记 `requiresCard`，默认不推荐
- 试用额度（$5 credit）标记 `trial`，单独分组

**厂商合规分层（产品级开关）**

| Tier | 含义 | 示例 | 默认 |
|---|---|---|---|
| **A 官方免费额度** | 注册即得、可长期使用、有公开限额 | Google AI Studio, Groq, Cerebras, Mistral, NVIDIA NIM, Cloudflare Workers AI, GitHub Models, OpenRouter `:free`, SiliconFlow, ModelScope, Zhipu, Cohere trial, OVHcloud | ✅ 开启 |
| **B 匿名/公共端点** | 无需 key 或公共 relay，稳定性差 | Pollinations, LLM7, AI Horde | ⚠️ 可选，标注「不保证」 |
| **C1 客户端订阅（OAuth）** | 账号已付费的订阅额度，通过官方 OAuth 授权 | Kiro, Copilot, Codex/ChatGPT 订阅, Claude Pro/Max, Gemini 订阅 | ⚠️ 可选，独立分组 |
| **C2 网页会话（Cookie）** | 通过内置登录窗口取得站点会话 | 各类 Web 版模型、Zen 类免费池 | ⚠️ 可选，独立分组 |

**Tier C 的产品与法律边界（写进 UI 与 EULA，不可省略）**

| 允许 | 禁止 |
|---|---|
| 用户用自己的账号、在自己的机器上、通过官方登录页授权 | 代替用户注册账号、批量养号、共享账号 |
| 内置浏览器窗口登录，用户可见、可随时撤销 | 静默解密系统浏览器 Cookie 数据库 |
| 复用官方客户端使用的公开端点与请求头 | 伪造设备指纹绕过风控、破解付费墙、抓取付费内容 |
| 明确标注「额度由上游决定，可能随时失效」 | 承诺可用性、代充、转售额度 |

> 结论：**Cookie 一律用内置 `BrowserWindow`（独立 partition）登录获取，不读系统 Chrome/Safari 的 Cookie 库。** 这样不需要「完全磁盘访问权限」，用户可见可控，法律定性也更干净。这是本项目与 9router/llm4free 的重要区别。

**发布形态的连带约束**：含 Tier C 的产品**无法上架 Mac App Store / Microsoft Store**，只能官网 + GitHub Releases 分发，并需自建 EULA 与下架响应流程。

### 3.4.1 发现源、候选池与「未收录」的自动收录

目录不是一张人工维护的清单，而是**五条流水线 + 一个候选池**。任何一条断掉，产品会在两周内腐烂——免费额度的变化速度远快于发版速度。

| 源 | 拉什么 | 频率 | 可信度 | 用途 |
|---|---|---|---|---|
| S1 上游 `/models` | 已连 provider 的真实模型列表 | 6h | 最高 | **下架检测**（优先级最高，能立刻发现模型消失） |
| S2 OpenRouter 公共目录 | `GET /api/v1/models`，过滤 `pricing.prompt == "0"` | 6h | 高 | **免费模型雷达**：覆盖面最广的单一数据源 |
| S3 价格/上下文库 | models.dev、LiteLLM `model_prices_and_context_window.json` | 24h | 中高 | 补齐 context、价格、模态、工具能力字段 |
| S4 厂商 changelog / status RSS | Groq、Cerebras、Mistral、Cloudflare、Gemini release notes | 6h | 高 | 「新模型发布即到」+ 故障公告 |
| S5 社区清单 | awesome-free-llm-apis、freellmapi 目录、9router registry | 24h | **低** | 只作**线索**，必须实测通过才收录 |

S5 只当线索，这是硬规则。9router 的 138 个 provider 里有大量转售 relay 与已死端点；直接照抄的结果是用户首次启动就看到一排 404，信任一次性破产。

**候选池（Candidate Pool）**

```
discovered → probing → accepted
                    ↘ rejected(reason) / needs_key
```

- 新 provider 进 `discovered_providers`，**不进主目录、不出现在 Models 页**
- 能匿名验证的（Tier B）直接跑 liveness，通过即 `accepted`
- 需要凭据的：在「发现」页展示卡片（厂商 / 官网 / 免费额度说明 / 需要什么），用户点「添加」才进正式流程
- 被拒的保留 `rejected_reason` + 7 天，避免反复探同一个死端点
- **同一模型多源供应**（如 `deepseek-v3.2` 在 OpenRouter / SiliconFlow / 官方都有）归一到 `model_family`，Models 页折叠成一行（显示「3 个来源」），路由时在族内按质量排序

**去重与归一**

| 问题 | 处理 |
|---|---|
| provider 别名（`groq` / `groq-cloud` / `api.groq.com`） | alias 表，入库前归一 |
| model key 噪声（`:free`、`-2024-11-20`、`-fp8`、`@latest`） | 拆成 `base` + `variant`，路由 key 用原始 id，展示名可覆盖 |
| 同一模型不同上下文长度 | 取该 provider 实测值，不取厂商宣传值 |
| 上游改了模型 id | 用 `/models` 的 diff 生成 `rename` 事件，保留旧 id 作为 alias 30 天 |

### 3.5 探针引擎（Capability Probe）

分四级，逐级递增成本，结果进缓存：

| 级别 | 请求 | 判定 | TTL |
|---|---|---|---|
| P0 连通 | `GET /models` 或 1 token chat | 鉴权/网络/模型存在 | 6h |
| P1 流式 | 同一请求 `stream:true` | 是否真的 SSE 分块（不是一次性 JSON） | 6h |
| P2 工具 | 给 1 个 `echo` 工具 + 强制 `tool_choice` | 是否产生结构化 tool_call | 6h |
| P3 结构化/视觉 | `response_format=json_schema` / 1×1 png | 是否遵守 schema / 是否接受图 | 24h |

规则：
- 并发 3（可配 1–6），单模型超时 45s，总预算可中断
- **只回文本不算失败**：降级为 `chat-only`，tools 能力关闭，路由时过滤
- 失败分类落库：`auth / rate_limit / timeout / format / unsupported / upstream_error`
- 判定按「模型 + key 指纹 + 客户端版本」缓存，key 变更即失效
- 重启复用缓存，秒开（借鉴 OC Ferry：成功 6h，负载状态 10min）
- TTL 只是**上限**；实际巡检周期按冷热分层（热 6h / 温 24h / 冷 7d），见 §3.5.1

### 3.5.1 巡检调度（Health Loop）

**必须先承认一个事实：探针本身消耗免费额度。**

天真的实现是「每天把全部模型 P0–P3 探一遍」：300 模型 × 4 级 ≈ 1,200 次请求。免费额度普遍是 1,000 RPD 量级——**巡检自己就把用户的额度吃光了**，然后用户看到的是「这网关一天就 429」。这是大多数同类产品的隐性死因。

所以巡检第一原则：**探测成本要计入预算，与真实流量竞争时永远让路。**

**两级探针，职责分离**

| 类型 | 请求内容 | 成本 | 频率 |
|---|---|---|---|
| `liveness` 存活 | `GET /models` + `chat(max_tokens=1, prompt="hi")` | 3–10 token | 高频（分钟–小时） |
| `capability` 能力 | P1 流式 / P2 tools / P3 json·vision | 50–500 token | 低频（入库、版本变更、手动） |

结论：**每日体检只跑 liveness，能力探测按需且有 TTL。** 这样既做到「每天检查一次」，又不会把额度烧在探测上。

**频率矩阵（默认值，全部可配）**

| 对象 | 周期 | 触发条件 | 备注 |
|---|---|---|---|
| 目录 feed | Free 6h / Pro 5min | 定时 + 手动 | 条件请求，304 成本≈0 |
| 凭据体检 | 6h | `expires_at` 检查 | 提前 24h 刷新（见 3.8.1） |
| 账号真实请求 | 24h | 每日审计 | 只有真实调用能验出「额度是否还在」 |
| 热模型 liveness | 6h | 7 天内被路由过 | 覆盖约 90% 实际流量 |
| 温模型 liveness | 24h | 7–14 天未使用 | — |
| 冷模型 liveness | 7d | >14 天未使用 | 省预算 |
| 候选模型 | 24h | 仍在候选池 | 直到 accepted / rejected |
| 冷却复检 | 剩余 ≤60s 时 | 自动 | 提前解除，不等满 |
| 能力探测 | TTL 7d | 入库 / 客户端大版本 / 手动 | 结果复用，key 变更即失效 |

**每日全量审计**：本地时间 03:00 ±90min 抖动（避免所有用户同时打上游），只跑 liveness，超预算即停。休眠或断网恢复后补跑一次，不重复跑。

**预算模型**

```
dailyProbeBudget = clamp(Σ(账号日配额估算) × 1%, 60, 800)
perAccountRate   = 2 次/分钟     // 与真实流量共享 RPM，但真实流量优先
```

- 探针带 `X-Freeport-Probe: 1`，日志与用量分开统计，**不计入用户的「今日请求」**
- 预算耗尽 → 降级为「只探当前路由链前 20 个模型」，保命优先
- 用户可关自动巡检，但 UI 明确写「关闭后无法提前发现失效模型」

**健康状态机（防抖是重点）**

```
unknown → healthy → degraded → cooling → unavailable → retired
```

| 状态 | 判定 | 路由行为 |
|---|---|---|
| `healthy` | liveness 通过 + 最近 20 次真实请求错误率 < 5% | 正常参与 |
| `degraded` | liveness 通过但 P1/P2 失败 | 只进 chat-only 路由 |
| `cooling` | 429 / 5xx 触发冷却 | 冷却期内排除 |
| `unavailable` | **连续 2 次** liveness 失败 | 排除，30 分钟后复检 |
| `retired` | 连续 7 天 unavailable，或目录标记下线 | 移出路由，保留历史 |

防抖参数直接采用 chatgpt2api 跑过真实规模的值：新账号 **10 分钟**宽限不判死、无效判定需 **30 秒**内二次确认、刷新失败退避 **5 分钟**。**单次失败绝不改状态**——免费厂商偶发 502 是常态，一次就判死会让整个池子每天抖动。

**每日摘要（Digest）**

不是报告，是「一行结论 + 三条变更」：

```
今日：新增 3 个模型 · 1 个下线 · 2 个恢复；探针 137/300 次预算，无异常。
```

### 3.5.2 目录运营：让「发布即到」真正成立

把轮询调到 1 分钟并不等于「发布即到」，只会让客户端更频繁地拿到**未经验证**的数据。真实链路是五段：

```
上游发布 → CI 验证 → 签名发布 → 客户端差量应用 → 灰度进路由
```

**feed 结构**

```
catalog/
  latest.json                     # {version, channel, seq, publishedAt, sha256, sig, url}
  snapshots/2026.10.19.1.json     # 全量快照（gzip）
  patches/2026.10.19.1.json       # RFC 6902 patch，相对上一版
```

- 签名对象 = **JCS 规范化的 JSON + sha256**；Ed25519 私钥只在 CI，公钥内置客户端
- 客户端带 `If-None-Match` 与 `?since=<seq>`，无变更返回 304 或空 patch
- 每 24h 拉一次全量并核对 `sha256`，防止 patch 链漂移
- 保留最近 5 个快照，异常时可回滚到 `lastKnownGood`

**变更事件（UI 渲染事件，不渲染版本号）**

```ts
type CatalogEvent =
  | { kind:'provider.added';    providerId; tier; requiresKey; homepage }
  | { kind:'model.added';       modelId; freeEvidence; limits; caps }
  | { kind:'model.removed';     modelId; reason:'upstream'|'paid'|'dead' }
  | { kind:'quota.changed';     modelId; from; to }
  | { kind:'capability.changed';modelId; caps }
  | { kind:'incident';          providerId; severity:'degraded'|'down'; until }
  | { kind:'quirk.changed';     providerId; note }
```

每个事件带 `eventId` + `seq`，**幂等投递**：重复收到不产生重复通知，乱序到达按 `seq` 丢弃旧事件。

**应用策略（不能自动全量应用）**

| 策略 | 行为 | 适用 |
|---|---|---|
| `auto`（默认） | 新模型以 `candidate` 入目录 → 探针通过 → **0.3 倍权重**进路由 → 连续 20 次成功且错误率不高于同类均值 → 转 `active` | 多数用户 |
| `notify` | 只提示，用户点「应用」才生效 | 保守用户 / 团队 |
| `pinned` | 锁定当前目录，不接收 feed | 离线 / 追求稳定 |

**三个安全阀（缺一个就可能一次 feed 打穿所有用户）**

1. 单次变更删除 > **15%** 模型，或新增 > **30%** → 暂停自动应用，转人工确认
2. 移除类事件立即生效（向下安全），但写 `tombstones`，日志可追溯
3. 签名校验失败、`seq` 小于本地、`sha256` 不匹配 → **丢弃并告警，绝不降级应用**

**发布流水线（这是 Pro 的真实护城河，不是定价页上的话术）**

```
06:00  抓 S1–S5 → 生成候选 diff
06:05  用维护者 key 池对新增/变更项跑真实探针（liveness + tools）
06:10  通过 → 自动签名发 beta；低置信 → 进人工队列
次日    beta 跑 24h 无异常 → promote 到 stable
```

GitHub Actions 足够，成本≈0。`beta` 频道的意义是让新 provider 先见一小批用户，避免全量翻车。

**运营侧必须有 Admin**：变更 diff 审阅、事件撤回、provider 强制下架（incident）、按版本回滚。没有这个，「发布即到」就是「事故即到」。

### 3.6 路由、权重与失败接力（核心子系统）

这是整个产品**唯一不能糊弄**的部分。用户感知的「稳」，90% 来自这里：同样的 20 家免费厂商，路由做得好就是「几乎不断」，做得差就是「一天崩八次」。

#### 3.6.1 请求流水线

```
Ingress ──▶ ① Resolve  ──▶ ② Gate ──▶ ③ Rank ──▶ ④ Pick Key ──▶ ⑤ Dispatch
                                                                      │
                          ┌───────────────────────────────────────────┘
                          ▼
                    ⑥ Stream（首字节即返回，边转发边翻译）
                          │
              ┌───────────┴────────────┐
              ▼                        ▼
        ⑦ Settle 成功             ⑧ Classify 失败
        · 释放 lease              · 判定 scope（key/model/platform）
        · 记账 rpm/rpd/tpm/tpd    · 写冷却 + 惩罚 + 学限额
        · 更新 Beta 后验          · 回到 ③（下一跳）
        · 清除失败窗口
```

关键约束：**第 ③ 步每次重试都重新执行**，不是「开局排一次序，然后线性走完」。因为一跳失败会改变候选集（skip 集合、冷却、余量），后面的顺序必须重算。

#### 3.6.2 候选集构建（Chain）

候选来自三层，按优先级合并去重：

| 层 | 来源 | 用途 |
|---|---|---|
| L1 | 请求显式指定 `provider/model` | 用户点了名，pin 到队首（但仍受硬门禁约束） |
| L2 | 激活的 Profile（`coding` / `long-context` / `cheap`） | 不同场景用不同链 |
| L3 | 全局链（`auto` 别名解析） | 兜底 |

合并后依次做**四类剔除**（剔除不是判死，只是本轮不考虑）：

1. **能力门禁**：`tools` / `vision` / `response_format` / 上下文窗口 / `tpm` 单请求上限
2. **凭据**：平台没有任何 `enabled + healthy|unknown` 的 key → 整平台移除（避免空转）
3. **本请求已排除**：`skipKeys` / `skipModels` / `skipPlatforms`
4. **冷却中**：`isOnCooldown(platform, model, key)` 未过期

> 剔除顺序有讲究：先能力、再凭据、最后请求级 skip。这样「所有模型都因为没 key 被剔除」能报 503 `no_providers_configured`，而不是误导成 429 限流。

#### 3.6.3 打分：三轴凸组合 × 三个守卫乘子

**第一层：质量轴（权重和恒为 1，结果 ∈ [0,1]）**

```
base = wR · R  +  wS · S  +  wI · I
```

| 轴 | 计算 | 冷启动先验 |
|---|---|---|
| **R 可靠性** | Beta 后验 `α/(α+β)`；路由时用 Thompson 采样 `sampleBeta(α,β)` | Beta(1,1) 均匀先验；样本按 7 天窗口、2 天半衰期衰减 |
| **S 速度** | `0.6 · (1 − e^(−tok/s / 60)) + 0.4 · ttfbScore` | 0.6（乐观，让未测模型有机会被选中） |
| **I 智能** | 先 `tier×1000 − √rank×31` 求复合值，再在**当前链内** min-max 归一化 | 未知 tier 记 0（地板），不排除 |

`ttfbScore`：≤300ms 得 1，≥5s 得 0，中间线性。**超时也计入速度轴**：按实际墙钟时长、零输出 token 计，上限 120s——否则「每次都挂 30 秒」的模型会保持漂亮的速度分。

智能轴的 tier 严格分层（Frontier 4 / Large 3 / Medium 2 / Small 1），`tier×1000` 保证高档位的差模型也压过低档位的好模型；`√rank×31` 只在这个档位内起作用，同时让用户改 rank 时能肉眼看到排序变化。

**第二层：守卫乘子（只降不升，∈ [floor, 1]）**

```
score = base × headroomFactor × rateLimitFactor × userOverride
```

| 乘子 | 触发 | 曲线 |
|---|---|---|
| `headroomFactor` | 剩余额度 < 20% | 线性从 1 降到 0.1；`min(月度余量, 窗口余量)` 取**更差的那个**，不相乘（避免 floor² 把模型打到 0.01） |
| `rateLimitFactor` | 近期 429 惩罚值 p ∈ [0,10] | `1 − (p/10)×0.6`，最低保留 40% 分数——**降权但不判死**，惩罚衰减后能自己爬回来 |
| `userOverride` | 用户手动设的 0–2 倍率 | 0 = 永久降到最后（仍可被显式 pin 调用） |

**为什么余量做乘子而不是轴**：余量是「危险度」不是「质量」。做成轴会稀释质量差异（一个又快又稳的模型因为用了 80% 额度就排到慢模型后面）；做成乘子则保持「好模型优先，快烧完了才让位」的语义。

#### 3.6.4 权重从哪来：预设 → 动态偏置 → 手动覆盖

**预设（用户可选，也可自定义滑杆）**

| 预设 | R | S | I | 适用 |
|---|---|---|---|---|
| `balanced`（默认） | 0.50 | 0.25 | 0.25 | 通用 |
| `coding` | 0.40 | 0.20 | 0.40 | 带 tools 的请求（自动判别） |
| `fastest` | 0.35 | 0.55 | 0.10 | 交互式补全、批量小任务 |
| `smartest` | 0.35 | 0.10 | 0.55 | 复杂重构、长推理 |
| `reliable` | 0.70 | 0.15 | 0.15 | 「我只要它能跑通」 |
| `cheap` | 0.55 | 0.25 | 0.20 | 额度优先（headroom 阈值提前到 40%） |
| `custom` | 用户滑杆 | | | 三个权重实时归一化 |

**动态偏置（只对 `balanced` / `coding` / `smartest` 生效）**

1. **任务判别**：请求含 `tools` → `coding`；`tools` 且上下文 > 32k → `coding` + 上下文权重；无 tools 且 `max_tokens < 512` → 速度偏置。
2. **时段偏置**（默认关闭）：晚 18:00–次日 06:00 免费 relay 拥塞，把 `speed` 权重的 60% 移给 `reliability`。
3. **豁免规则**：`fastest` / `reliable` / `custom` 不参与任何自动调权——用户选这两端就是已经做了决定，再自动改会把预设变成「另一个预设的噪声副本」。

**探索（防饥饿）**

```
每次请求，若 未降级 且 非 pin 且 random() < 0.08：
  从「样本数 < 5 且通过本请求全部硬门禁」的模型里均匀抽 1 个，置顶试一次
  失败则正常落回打分顺序
```

没有这一步，138 个 provider 里排名靠后的永远拿不到样本，永远排最后，形成死循环。探索失败的代价是**一次尝试**，收益是整个池子的信息量。

#### 3.6.5 从分数到执行顺序

排序键是三段式，**前一段严格优先于后一段**：

```
sort key = (match_tier ASC, score DESC, priority ASC)
```

- `match_tier`：slug 解析命中的模型记 1，直连 `model_id` 记 0。保证「用户点名 `glm-4.7`」不会输给「别名匹配到的 `glm-4.5`」，无论后者实时数据多好。
- `priority`：用户链里的手工顺序，只做**平局打破**。这样既保留手动控制感，又不让「手工置顶一个 0 成功率模型」毁掉整体可用性。
- **粘性会话 pin**：同一会话（首条 user message 的 SHA-1）30 分钟内把上次成功的模型移到队首；命中但该模型已冷却则放弃 pin。

#### 3.6.6 Key 选择（模型定了，用哪个 key）

| 策略 | 规则 | 适用 |
|---|---|---|
| `auto`（默认） | 该模型下按 key 级 R 采样 + 轮转 | 通用 |
| `least-remaining` | 跳过共享池，按**剩余额度最多**的可用 key 排序 | 多账号、平台有账号级总配额 |

先过滤：`enabled` 且 `status ∈ {healthy, unknown}` 且不在冷却 且 未达并发上限。
`unknown` 视为可用——没探测过不等于坏，让真实请求去证伪。

#### 3.6.7 失败接力：判定作用域，而不是「重试三次」

失败处理的第一件事不是退避，而是**判断这次失败属于谁**。判错作用域是绝大多数聚合网关崩掉的根因：一个平台 5xx，却在它的 4 个 key × 6 个模型上各烧一跳，40 秒过去用户还在等。

**错误 → 作用域 → 动作（完整矩阵）**

| 上游信号 | 归类 | 跳过范围 | 冷却 | 惩罚 | 学限额 |
|---|---|---|---|---|---|
| 401 / invalid_api_key | auth | **该 key** | 5 min | — | 否 |
| 402 / insufficient_quota | credit | **该平台全部模型**（该 key） | 24 h | — | 否 |
| 403 model forbidden | tier | **该模型**（全 key） | 24 h | — | 否 |
| 404 model not found / 410 EOL | retired | **该模型** | 24 h + 标记下线 | — | 否 |
| 429 日额度耗尽（body 明示） | daily_exhausted | **模型 + key** | 到 UTC 零点 / `Retry-After` | 重（3） | 是 |
| 429 分钟级 rpm/tpm | rate_limited | **该 key** | 90 s（不计阶梯） | 轻（1） | 是 |
| 5xx / 超时 / 连接重置 | upstream | **整个平台**（本请求） | 90 s → 阶梯 | 轻（1） | 否 |
| 400 context too large | too_large | **该模型** | — | — | 是（收紧上下文） |
| 400 不支持 tools | no_tools | **该模型（仅 tool 请求）** | 软 bench 6 h | — | 否 |
| 400 `response_format` 被忽略 | format_ignored | **该模型（仅结构化请求）** | — | — | 否 |
| 400 tool arguments 非法 | bad_tool_args | 该 key | — | — | 否 |
| 空响应 / 只有 reasoning | empty | 该 key | 豁免（连续 ≤3 次） | — | 否 |
| 整段回答就是 `safe` / `unsafe` | relay_filter | 该平台（本请求） | — | — | 否 |
| 流在中途断掉 | truncated | **该平台（本请求）** | 阶梯 | 轻 | 否 |
| 客户端主动断开 | client_gone | **终止整条链** | — | — | — |

三条不写在表里但必须实现的规则：

1. **平台级失败只在本请求内跳过平台**，不写全局 bench（除非触发熔断）。否则一次网络抖动会让整个 provider 被误杀 90 秒。
2. **`HedgeAbortError` / 客户端断开不算 provider 失败**：不冷却、不惩罚、不学限额。把它记成失败会让健康的模型被冤枉。
3. **模型级 bench**：15 分钟内跨 key 累计 3 次可重试失败 → 该模型在**所有 key** 上 bench 10 分钟。一次成功即清空窗口。

**冷却阶梯（按 `model+key` 计，滚动 24h）**

| 第 N 次 | 时长 |
|---|---|
| 1 | 2 min |
| 2 | 10 min |
| 3 | 1 h |
| 4+ | 24 h |

- 上游给了 `Retry-After` 且大于我们的估计 → 用上游的，并标 `authoritative`（不再探测）。
- 未声明限额的平台（只有「免费」没有任何 rpm/rpd 数字）：1 小时内 2 次 429 判为「事实上的日耗尽」，但冷却**封顶 10 分钟**，避免 90 秒循环打一整天。
- 冷却来源分四类：`heuristic`（可探针提前恢复）/ `authoritative` / `credit` / `tier`。只有 `heuristic` 允许提前解除。

**冷却探针（把「猜的」变成「验的」）**

启发式冷却天然保守：上游 30 秒就好了，我们却关 2 分钟。后台每 60s 扫一次：

```
条件：已过 ≥50% + 剩余 ≥60s + source = heuristic
动作：用最小代价请求验证 key（/models 或 1 token 的 chat）
命中：清掉该 key 的全部 heuristic 冷却（跨模型）
失败：退避 2m → 4m → 8m → 15m，冷却本身不变
限额：每轮最多 3 个，首次启动加 0–45s 抖动
```

#### 3.6.8 时间预算与对冲（最容易被忽略的一节）

只做「失败后重试」不够。真实故障里最常见的是**上游不回包也不报错**——socket 挂着，30 秒没有首字节。如果预算只控制在「是否开始下一次重试」，用户会卡死在第一跳。

```
FALLBACK_BUDGET_MS = 45000        // 总墙钟预算
attempt 0          必跑（否则永远没有第一次）
attempt 1          必跑（保证至少一次接力机会）
attempt ≥ 2        检查 elapsed >= budget → 停止，返回 timedOut
```

- 每次尝试武装一个 `hedgeTimer`，到点直接 `AbortController.abort()` 掐掉在途请求。
- **首字节到手立即 `disarmHedge()`**：流已经开始返回，再掐就是截断一个健康响应，且此时已无法接力（响应头已发出）。
- 预算耗尽返回 429 + `Retry-After`，body 明确写「已尝试 N 跳，最近可恢复时间 X」，绝不返回空 200。

#### 3.6.9 熔断与降级模式

| 机制 | 触发 | 行为 |
|---|---|---|
| **单请求熔断** | 同请求连续 8 次上游失败 | 立即 503 `upstream_unhealthy`，不再烧剩余候选的配额 |
| **全局降级** | 健康 provider 占比 < 50%，持续 60s | 进入 degraded：**关闭探索**，只按已知分数走；日志与 UI 明确标注 |
| **降级恢复** | 占比 ≥ 50%，持续 120s | 退出（滞回，防抖动） |
| **平台不可用** | 单平台连续 20 次 5xx | 平台级熔断 5 分钟，UI 显示「厂商故障」而非「你的配置错了」 |

#### 3.6.10 结算与学习

- **租约（lease）**：选 key 时写入「预估 token 占用」，请求结束在 `finally` 释放。没有租约，10 个并发请求会同时读到「还有余量」，然后一起打爆限额——这是 check-then-act 竞态，必须用租约关掉。
- **记账时机**：成功响应后按上游 `usage` 实写；失败不记 token，但记请求数（很多平台按请求计数）。
- **学限额**：上游报错里带 `Limit 30000, Requested 33476` 这类信息时，**只收紧不放宽**地写回模型限额。学到之后，下一次请求在本地就被拦住，不用再吃一次 413。
- **惩罚衰减**：429 惩罚值每 5 分钟衰减 1，最低 0。恢复靠时间，不靠人工重置。

#### 3.6.11 契约：响应头与幂等

| 头 | 内容 | 用途 |
|---|---|---|
| `X-Freeport-Route` | `groq/llama-3.3-70b/key#2` | 这一跳是谁答的 |
| `X-Freeport-Attempts` | `2` | 之前失败了几跳 |
| `X-Freeport-Trail` | `google/gemini-2.5-flash key1=rate_limited; groq/... key2=upstream` | 每跳的失败类别（≤10 条，1024 字符） |
| `X-Freeport-Detail` | 同上 + `t=0+1205ms` 耗时 | 默认关闭，排查时开（≤2048 字符，消息脱敏） |
| `Retry-After` | 秒 | 全部耗尽时告知最近恢复时间 |

`Idempotency-Key`：SHA-256(key) + 请求指纹（model + messages + temperature + tools）→ 24h 内重放同一响应；指纹不一致返回 409。**流式不缓存**，`finish_reason=length` 不缓存（截断的答案重放没有意义）。

#### 3.6.12 主循环伪代码（实现时的唯一真相）

```ts
const state = { skipKeys: new Set(), skipModels: new Set(), skipPlatforms: new Set() }
let lastError

for (let attempt = 0; attempt < MAX_RETRIES; attempt++) {           // 默认 20
  if (attempt > 0 && clientGone()) return                            // 没人在等，别再烧额度

  const budget = endpointBudget(route?.platform, BASE_BUDGET_MS)     // 默认 45s
  if (attempt > 1 && elapsed() >= budget) return exhausted('timed_out')

  const route = routeRequest({ estimatedTokens, ...state, task })     // 重算候选集
  if (!route) return routingExhausted(diagnostics)                    // 零次尝试 → 503/429 分类

  const lease = acquireLease(route, estimatedTokens)                  // 并发可见
  const hedge = armHedge(budget - elapsed(), () => abortInFlight())    // 掐死卡住的请求

  try {
    const res = await dispatch(route, { signal, onFirstByte: disarmHedge })
    recordSuccess(route, res.usage)                                   // 后验 + 记账 + 清窗口
    return res
  } catch (err) {
    if (isHedgeAbort(err) || isClientAbort(err)) return exhausted('timed_out')

    const cls = classify(err)                                         // 见 3.6.7 矩阵
    if (cls.scope === 'key')      state.skipKeys.add(route.keyRef)
    if (cls.scope === 'model')    state.skipModels.add(route.modelDbId)
    if (cls.scope === 'platform') state.skipPlatforms.add(route.platform)

    applyCooldown(route, cls)                                         // 阶梯 + Retry-After + provenance
    applyPenalty(route.modelDbId, cls.penalty)                        // 重 3 / 轻 1 / 无
    if (cls.learnLimit) learnLimitFromError(route.modelDbId, err)      // 只收紧
    if (!cls.retryable) return fatal(err)                             // 400 非法参数等直接返给用户
    if (++breaker >= BREAKER_LIMIT) return upstreamUnhealthy()         // 连续 8 次 → 503
    lastError = err
  } finally {
    releaseLease(lease)                                               // 任何退出路径都释放
    disarmHedge()
  }
}
return exhausted('retries_exhausted', lastError)                      // 按「最能解释的一类」聚合
```

**耗尽时的状态码必须诚实**（这条几乎决定了用户对产品的信任）：

| 情况 | 返回 |
|---|---|
| 全部是鉴权失败 | 502 `provider_authentication_failed`（不是你的 key 错） |
| 全部是上下文超限 | 413 `context_length_exceeded` |
| 全部是限流/额度 | 429 + `Retry-After` + 最近恢复时间 |
| 混合失败 | 502 `upstream_failed` + 每类失败计数 |
| 一个 provider 都没配 | 503 `no_providers_configured` |

最后一行特别重要：**永远不要返回 500**。500 意味着「网关自己崩了」，而这里所有失败都是上游的。用户看到 500 会来提 issue，看到 502 + 明细会去加 key。

### 3.7 协议入口

| 入口 | 路径 | 覆盖客户端 |
|---|---|---|
| OpenAI Chat | `POST /v1/chat/completions` | Cline, Continue, Aider, OpenCode, Roo, Kilo, Crush, 绝大多数 SDK |
| OpenAI Responses | `POST /v1/responses` | Codex CLI (`wire_api = "responses"`) |
| Anthropic Messages | `POST /v1/messages`（含 `count_tokens`） | Claude Code, Anthropic SDK |
| Gemini | `POST /v1beta/models/{m}:generateContent` / `:streamGenerateContent` | Gemini CLI, Google SDK |
| Ollama | `/api/tags`, `/api/chat`, `/api/generate` | Zed, JetBrains AI |
| Meta | `GET /v1/models` | 全部 |
| 运维 | `GET /health`, `GET /metrics`(可选) | 自身 |

**必须处理的转换细节（踩坑清单）**
- `tool_calls` 增量：OpenAI 是 `delta.tool_calls[i].function.arguments` 字符串拼接；Anthropic 是 `content_block_delta(input_json_delta)`；Gemini 是 `functionCall.args` 整体对象 → IR 统一为「按 index 累积 + 结束发完整」 
- `reasoning_content`：DeepSeek/GLM 系需要回传；部分客户端会丢弃 → 做 reasoning echo 缓存
- `max_tokens` 语义差异：Anthropic 必填、Gemini 是 `maxOutputTokens`、部分模型把思考 token 算在内 → 统一 `maxTokens` 并在 adapter 内换算
- `system` 位置：Anthropic 顶层 `system`，OpenAI 是 message，Gemini 是 `systemInstruction`
- 图片：OpenAI `image_url`(data URL/base64)、Anthropic `source.base64`、Gemini `inlineData`
- 空 `content` + 只有 tool_calls 的 assistant 消息：多数上游会 400
- SSE 结束：`data: [DONE]` vs `message_stop` vs Gemini 无终止符
- 上游强制流式（如某些 Zen 模型）：非流式请求需内部聚合后再返回

**建议**：先只做「OpenAI 入口 + 全 adapter」，Anthropic/Gemini 入口在 P1。因为 90% 的 agent 能说 OpenAI，而 Claude Code 必须 Anthropic。

### 3.8 一键注入（Inject）

**Agent Adapter 插件模型**（配置文件驱动，不写死代码）

```jsonc
{
  "id": "claude-code",
  "name": "Claude Code",
  "detect": { "paths": ["~/.claude"], "bin": "claude" },
  "protocol": "anthropic-messages",
  "baseUrlStyle": "root",          // 根路径，客户端自己拼 /v1/messages
  "configType": "json",
  "targets": {
    "user":    ["~/.claude/settings.json"],
    "project": [".claude/settings.json"]
  },
  "write": { "strategy": "json-merge", "pointer": "/env" },
  "fields": {
    "ANTHROPIC_BASE_URL":  "{{baseUrl}}",
    "ANTHROPIC_AUTH_TOKEN": "{{apiKey}}",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "{{model}}"
  },
  "verify": { "request": "ping", "expect": "200" },
  "docsUrl": "https://docs.anthropic.com/en/docs/claude-code"
}
```

**注入流水线（六步，全部可 dry-run）**

```
1. Detect    路径/命令/版本探测 → 已安装列表
2. Plan      计算目标文件 + 变更内容 → 生成 unified diff
3. Backup    <file>.bak-freeport-<ts>（一次安装只备份一次原始文件）
4. Lock      文件级锁（避免与 agent 自身写入竞争）
5. Write     原子替换（tmp + rename），权限 0600（含密钥）
6. Verify    重读校验 + 一次健康请求 → 失败自动回滚
```

**合并策略（按文件格式）**

| 格式 | 策略 | 说明 |
|---|---|---|
| JSON / JSONC | 结构化 merge（保留未知字段、去尾逗号容错） | Claude Code, Cline, OpenCode, Continue |
| TOML | **托管块标记** `# >>> freeport managed >>> ... # <<< freeport managed <<<` | Codex CLI `config.toml` |
| YAML | 结构化 merge（注释会丢 → 先备份，UI 明示） | Goose, Hermes |
| .env | 键级 merge，0600 | OpenClaw, DSH, Reasonix |
| 任意 | 复制配置片段（guide 模式） | Cursor 等闭源 GUI |

**必须做到的六件事**
1. **只动自己写的节点**：绝不覆盖用户手写 provider/model
2. **幂等**：重复注入结果一致，不产生重复条目
3. **可回滚**：一键恢复 + 退出时可选「摘掉自己的条目/保留」
4. **作用域**：用户级 / 项目级（`--scope project` 写进仓库，适合团队）
5. **变更可见**：diff 预览 + 写入后展示「改了哪几行」
6. **不泄密**：日志与 UI 不显示完整 key（只显示后 4 位），文件 0600

**v1 支持清单（12 个自动注入 + 2 个引导式）**

| # | Agent | 协议 | 目标文件 | 难度 |
|---|---|---|---|---|
| 1 | Claude Code | Anthropic | `~/.claude/settings.json` (`env`) | 低 |
| 2 | Codex CLI | Responses | `~/.codex/config.toml` | 中（TOML 托管块） |
| 3 | Cline | OpenAI | VS Code globalStorage `saoudrizwan.claude-dev/settings/cline_settings.json` | 中（多平台路径） |
| 4 | Continue | OpenAI | `~/.continue/config.yaml` | 中（YAML） |
| 5 | OpenCode | OpenAI | `~/.config/opencode/opencode.json` | 低 |
| 6 | Gemini CLI | Gemini | `~/.gemini/settings.json` + `.env` | 低 |
| 7 | Aider | OpenAI | `~/.aider.conf.yml` + `.env` | 低 |
| 8 | Roo Code | OpenAI | VS Code globalStorage `rooveterinaryinc.roo-cline/settings/` | 中 |
| 9 | Kilo Code | OpenAI | VS Code globalStorage `kilocode.kilo-code/settings/` | 中 |
| 10 | Goose | OpenAI | `~/.config/goose/config.yaml` | 中（YAML） |
| 11 | Crush | OpenAI | `~/.config/crush/crush.json` | 低 |
| 12 | WorkBuddy | OpenAI | `~/.workbuddy/models.json` | 低（参考 OC Ferry） |
| 13 | Cursor | OpenAI | 引导式（应用内填写，附截图） | — |
| 14 | 通用 OpenAI 客户端 | OpenAI | 生成配置片段 / 一键复制 | — |

**VS Code 系路径解析**：需按平台枚举 `~/Library/Application Support/Code/User`（mac）、`%APPDATA%/Code/User`（win）、`~/.config/Code/User`（linux），并同时覆盖 Insiders / VSCodium / Cursor 变体。检测不到即为「未安装」，不创建目录。

后续可扩展：Qwen Code / OpenClaw / Hermes / Pi / Reasonix / DSH（freellmapi 已有 20 个实现可参考）。

### 3.8.1 Auth Broker（Tier C 的核心子系统）

全量支持订阅与 Cookie，意味着「凭据」不再只是 API Key，而是五种形态。必须有一个统一凭证生命周期层。

```ts
type CredentialKind =
  | 'api_key'         // 静态 key
  | 'oauth_pkce'      // 公共客户端 + PKCE（授权码）
  | 'oauth_device'    // 设备码
  | 'session_cookie'  // 内置窗口登录后的 cookie jar
  | 'local_token'     // 本地运行时签发的 token（Ollama 等）

interface Credential {
  id: string
  providerId: string
  kind: CredentialKind
  secretRef: string            // Keychain 引用，不存明文
  scopes?: string[]
  expiresAt?: number
  refreshable: boolean
  fingerprint: { ua: string; locale: string; tz: string }  // 保持请求一致性
  status: 'ok' | 'expiring' | 'expired' | 'revoked' | 'blocked'
}
```

**四条授权路径**

| 路径 | 实现 | 适用 |
|---|---|---|
| API Key | 用户粘贴 → 0600 临时文件 → Keychain → 立即验证 | Tier A/B |
| OAuth PKCE | 内置 `BrowserWindow` 打开授权页 → 回环 `127.0.0.1/callback` 收 code → 换 token | C1 主流 |
| Device Code | 展示 user_code → 轮询 token endpoint | C1（无回环的 provider） |
| Cookie 会话 | `BrowserWindow` + `session.fromPartition('persist:provider-x')` → 登录后读 `cookies.get()` → 存加密 jar | C2 |

**硬性技术约束**

1. **只用公共客户端 + PKCE**，绝不把 `client_secret` 打进客户端（二进制必然被提取）。若某 provider 只支持 confidential client → 该 provider 不做，或改为用户自填自有 client id。
2. 每个 provider 独立 `partition`，登录态互不污染，退出即清。
3. Token 刷新用「提前 5 分钟 + 单飞（single-flight）」防止并发刷新把 refresh token 打废。
4. 刷新失败 → 标记 `expired`，UI 显示「需重新登录」，不静默重试。
5. 请求头一致性：同一凭证固定 UA / Accept-Language / 时区，避免「同一账号指纹跳变」触发风控。
6. **不做**：自动注册、验证码识别、多账号轮换刷额度、伪造设备指纹。

**风控与节流（Tier C 必须默认开启）**

| 措施 | 默认值 |
|---|---|
| 单凭证并发上限 | 2 |
| 最小请求间隔 | 800ms（可 per-provider 覆盖） |
| 429/403 后熔断 | 15 min，阶梯退避 |
| 同 provider 并发总上限 | 8 |
| 异常模式检测 | 连续 5 次 403 → 暂停该凭证并通知 |

**首批 Tier C 接入清单（v1 只做这两家 + 一个 Cookie 探路）**

| 顺位 | 目标 | 凭据形态 | 为什么选它 | 风险 |
|---|---|---|---|---|
| C1 | **OpenCode Zen** | API Key（官方签发）+ 可选 web 会话 | 官方就提供免费模型池（`opencode/` 前缀），有公开 API，接入成本最低；用户群里渗透率高 | 免费池模型会轮换，需目录同步 |
| C2 | **GitHub Copilot** | OAuth Device Code（`ghu_` token） | 全平台最稳定的「订阅转 API」路径，设备码流程无需回环、无需 Cookie 库；Copilot 有明确的 `/chat/completions` 兼容端点 | 条款限制第三方客户端；只允许个人已订阅账号 |
| C3 | **OpenCode Zen web 会话** | `session_cookie`（内置窗口） | 作为 Cookie 路径的**唯一**试点，验证 jar 加密、刷新、失效检测全套链路 | 会话易失效；不做自动续期，只提示重新登录 |

**明确不做**：Cursor / Windsurf / Kiro 的客户端 token 提取。这三家都要读本地 IDE 的私有存储或注入进程，法律与稳定性双输，且封号概率最高。产品里保留「为什么不做」的说明页，比偷偷做更可信。

### 3.8.2 账号池（Pool）与配额归属

```
provider ─┬─ account A (api_key)  ─┐
          ├─ account B (oauth)     ├─▶ pool: "coding" ─▶ 路由候选
          └─ account C (cookie)   ─┘
```

- 配额账本按 **account × model × window** 记账，不按 provider 汇总
- 账号池按「用途」划分（coding / chat / vision），路由时只在该池内选
- 单账号被限 → 只降权该账号，不牵连同 provider 其他账号
- 账号可手动「暂停 / 仅低优先级使用」

### 3.9 安全模型

| 面 | 措施 |
|---|---|
| 监听 | 仅 `127.0.0.1`；局域网需显式开启并强提示 |
| 鉴权 | 本地随机 token（32B），`Authorization: Bearer`；可选「回环免鉴权」 |
| 来源 | 带 `Origin` 头的请求一律 403（端口不给浏览器用），防 DNS rebinding / 网页探测 |
| 密钥存储 | macOS Keychain / Windows DPAPI；DB 只存引用；导出需二次确认 |
| 会话 Cookie | 加密 jar（AES-256-GCM，密钥在 Keychain），按 provider 隔离 partition，退出可一键清除全部会话 |
| 登录窗口 | 独立 `BrowserWindow`，禁用 devtools、禁用外部导航白名单之外跳转，登录完立即销毁 |
| 凭证可见性 | UI 只显示后 4 位；复制需点击且 30s 后自动清剪贴板（可选） |
| 日志 | 正则脱敏（`sk-*`, `gsk_*`, `AIza*`, `hf_*`, `xai-*`）；请求体默认不落盘，仅元数据 |
| 注入文件 | 0600；备份同样 0600 |
| 更新 | 签名校验 + 可选关闭 |
| 网络 | 系统代理支持（HTTP/HTTPS），可选 per-provider 代理 |

### 3.10 数据模型（SQLite, WAL）

```sql
providers(id, name, tier, base_url, auth_type, homepage, enabled, quirks_json)
accounts(id, provider_id, label, kind, status, fingerprint_json, last_checked_at, created_at)
credentials(id, account_id, secret_ref, scopes_json, expires_at, refreshable,
            refresh_fail_count, session_partition, status)
account_pools(id, name, purpose)                      -- coding / chat / vision
pool_members(pool_id, account_id, priority, enabled)
models(id, provider_id, model_key, display_name, context, max_output,
       price_in, price_out, is_free, requires_card, deprecated,
       caps_json, source, enabled, first_seen_at, last_seen_at)
probe_results(model_id, account_id, level, status, latency_ms, detail, expires_at)
routing_profiles(id, name, is_default, strategy, weights_json)
routing_chain(profile_id, model_id, rank, enabled, account_pool_id)
quota_windows(account_id, model_id, window, used, `limit`, reset_at)
request_logs(id, ts, client, requested_model, resolved_model, provider_id,
             status, latency_ms, ttft_ms, tokens_in, tokens_out, error_code,
             trail_json)
route_decisions(id, request_id, candidates_json, chosen_json, reason)
agents(id, name, protocol, detected, config_paths_json, injected_at, state_json)
agent_bindings(agent_id, profile_id, base_url, api_key_ref, scope, managed_json)
settings(key, value)

-- 目录与运营
catalog_versions(version, channel, seq, applied_at, payload_hash, decision)
catalog_events(event_id, seq, kind, payload_json, published_at, applied_at, decision)
tombstones(model_id, deleted_at)          -- 用户删过的模型不被目录复活
discovered_providers(id, name, homepage, source, tier_guess, status,
                     rejected_reason, first_seen_at, last_seen_at)
model_families(id, canonical_key, display_name)
model_family_members(family_id, model_id)

-- 巡检
health_checks(id, target_kind, target_id, level, status, latency_ms,
              error_class, cost_tokens, ran_at)
health_state(target_kind, target_id, state, consecutive_fail,
             last_ok_at, next_check_at, score, updated_at)
probe_budget(day, used, `limit`, by_provider_json)

-- 扩展
extensions(id, name, version, manifest_json, status, port,
           installed_at, last_started_at, crash_count)
```

### 3.11 目录结构（建议）

```
free-llm-plus/
├─ apps/
│  ├─ desktop/            # 壳：托盘、窗口、自启、更新、单实例
│  ├─ ui/                 # React 前端（构建产物内嵌）
│  └─ gateway/            # HTTP 服务 + 路由 + 适配器
├─ packages/
│  ├─ core/               # Canonical IR、类型、错误模型
│  ├─ providers/          # 每个厂商一个 adapter（声明式 + 少量代码）
│  ├─ translate/          # openai / anthropic / gemini / ollama
│  ├─ probe/              # 探针引擎（liveness / capability）
│  ├─ health/             # 巡检调度、预算、健康状态机、digest
│  ├─ inject/             # agent 适配器 + 合并/备份/回滚
│  ├─ catalog/            # 内置基线 + 签名 feed + 事件应用
│  ├─ extensions/         # Extension Host：manifest 校验、supervisor、端口分配
│  └─ shared/             # DB schema、日志、脱敏、加密
├─ catalog/
│  ├─ catalog.json        # 内置基线（随版本）
│  └─ schema.json
├─ feed/                  # 仅维护者仓库：签名 feed 的构建产物与脚本
└─ docs/
```

---

### 3.12 扩展宿主（Extension Host）与 chat2api 的定位

**先给 chatgpt2api 定性**：它不是一个 LLM provider，而是「**OpenAI 账号池 + ChatGPT 官网逆向（以图片为主）**」的独立 Python 服务。它和 Freeport 主线（coding agent 的 chat 流量）只有两处交集，所以只能**挂成扩展**，不能合进主进程。

**值得直接吸收的五件事（现在就该进 Auth Broker）**

| 机制 | chatgpt2api 的实现值 | 移植到 Freeport |
|---|---|---|
| access token 提前刷新 | `_ACCESS_TOKEN_REFRESH_SKEW = 24h` | `expires_at - 24h` 触发刷新，而不是过期才刷 |
| refresh token 保活 | 3 天一次；失败退避 6h；批量 3 个 | 长期不用的账号也要保活，否则用户两周后回来全是死号 |
| 刷新失败退避 | 5 分钟 | 5m → 30m → 2h 阶梯，不打死 |
| 新号 / 无效判定防抖 | 新号 10 分钟宽限；30 秒内二次确认 | 与 3.5.1 状态机同一套参数 |
| 代理优先级链 | 账号级 > 稳定代理运行时 > 显式代理 > 全局 | 网络层四级解析，账号级最高 |

这些是跑过真实规模才有的参数，比自己拍脑袋定可靠得多。

**不值得吸收的**

- **逆向官网图片链路本身**：稳定性最差、法律风险最高；而 Freeport 的用户是 coding agent，图片不是主诉求
- **Python + Docker 运行时进主包**：包体 +80MB，签名 / 公证 / 自动更新全部复杂化

**集成路径（三步，每步独立可交付）**

| 阶段 | 做法 | 成本 | 价值 |
|---|---|---|---|
| **v1（现在）** | 已支持「自定义 OpenAI 兼容端点」。用户自行 `docker run` chatgpt2api，把 `127.0.0.1:3000/v1` + auth-key 填进 Freeport | 0（现有能力） | 立刻可用，产品不承担任何责任 |
| **v1.5** | **Extension Host**：可选安装 sidecar，Freeport 负责下载 / 校验 / 启动 / 健康检查 / 端口分配 / 卸载 | 2–3 周 | 一键装、统一日志、可建扩展目录 |
| **v2** | 扩展市场 + 签名分发 + 权限模型 | 4 周 | 生态 |

**Extension Host 最小协议**

```jsonc
// manifest.json
{
  "id": "chatgpt2api",
  "name": "ChatGPT Image Bridge",
  "version": "1.8.0",
  "kind": "sidecar",
  "runtime": "python",              // python | node | binary
  "entry": "main.py",
  "health": "GET /healthz",
  "capabilities": [
    { "modality": "image", "models": ["gpt-image-2"], "ops": ["generations", "edits"] }
  ],
  "permissions": { "network": ["chatgpt.com", "auth.openai.com"], "fs": ["data/"] },
  "resources": { "minRamMb": 256, "diskMb": 400 }
}
```

运行规则：

- **懒启动**：首次请求该 modality 才 spawn；空闲 10 分钟停止；崩溃重启 ≤3 次后标记 `broken`
- **端口隔离**：`127.0.0.1` 动态端口，只注册进网关内部路由表，不暴露给用户
- **进程隔离**：扩展崩溃不影响 chat 主链路，supervisor 只记录、不重启主网关
- **权限明示**：安装时列出「可访问域名 / 目录」——扩展持有用户凭据，这是最大风险点
- **只信任签名扩展**：作者公钥 + 哈希校验；未签名只能在「开发者模式」加载

**Gateway 的扩展点（v1 只留接口，不写实现）**

`/v1/images/*`、`/v1/audio/*` 按 `modality` 路由到扩展；Canonical IR 增加 `ImageRequest / ImageResponse`，chat 主链路完全不受影响。

现在加一个 `modality` 字段 + 一张 `extensions` 表，比将来重构路由层便宜一个数量级。

---

## 4. 功能设计

### 4.1 优先级

| 级别 | 功能 | 说明 |
|---|---|---|
| **P0** | 网关 + OpenAI 入口 + `/v1/models` | 能跑通一个 Cline |
| **P0** | Tier A 12 家 provider + key 管理 | Groq/Gemini/Cerebras/Mistral/NVIDIA/Cloudflare/GitHub/OpenRouter/SiliconFlow/ModelScope/Zhipu/Cohere |
| **P0** | `liveness` 探针 + 能力探测（P1–P3）+ 能力标签 + 缓存 | 没有这个就是「假可用」 |
| **P0** | **巡检调度 + 探针预算 + 健康状态机** | 否则巡检会把用户额度吃光，或一次抖动判死整个池子 |
| **P0** | `auto` 路由 + 失败回退 + 冷却 | 体验核心 |
| **P0** | 注入 Claude Code / Codex / Cline / Continue / OpenCode | 一键接管 |
| **P0** | 请求日志 + 路由决策 | 出问题能解释 |
| **P0** | 托盘 + 开机自启 + 退出清理 | 常驻形态 |
| **P1** | Anthropic + Gemini 入口 | Claude Code / Gemini CLI 原生 |
| **P1** | **Auth Broker**：OAuth PKCE / Device Code / 内置窗口 Cookie / token 刷新 | Tier C1/C2 的地基 |
| **P1** | **账号池 + 风控节流 + 凭证级配额** | Tier C 不被打爆、不触发封禁 |
| **P1** | 注入扩展至 12 个（+ Gemini CLI / Aider / Roo / Kilo / Goose / Crush / WorkBuddy）+ 项目级 scope | 覆盖度 |
| **P1** | 每日 Digest + 变更通知（托盘角标 + 事件流） | 让「自动」变成「用户知道」 |
| **P1** | 四窗口配额账本 + 多 key | 防 429 |
| **P1** | 路由策略切换 + 链编辑 + 粘性会话 + 上下文交接 | 可调优 |
| **P1** | 响应缓存（精确匹配）+ 用量看板 | 省额度 |
| **P1** | **签名目录 feed + 变更事件 + 应用策略（auto/notify/pinned）** | 「新模型 / 配额 / 修复发布即到」的唯一正确做法 |
| **P1** | **Discover 页：候选池、未收录厂商、一键启用** | 让「自动发现」可见、可控 |
| **P1** | 代理设置（账号级 > 运行时 > 全局） | 网络环境；Tier C 必需 |
| **P2** | 应用自动更新 + feed 双通道（stable/beta） | 免发版修复客户端 |
| **P2** | **Extension Host + chat2api 集成（v1.5）** | 图片等非 chat 能力的唯一合理载体 |
| **P2** | 维护者 Admin：feed diff 审阅 / 事件撤回 / 强制下架 | 没有它，「发布即到」=「事故即到」 |
| **P2** | MCP server（让 agent 自省可用模型） | 差异化 |
| **P2** | prompt/工具输出压缩（RTK 类） | 省 token |
| **P2** | 导入导出、i18n、崩溃上报（opt-in） | 发行需要 |

### 4.2 关键用户流程

**首次启动（目标：3 分钟内看到第一次成功请求）**

```
启动 → 检测已安装 Agent（并行）
     → 展示「推荐免费厂商」卡片（按地区/可达性排序）
     → 用户点「获取 Key」跳转 → 粘贴 → 自动探针（并发 3）
     → 可用模型列表（带能力徽章）
     → 「注入到 Claude Code」→ diff 预览 → 确认
     → 完成页：base URL / key / 一行验证命令
```

**日常（目标：零操作，但每一步都可解释）**

```
托盘常驻
  → 热模型 6h / 冷模型 7d 的 liveness 巡检（受每日预算约束）
  → 冷却到期前 60s 自动复检，提前解除
  → 目录 feed 差量检查：新模型进 candidate，探针通过后 0.3 倍权重灰度
  → 每日 03:00 全量审计 + Digest：「新增 3 · 下线 1 · 恢复 2；探针 137/300」
  → 只有「需要用户动手」的事才通知（额度耗尽需换 key、OAuth 过期需重登）
```

**故障（目标：10 秒内知道原因）**

```
请求失败 → 日志卡片：为什么没选 A（配额耗尽，2m 后恢复）
                    为什么没选 B（不支持 tools）
                    最终用了 C（延迟 840ms）
```

---

## 5. UI 设计

> **可交互原型：`design/prototype.html`**（单文件、离线可用）。8 个页面可切换，支持浅/深色与中英切换；Routing 页的三根权重滑杆会实时重排链路，并带一次「429 → 402 → 成功」的接力演示。评审以该原型为准。

### 5.1 信息架构

```
┌ 侧边栏 ─────────┬─────────────────────────────────────────┐
│ ● 概览          │                                         │
│   Models        │           主内容区                       │
│   Providers     │                                         │
│   Accounts      │  ← Tier C：OAuth / Cookie / 账号池       │
│   Discover  ②   │  ← 未收录厂商 / 候选模型（角标=新发现）  │
│   Routing       │                                         │
│   Agents        │                                         │
│   Extensions    │  ← 扩展宿主（图片等非 chat 能力）        │
│   Logs          │                                         │
│   Settings      │                                         │
│                 │                                         │
│ ─────────────── │                                         │
│ 网关 ● 运行中    │                                         │
│ 127.0.0.1:8787  │                                         │
└─────────────────┴─────────────────────────────────────────┘
```

**设计定位**：不是「AI 聊天产品」，是「网络控制台」。参考对象：Raycast 的密度 + Linear 的克制 + Cloudflare Dashboard 的信息层次。

### 5.2 设计语言

| Token | 值 | 说明 |
|---|---|---|
| 主色 | `#4F46E5` (indigo-600) | 单一强调色，只用于主操作与选中态 |
| 成功/健康 | `#059669` | 可用、200 |
| 警告/降级 | `#D97706` | chat-only、额度低 |
| 危险/失败 | `#DC2626` | 401/429/不可用 |
| 中性底 | `#FAFAF9` / 卡片 `#FFFFFF` | 暖白，避免纯灰的冷感 |
| 边框 | `#E7E5E4` | 1px，弱对比 |
| 文本 | `#1C1917` / 次 `#78716C` | 高对比正文 |
| 圆角 | 卡片 12 / 控件 8 / 徽章 6 | 统一 |
| 字体 | UI: Inter / -apple-system；数据: JetBrains Mono | key、端口、模型 id 用等宽 |
| 间距 | 4 的倍数，卡片内 20，区块间 32 | |
| 阴影 | `0 1px 2px rgba(0,0,0,.04)` 仅悬浮层 | 平面优先 |

浅色为主，深色跟随系统。不做渐变、不做玻璃拟态（除托盘 popover 可用轻模糊）。

### 5.3 页面规格

#### ① 概览（Dashboard）

```
┌────────────────────────────────────────────────────────────┐
│ 免费网关正在运行                            [复制地址] [暂停] │
│ http://127.0.0.1:8787/v1   ·   Key sk-…9f2c                │
├──────────────┬──────────────┬──────────────┬───────────────┤
│ 今日请求      │ 可用模型      │ 平均 TTFT     │ 已注入 Agent   │
│ 1,284        │ 37 / 62      │ 620 ms       │ 3             │
│ ↑ 12%        │ 5 个仅对话    │ p95 2.1s     │ Claude/Codex… │
├──────────────┴──────────────┴──────────────┴───────────────┤
│ 路由健康（近 24h 堆叠柱：成功/回退/失败）      [时间范围 ▾]   │
│ ▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆▇                            │
├────────────────────────────────────────────────────────────┤
│ 需要处理                                                    │
│ ⚠ Groq 日额度已用 92%（约 40 分钟后重置）        [换模型]     │
│ ⚠ 2 个模型不支持工具调用，已从 coding 路由移除    [查看]     │
├────────────────────────────────────────────────────────────┤
│ 最近请求                                                    │
│ 10:42  claude-code  auto→groq/llama-3.3-70b  200  680ms  │
│ 10:41  cline        auto→gemini/2.5-flash    200  1.2s   │
│ 10:40  codex        ⇄ fallback (429) → cerebras/…  200    │
└────────────────────────────────────────────────────────────┘
```

要点：首屏回答五个问题——**在跑吗 / 有多少能用 / 快不快 / 接了几个 / 今天变了吗**。

第五块是「今日变更」（目录事件流，不是版本号）：

```
今日变更                                          昨天 03:00 审计
＋ 3 个新模型   cerebras/qwen3-coder-480b · groq/llama-4-scout …
－ 1 个下线     openrouter/deepseek-r1:free（上游移除）
↺ 2 个恢复     gemini-2.5-flash · mistral-small-3.2
探针预算 137 / 300       [查看全部]  [暂停自动应用]
```

只显示**会影响决策**的变更；纯元数据（描述、图标）变动折叠。

#### ② Models（核心页）

```
[搜索模型]  [全部厂商 ▾] [能力: ☑工具 ☑视觉 ☐JSON] [仅可用 ☑]   [探测全部]
──────────────────────────────────────────────────────────────────────
▾ Groq                                            12 模型 · 免费 30 RPM
  ● llama-3.3-70b-versatile   ctx 128K  ⚡680ms  🛠 视觉–  JSON✓   可用
  ● llama-4-scout-17b         ctx 128K  ⚡420ms  🛠 视觉✓  JSON✓   可用
  ◐ qwen3-32b                 ctx 32K   ⚡—      🛠 视觉–  JSON?   仅对话
  ○ mixtral-8x7b              ctx 32K   —       —                不可用(401)
▾ Google AI Studio                                8 模型 · 免费 15 RPM
  ● gemini-2.5-flash          ctx 1M    ⚡820ms  🛠 视觉✓  JSON✓   可用
```

行内交互：点行展开详情（探针明细、错误原文、最近 10 次请求、加入/移出路由）；行尾 `⋯` 菜单（只测这一个 / 禁用 / 复制 id）。

#### ③ Providers

卡片网格。每张卡：

```
┌────────────────────────────────┐
│ Groq                    ● 已连接 │
│ 免费额度 30 RPM / 14.4K RPD     │
│ 12 个模型 · 工具调用 ✓           │
│ [管理 Key]  [重新探测]  [文档 ↗] │
└────────────────────────────────┘
```

未连接卡片显示「获取免费 Key ↗」+ 一步粘贴框。Tier C 卡片在独立分组「订阅额度（需自行评估风险）」，默认折叠。

#### ③b Accounts（Tier C 专属）

```
┌ 账号池 ─────────────────────────────────────────────────────┐
│ [全部] [API Key 8] [订阅授权 3] [网页会话 2]                │
├─────────────────────────────────────────────────────────────┤
│ Kiro · 订阅          a***@gmail.com     ● 正常   剩余 42 次  │
│   OAuth · 到期 52 分钟后自动续期        [重新授权] [暂停] [×] │
├─────────────────────────────────────────────────────────────┤
│ Copilot · 订阅       b***@outlook.com   ◐ 即将到期          │
│   刷新失败，需重新登录                  [重新登录]          │
├─────────────────────────────────────────────────────────────┤
│ Zen · 网页会话       —                  ● 正常   会话 3 天   │
│   内置窗口登录 · partition: zen-1       [打开会话页] [清除]  │
└─────────────────────────────────────────────────────────────┘
```

交互要点：
- 「添加账号」先选授权方式，再开登录窗口；窗口内是**官方页面**，应用不接触密码
- 登录成功回到主窗，显示「已获取会话（不含密码）」+ 自动跑一次 P0 探针
- 失效账号不删除，保留历史，标明「需重新登录」
- 每个账号可见「本月已用 / 上游限额（已知时）」与「最近一次成功」
- 底部常驻一行免责：**额度归上游所有，可能随时变更；请遵守各服务条款**

#### ③c Providers（补充：Tier C 卡片）

Tier C 卡片不显示「免费」字样，改显示 `订阅额度` / `网页会话` 徽章，并附「风险说明 ↗」链接；首次启用需勾选确认（记录时间戳，可在设置里查）。

#### ④ Routing

```
策略  ○ Balanced  ○ Fastest  ○ Smartest  ○ Reliable  ○ 自定义
──────────────────────────────────────────────────────────────
虚拟模型           候选链（拖拽排序）
auto           →  1. groq/llama-3.3-70b     可靠 0.94  余量 62%
               →  2. gemini/2.5-flash       可靠 0.91  余量 88%
               →  3. cerebras/llama-3.3-70b 可靠 0.88  余量 71%
               →  4. ollama/qwen2.5-coder   本地兜底

free-fast      →  …
free-smart     →  …
coding         →  …（仅 tools 能力模型）
```

右侧「为什么这样排」面板：显示每个候选的四维分数与门禁结果。

#### ⑤ Agents

```
已检测到 4 个 Agent

● Claude Code      Anthropic Messages   ~/.claude/settings.json
  已注入 · 12 分钟前 · 2 个模型              [查看 diff] [恢复原配置]
● Codex CLI        OpenAI Responses     ~/.codex/config.toml
  已注入 · 托管块 3 行                       [查看 diff] [恢复]
○ Cline            未检测到安装
○ Cursor           需在应用内手动填写       [复制配置]
```

注入弹窗：左侧原文件、右侧注入后，逐行高亮；底部「将备份到 …」+「确认注入 / 仅复制」。

#### ⑥ Logs

表 + 抽屉。列表列：时间、客户端、请求模型、实际模型、状态、TTFT、tokens、回退次数。
抽屉内容：请求元数据、候选链与淘汰原因、耗时瀑布（排队/连接/首字节/流式）、错误原文（脱敏）、`复制为 curl`。

#### ⑥b Discover（发现）

这是「自动收录」唯一可见的地方，也是它可被信任的原因。

```
发现                                                   [立即扫描]
──────────────────────────────────────────────────────────────────
可直接启用（已验证，无需 key）
┌──────────────────────────────┐ ┌──────────────────────────────┐
│ Pollinations          匿名   │ │ LLM7                  匿名   │
│ 6 个模型 · 无需注册          │ │ 3 个模型 · 公共端点          │
│ 验证：2 分钟前 · 延迟 840ms   │ │ 验证：18 分钟前              │
│                  [启用]      │ │                  [启用]      │
└──────────────────────────────┘ └──────────────────────────────┘

需要 Key（有免费额度）
┌──────────────────────────────┐ ┌──────────────────────────────┐
│ Together AI        Tier A    │ │ Hyperbolic         Tier A    │
│ 免费 $1 试用 · 需绑卡 ⚠      │ │ 5 个模型 · 无需绑卡          │
│ [获取 Key ↗]  [我已粘贴]      │ │ [获取 Key ↗]  [我已粘贴]      │
└──────────────────────────────┘ └──────────────────────────────┘

候选池（正在验证，不进路由）
· nvidia/nemotron-4-340b      第 1/2 次探测失败，明天重试
· codestral-latest            不支持 tools → 仅对话
```

规则：候选池的东西**不出现在 Models 页、不进路由**；用户主动启用才转入正式流程。低置信来源（社区清单）额外标注 `来源：社区，未验证`。

#### ⑥c Extensions（扩展宿主）

```
扩展                                       [浏览目录]  [开发者模式]
──────────────────────────────────────────────────────────────────
● ChatGPT Image Bridge   1.8.0   运行中   端口 51234   内存 96 MB
  image: gpt-image-2 / codex-gpt-image-2        [停止] [日志] [卸载]
  权限：chatgpt.com · auth.openai.com · data/
  ⚠ 该扩展通过网页会话访问第三方服务，账号风险自负

○ Whisper Local          0.3.2   已停止   —            —
  audio: transcribe                              [启动] [卸载]
──────────────────────────────────────────────────────────────────
资源占用 96 MB / 2.1 GB 磁盘
```

安装流程强制三步：**看权限 → 看签名 → 确认**。未签名扩展只能从「开发者模式」加载，且每次启动都提示。

#### ⑦ Settings

分组：

| 分组 | 内容 |
|---|---|
| 网关 | 端口、鉴权、回环绑定 |
| 启动 | 自启、启动时最小化、退出清理 |
| 网络 | 代理（账号级 / 运行时 / 全局优先级）、超时、并发上限、证书 |
| **目录** | 应用策略 `auto / notify / pinned`、频道 `stable / beta`、扫描间隔、手动导入 JSON |
| **巡检** | 每日审计时间、探针预算上限、冷却复检开关、通知阈值 |
| **扩展** | 已装扩展、开发者模式、扩展数据目录 |
| 数据 | 备份、导出、清空日志、日志保留天数 |
| 外观 | 主题、语言（中/英） |
| 关于 | 版本、检查更新、许可、EULA |

#### 托盘 popover（380×420）

```
┌──────────────────────────────┐
│ Freeport           ● 运行中   │
│ 127.0.0.1:8787               │
├──────────────────────────────┤
│ 今日 1,284 请求 · 37 个模型   │
│ ▁▂▃▅▆▇▆▅▃▂▁▂▃▅▆             │
├──────────────────────────────┤
│ [复制 Base URL]  [复制 Key]  │
│ [打开控制台]     [暂停路由]   │
├──────────────────────────────┤
│ 3 个 Agent 已接入            │
└──────────────────────────────┘
```

### 5.4 空状态与错误态（决定质感）

| 场景 | 设计 |
|---|---|
| 没有任何 provider | 三张推荐卡（Groq / Gemini / OpenRouter）+「为什么选它们」一句话 |
| 全部模型不可用 | 顶部红色横幅 + 「最近一次成功请求是 X 分钟前」+ 一键诊断 |
| 探测中 | 行内进度条 + 「还剩 14 个」，不阻塞 UI |
| 429 全耗尽 | 明确写出「最近恢复时间 2m14s」，不显示空响应 |
| 端口被占用 | 自动换端口并在 UI 提示「已改用 8788」 |

---

## 6. 技术选型（已定稿）

```
Electron 38 (Chromium 140 / Node 22)
TypeScript 5.9 · esbuild 打包主进程
UI:      React 19 + Vite 7 + Tailwind CSS v4 + Radix Primitives
状态:    Zustand（本地 UI 态） + TanStack Query（异步/缓存）
图表:    Recharts
HTTP:    Hono + @hono/node-server（主进程内，127.0.0.1）
DB:      better-sqlite3（WAL，单例，仅主进程）
加密:    keytar → macOS Keychain / Windows DPAPI
Cookie:  Electron session（persist: partition） + AES-256-GCM 加密 jar
更新:    electron-updater + GitHub Releases
打包:    electron-builder（dmg / zip / nsis）
测试:    Vitest（单元/集成） + Playwright（Electron E2E + 注入到临时 HOME）
日志:    pino（结构化） + 正则脱敏 + 滚动文件
```

**为什么不用 Tauri**：本项目 70% 的工作量在「协议转换 + 厂商 quirk + 注入合并」，这些在 temp 里有大量 TS 现成实现；用 Rust 重写会把 MVP 从 ~6 周拉到 ~11 周。包体劣势用「首装下载 + 后续差量更新」缓解。

**主进程职责边界（防止架构腐化）**

| 允许在主进程 | 必须放渲染进程 |
|---|---|
| HTTP 服务、DB、Keychain、Cookie jar、文件写入 | 所有 UI 渲染、图表、表单 |
| 注入的读/写/备份/回滚 | diff 的展示与确认交互 |
| 探针调度 | 探针结果的排序与筛选 |

渲染进程**不直接** `fs` / `net`：通过 `contextBridge` 暴露受限 API（白名单通道），`nodeIntegration: false`、`contextIsolation: true`、`sandbox: true`。

### 6.1 发布架构（mac + win）

| 项 | 方案 |
|---|---|
| macOS 签名 | Developer ID Application + Hardened Runtime + `notarytool` 公证 + staple |
| macOS 架构 | arm64 与 x64 分别构建（原生模块需对应 ABI） |
| Windows 签名 | Azure Trusted Signing（EV）或 OV 证书；NSIS 安装包 + 便携版 |
| 更新通道 | `stable` / `beta` 双通道；`electron-updater` + `latest.yml` |
| 差量更新 | electron-builder 差分（blockmap），控制下载体积 |
| 崩溃上报 | Sentry（**默认关闭**，opt-in 且脱敏：不含 prompt、key、路径） |
| 遥测 | 默认全关；仅「版本 + 平台 + 是否成功启动」可选上报 |
| 分发 | 官网 + GitHub Releases；**不上架 App Store / Microsoft Store**（Tier C 约束） |
| 合规物料 | EULA、隐私政策、Tier C 风险说明、下架/删除请求响应流程 |
| 自动更新开关 | 设置内可关；企业环境可用 `--no-update` |

**发布前必须通过的检查**

1. 干净机器（无 Node、无开发工具）双击可用
2. 升级安装不丢 DB、不覆盖用户配置
3. 卸载不残留自启项、不残留注入条目
4. 主进程崩溃后能自动重启并恢复监听
5. 日志中不出现任何完整 key / cookie 值（自动化正则扫描）
6. 首次启动 60 秒内可完成「一个 provider + 一次成功请求」

---

## 7. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 免费额度说变就变 | 用户觉得「骗人」 | 目录热更新 + 探针缓存 + 如实显示「最后验证时间」 |
| 模型「能聊不能干活」 | agent 中途失败，体验极差 | 强制 tools 探针门禁 + 能力徽章 + coding 路由只收 tools 模型 |
| 429 集中爆发 | 请求失败 | 四窗口账本 + 多 key + 熔断 + 明确恢复时间 |
| 注入改坏用户配置 | 信任崩塌 | 备份 + diff + 原子写 + 幂等 + 一键恢复 + 退出清理 |
| 协议长尾（工具调用/思考/多模态） | 特定 agent 不可用 | IR + 每 provider quirk 字段 + 可复现测试用例库 |
| 合规（Tier C） | 账号封禁、厂商函告、下架 | 仅用户自有账号 + 官方登录页；不代注册/不绕过付费；EULA 明确风险转移；提供「关闭全部 Tier C」开关；预留按 provider 远程禁用名单 |
| 公开发布后被逆向 | 内置逻辑与端点被抄、被滥用 | 不内嵌任何 client_secret；关键目录走签名 feed；异常用量遥测（opt-in）；保留远程 kill-switch |
| Cookie 会话失效频繁 | 用户反复重登 | 会话健康检查（每 30 min 一次轻量请求）+ 提前提示 + 一键重登 |
| OAuth provider 变更 client id | 授权整体失效 | client id 做成远程配置（签名 feed），不发版即可更新 |
| 签名/公证流程卡住 | 无法发布 | 提前 2 周申请 Developer ID 与 Windows 证书；CI 里先跑未签名构建 |
| 端口被占/防火墙 | 启动失败 | 端口扫描 + 自动切换 + 显式展示实际端口 |
| 密钥泄露 | 严重 | OS keychain + 日志脱敏 + 0600 + 不回显完整 key |

### 7.1 商业化：开源核心 + Pro（freellmapi 的启示）

freellmapi 已经跑通了一条很干净的商业化路径，值得直接借鉴其**结构**（不是照抄定价）：

| 层 | freellmapi 的做法 | 本质 |
|---|---|---|
| 免费 | 功能完整的网关；目录为**月度快照** | 产品可用，不阉割 |
| Premium | license key → **live tier 目录**（实时更新）；`/v1/license/activate` 校验、`/check` 定期复查、状态本地缓存；失效自动降级回月度快照，**不阻塞任何功能** | 卖「持续更新的情报」，不卖「解锁按钮」 |

这套设计的精髓有三点，我们必须保留：

1. **免费版必须是完整产品**。锁路由、锁重试次数、锁并发，都会让用户第一周就去找盗版/替代品。
2. **付费买的是「时效」而不是「权限」**。目录、额度情报、探针结果天然会过期，这是唯一合理的订阅理由。
3. **license 失效只能降级，不能罢工**。校验服务挂了、用户断网、订阅过期——网关必须照常转发请求。

**Freeport 的三档设计**

| 档 | 价格（建议） | 内容 | 工程实现 |
|---|---|---|---|
| **Free** | ¥0 | 全部 provider 接入、全部协议、全部注入器、无限请求、本地日志（7 天） | 目录走**内置基线 + 7 天快照**；探针本地跑 |
| **Pro** | ¥29/月 或 ¥199/年 | ① 实时目录（5–15 分钟级）② 社区额度情报（哪家此刻还能用、哪家在维护）③ 多机配置加密同步 ④ 云端并发探针（比本地快 10 倍发现新模型）⑤ 日志保留 90 天 | license = 签名 JWT（Ed25519），公钥内置，**离线可验**，7 天宽限期 |
| **Team**（后续） | 按席位 | 策略模板下发、团队共享的 Routing Profile、统一注入清单 | **只同步策略，不同步 key**——共享 key 会变成额度分销，法律与风控都不成立 |

**硬性约束（写进代码，不靠自觉）**

- 目录请求带 `Authorization: Bearer <license>`，服务端只返回对应 tier；**客户端不判断 tier**，只应用服务端给的数据 —— 避免「本地改个 flag 就解锁」。
- 校验失败/超时/无网：继续用最近一次成功同步的目录，UI 显示「目录已 N 天未更新」，**绝不阻断路由**。
- 不做：卖 key、转售额度、按 token 计费、代注册账号。这四条任何一条都会把产品从「工具」变成「灰产中间商」。
- 遥测默认关闭；开启后只上报「匿名模型可用性统计」，绝不上报 prompt、响应、key。

### 7.2 商业化对架构的反向要求

| 要求 | 设计决策 |
|---|---|
| 目录必须能「热替换且可回滚」 | 目录带 `version + tier + signature`；应用前校验签名与版本单调性；失败保留旧版 |
| 离线可用 | license 用非对称签名，公钥编译进二进制；不需要每次启动联网 |
| 免费/付费共用一条代码路径 | tier 只影响**数据源**，不影响任何功能开关——避免两套逻辑分叉 |
| 可灰度、可熔断 | 目录服务支持按版本回滚；客户端支持远程 `kill-switch`（仅用于下架违规 provider） |

---

## 8. 里程碑（按可验证结果切）

| 阶段 | 交付 | 验收标准 |
|---|---|---|
| M1 骨架 | Electron + Hono + SQLite + 托盘 + `/v1/models` + `/v1/chat/completions`（单 provider 直连） | `curl` 能拿到 Groq 回复 |
| M2 发现 | 目录 + key 管理 + liveness/能力探针 + 模型列表 UI | 录入 key 后 30s 内看到可用模型与延迟 |
| M2.5 巡检 | 巡检调度器 + 探针预算 + 健康状态机 + 每日 Digest | ① 连续运行 7 天，探针消耗不超过设定预算；② 手动让某模型 401，连续 2 次失败后才变 unavailable，且 30 分钟后自动复检；③ 单次 502 不改任何状态 |
| M3 路由与接力 | 三轴打分 + 守卫乘子 + 错误作用域矩阵 + 冷却阶梯 + 45s 预算 + 冷却探针 + 决策日志 | ① 手动把主 key 打到 429，请求在 **2 跳内**切到可用模型；② 杀掉一个 provider 的域名解析，请求在 45s 内返回 429/502 而非挂死；③ 日志能逐跳解释「为什么跳、跳了多久」 |
| M4 注入骨架 | 注入引擎（diff/备份/锁/原子写/回滚）+ 5 个 agent 适配器 | 注入 Claude Code 后能真实跑一轮对话；恢复后文件与原始一致 |
| M5 体验 | 概览/Providers/Models/Routing 页面 + 托盘 popover + 空状态 | 全流程无终端操作 |
| M6 协议加固 | Anthropic + Gemini 入口 + 配额账本 + 粘性会话 | Claude Code 原生协议跑通；连续 200 次请求无 429 泄漏 |
| M6.5 目录运营 | 签名 feed + 变更事件 + 应用策略 + Discover 页 + beta 频道 | ① 在 feed 里加一个假模型，客户端 5 分钟内出现（Free 6h 内）且带 candidate 标记；② 删除一个模型，路由立刻不再选中；③ 篡改 feed 签名，客户端拒绝应用并告警 |
| M7 Auth Broker | OAuth PKCE / Device / 内置窗口 Cookie + token 刷新 + 账号池 | 一个订阅类 provider 可稳定使用 ≥24h，自动续期 |
| M8 注入全覆盖 | 12 个适配器 + 项目级 scope + 引导式 | 每个适配器都有自动化测试（临时 HOME + 快照比对） |
| M9 发行 | 签名/公证/双架构/自动更新/崩溃上报/EULA | 干净机器安装即用；升级不丢数据；卸载无残留 |
| M10 扩展（v1.5） | Extension Host + chat2api 样板 | 可选安装后 `/v1/images/generations` 可用；扩展崩溃不影响 chat；卸载无残留 |

---

## 9. 决策与开放问题

### 已定（2026-10-19）

| # | 决策 | 结论 |
|---|---|---|
| 1 | 产品名 | **Freeport（自由港）** |
| 2 | 技术栈 | Electron 38 + TypeScript 全栈 |
| 3 | 合规 | Tier A/B/C 全量，含 OAuth 订阅与内置窗口 Cookie |
| 4 | Tier C 首批 | **OpenCode Zen + GitHub Copilot**；Cookie 只做 OpenCode Zen web 会话 |
| 5 | 注入 | 12 个适配器 + 2 个引导式 |
| 6 | 发行 | macOS + Windows，签名/公证/自动更新，官网与 GitHub 分发 |
| 7 | 局域网共享 | **不做** |
| 8 | 双语 | **需要**，中/英 |
| 9 | 本地 Ollama | **不接** |
| 10 | 商业化 | 开源核心 + Pro（实时目录/额度情报/多机同步） |
| 11 | 巡检 | **默认开启**；每日 03:00 全量 liveness + 热模型 6h；探针预算 `clamp(日配额×1%, 60, 800)`，与真实流量竞争永远让路 |
| 12 | 目录更新 | **签名 feed + 事件 + 三策略**（auto 默认 / notify / pinned）；新增模型必须经 canary（0.3 倍权重）才转正；单次删除 >15% 自动熔断 |
| 13 | chat2api | **不进主链路**。v1 走「自定义 OpenAI 兼容端点」；v1.5 走 Extension Host；只吸收其凭据保活与代理优先级参数 |

### 仍需你拍板（不阻塞 M1–M3）

| # | 问题 | 现状 / 建议 |
|---|---|---|
| 1 | Pro 定价 | 建议 ¥29/月、¥199/年；首年可做「早鸟终身 ¥299」换口碑 |
| 2 | 品牌视觉 | 需要你确认风格方向（我给的方案：暖白 + indigo，等宽字体做技术感） |
| 3 | 官网与文档语言 | 建议中英双语，GitHub README 英文优先、官网中文优先 |
| 4 | 开源许可证 | 建议 **AGPL-3.0 + 商业授权例外**：防止有人直接拿去卖 SaaS，同时不阻碍个人使用 |

---

## 10. 开工顺序（下一步）

```
Week 0  原型评审（design/prototype.html）→ 确认视觉与信息架构，再动工
Week 1  仓库骨架 + Electron/Hono/SQLite + /v1/chat/completions 打通 Groq
Week 2  目录(12家) + key 管理 + liveness/能力探针 + 巡检调度 + 预算 + Models 页
Week 3  路由三轴打分 + 守卫乘子 + 错误作用域矩阵 + 冷却阶梯 + 45s 预算 + 决策日志
Week 4  注入引擎 + Claude Code/Codex 两个适配器（做到可回滚、可幂等）
Week 5  概览/Providers/Routing 页 + 空状态 + 首次启动流程
Week 6  Anthropic 入口 + 配额账本 + 冷却探针 → 内测版（Tier A）
Week 7  签名 feed + 变更事件 + 应用策略 + Discover 页（「发布即到」闭环）
Week 8  Auth Broker（Device Code + 内置窗口 Cookie）+ OpenCode Zen / Copilot
Week 9  注入扩至 12 个 + 项目级 scope + 双语
Week 10 签名/公证/更新/EULA → mac+win 发布候选
Week 11 扩展宿主 + chat2api 样板（v1.5，可与发布并行）
```

**第 3 周是分水岭**：路由与接力的验收不是「能用」，而是「能解释」。如果第 3 周结束还做不到「任意一次失败都能在日志里说出跳了几跳、为什么跳、每跳花了多久」，就不要继续往前做功能——后面所有体验都建立在这层之上。

**第 7 周是第二个分水岭**：验收标准是「一个假模型从 feed 发出去，客户端在承诺时间内出现、带 candidate 标记、不立刻抢流量；删掉它，路由立刻不再选」。做不到这一点，「自动更新免费清单」就只是营销话术。

**第一周就能验证的最小闭环**：装 Groq key → 网关起在 8787 → Cline 改 base URL → 对话成功 → 日志能看到「为什么选了这个模型」。这个闭环跑通，后面都是加法。

---

## 附：可直接复用的参考坐标

| 要解决的问题 | 直接看 |
|---|---|
| provider 声明式注册表 | `9router/open-sse/providers/registry/*.js`（138 个样例） |
| 协议转换 | `9router/open-sse/translator/`（openai/claude/gemini/responses） |
| 路由打分与守卫 | `freellmapi/server/src/services/{router,scoring}.ts` |
| 失败分类与冷却 | `freellmapi/server/src/services/{ratelimit,cooldown-probe}.ts` |
| 目录同步与签名 | `freellmapi/server/src/services/catalog-sync.ts` |
| 模型发现与免费判定 | `freellmapi/server/src/services/model-discovery.ts` |
| Agent 注入器（结构化合并） | `9router/cli/src/cli/commands/connectTools.js` |
| Agent 注入器（托管块/卸载） | `router/install/install.sh` |
| 注入清单与协议矩阵 | `freellmapi/client/src/data/agent-tools.json` + `docs/zh-cn/clients/01-agent-clients.md` |
| 本地转发 + 生命周期清理 | `RuiC-oc-ferry/src/{main,server,sync}.js` |
| Electron 内嵌 server + 托盘 | `freellmapi/desktop/src/{main,server-host,tray}.ts` |
| 干净 MVP 骨架 | `freellm/gateway/src/` |
| 免费厂商结构化数据 | `awesome-free-llm-apis/data.json` |
| 凭据保活 / 刷新退避 / 状态机参数 | `chatgpt2api/services/account_service.py`（`_ACCESS_TOKEN_REFRESH_SKEW` / `_INVALID_CONFIRM_SECONDS` / 退避常量） |
| 代理优先级链 | `chatgpt2api/services/proxy_service.py` + `docs/deployment.md` |
| 账号池轮转与限流恢复 | `chatgpt2api/services/account_service.py`（pick / refresh_accounts） |
