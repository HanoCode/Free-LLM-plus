# ADR-001：运行时选型——从 Electron 单进程转向「单二进制 · 双形态」

- 状态：**已接受（2026-10-19 拍板，M1.5 已落地）**
- 日期：2026-10-19
- 决策者：项目负责人
- 影响面：§3.1 进程模型、§6 技术选型、§6.1 发布架构、M2 之后全部里程碑
- 参考：`temp/cc-switch`（Tauri 2 + Rust）、`temp/magpie`（Go + Wails v3）

---

## 1. 问题

已定稿的 v1.1 架构选择 Electron 38，理由是「70% 工作量在协议转换与厂商 quirk，temp 里有大量 TS 现成实现，Rust 重写会把 MVP 从 6 周拉到 11 周」。

这个理由在 M0 成立，但它回答的是「**谁写起来快**」，没有回答「**这东西要 7×24 常驻，代价是什么**」。Freeport 不是一个打开用十分钟的编辑器，它是一台**本机常驻的代理服务器**，UI 只是它的遥控器。把 Data Plane 焊死在 GUI 进程里，会产生三个结构性代价：

| 代价 | 表现 | 严重度 |
|---|---|---|
| 包体 | 242 MB 运行时，业务代码只有 1.8 MB | 中（首装流失） |
| 内存 | Chromium 多进程常驻，空载 200–300 MB | **高**（常驻进程的日常成本） |
| 生命周期 | 关窗即断服，只能靠托盘隐藏维持 | **高**（与「常驻网关」的产品定位冲突） |

第 3 条是架构问题，不是优化问题。Electron 里想做到「关窗后网关继续跑」，只能隐藏窗口——那 200 MB 还在；或者另起一个 Node 子进程——那等于承认要拆，但背上了两套运行时。

**结论：应该调整。但调整的核心不是「换掉 Electron」，而是把 Data Plane 从 GUI 进程里解耦出来。** 换壳只是解耦之后的自然结果。

---

## 2. 实测与可比数据

### 2.1 本机现状（Freeport v0.1，M1）

| 项 | 数值 |
|---|---|
| `node_modules/electron/dist` | **242 MB** |
| 前端构建产物 `dist/` | 1.8 MB |
| 应用源码 `src/ + electron/` | 304 KB |
| 运行时 : 业务代码 | **≈ 800 : 1** |

也就是说，用户下载的 100 MB 里有 99% 是 Chromium + Node，跟 Freeport 做了什么毫无关系。

### 2.2 同类产品的公开发布包（GitHub Releases 实测）

| 产品 | 技术栈 | macOS 包 | Windows 包 | 含什么 |
|---|---|---|---|---|
| **magpie** v0.1.1290 | Go + Wails v3 | **22.5 MB**（dmg） | 53.5 MB（exe） | 网关 + GUI + TUI + CLI + 60+ Agent 注入 + 目录 + 配额 |
| **cc-switch** v4.0.15 | Tauri 2 + Rust | **33.2 MB**（dmg） | 15.3 MB（msi） | 代理 + 协议转换 + SQLite + 60+ Agent 切换 + 用量统计 |
| **Freeport**（Electron 预估） | Electron 33 | **85–100 MB** | 80–95 MB | 网关 + GUI |

Windows 上差距更刺眼：magpie 53.5 MB 是**单文件绿色 exe**（无安装器），cc-switch 的 msi 只有 **15.3 MB**，而 Electron NSIS 普遍 80 MB 起。

### 2.3 内存量级（公开基线 + 架构推算，待 M0 spike 实测校准）

| 形态 | 空载 RSS | 说明 |
|---|---|---|
| Electron GUI | 200–300 MB | main + renderer + GPU + utility 四类进程 |
| Electron headless（当前 `FP_DAEMON`） | 55–75 MB | 纯 Node，无窗口 |
| Tauri / Wails GUI | 90–140 MB | 系统 WebView（WKWebView / WebView2）共享进程 |
| **Rust headless 网关** | **15–25 MB** | 无 WebView、无 GC |
| Go headless 网关 | 20–35 MB | Go runtime + GC 堆 |

关键不是「GUI 差 100 MB」，而是**常驻的那一份可以只有 20 MB**。用户真正在意的是「我后台一直开着的东西占多少」，而不是「我点开设置窗口那一下占多少」。

---

## 3. 两个参考项目的架构解构

### 3.1 magpie：单二进制 · 多形态 · 网关即库

这是本次最重要的参考。它把「一个网关产品」拆得极干净：

```
                    ┌─────────────────────────────┐
                    │      main.go（唯一入口）      │
                    └──────────────┬──────────────┘
                                   │
                    ┌──────────────▼──────────────┐
                    │      internal/cli            │  ← 命令分发
                    └──┬────┬────┬────┬────┬───────┘
                       │    │    │    │    │
        ┌──────────────┘    │    │    │    └──────────────┐
        ▼                   ▼    │    ▼                   ▼
   ┌─────────┐        ┌────────┐ │ ┌────────┐      ┌──────────┐
   │  GUI    │        │  TUI   │ │ │  web   │      │  serve   │
   │ (Wails) │        │(bubble)│ │ │(页面)  │      │(headless)│
   └────┬────┘        └───┬────┘ │ └───┬────┘      └────┬─────┘
        │                 │      │     │                │
        └─────────────────┴──────┼─────┴────────────────┘
                                 ▼
                      ┌─────────────────────┐
                      │  internal/gateway   │ ← 库，不是进程
                      │  （6789 行主干）      │
                      └──────────┬──────────┘
                                 │
        ┌────────────┬───────────┼───────────┬────────────┐
        ▼            ▼           ▼           ▼            ▼
   provider      catalog      routes      budget       agent
   (账号/组)     (模型目录)   (SWRR/策略)  (配额账本)   (注入器×60)
```

**四个可直接搬的设计：**

1. **`gateway` 是 lib crate，不是 bin。** GUI 进程内 `startBackend()` 直接调用它；`magpie serve` 也调用它；`magpie daemon install` 把这个 `serve` 写成 launchd/systemd 服务。同一份网关代码，三种宿主。
2. **`agent` 是一文件一适配器**（`claude.go` / `codex.go` / `cursor.go` …共 60+ 个），没有统一的注入 DSL，也没有抽象工厂。共享的只有托管块读写、原子写、备份这些工具函数。**过度抽象在长尾适配上是负资产。**
3. **目录不自建全量。** `catalog` 用 models.dev + 各厂商 `/models` 实时拉（`catalog.Sync` / `KeepFresh`）。自己只维护「免费额度元数据」这一层增量——这正好是我们 Pro 的卖点，而不是重复造 models.dev。
4. **路由用「rest」模型 + 平滑加权轮询（SWRR）。**
   - `restingUntil[key]` 记录「谁在休息、休息到什么时候、因为什么」——和我们的「错误作用域矩阵」是同一件事的两种写法，但它的**理由是自解释的**，能直接渲染成 UI 上的一句话（"预计 14:32 恢复，原因：额度耗尽"）。
   - `weighted.go` 用 NGINX 的 smooth WRR：权重 3:1 的序列是 `a,a,b,a,a,a,b,a`，**短期比例就准**，不是只有长期平均对。
   - `routes.Smart` 的策略一句话：**剩余额度最早刷新的先用**——因为不用的额度会过期作废，晚刷新的额度可以留着。这条比我们现在的三轴凸组合更接近「经济最优」，而且用户一听就懂。

5. **配额在请求前预留、finally 释放**（`gwbudget.Reserve` 返回 `release func()`），fail-closed。和我们 §3.6 的四窗口账本一致，可以确认这条设计是对的。

### 3.2 cc-switch：Tauri 2 的生产级验证

它证明了一件我们最需要确认的事：**Tauri 做「代理 + 三协议转换 + SSE 流式 + SQLite + 60 个 Agent 配置」是够用的，而且只有 33 MB。**

其 `src-tauri/src/proxy/` 的结构几乎就是我们要写的东西：

| cc-switch 模块 | 对应我们的层 | 备注 |
|---|---|---|
| `handlers.rs` / `server.rs` | Ingress | axum 路由表 |
| `provider_router.rs` | Route | 按优先级返回候选列表 |
| `circuit_breaker.rs` | Gate | **三态熔断（Closed/Open/HalfOpen）**，比纯 cooldown 多一个「半开试探」 |
| `failover_switch.rs` | Failover | P1→P2→… 顺序接力 |
| `providers/{claude,codex,gemini}.rs` + `transform_*.rs` | Adapter + Translator | 每种 wire 一个文件 |
| `sse.rs` / `response_processor.rs` | Translate(SSE) | 流式改写 |
| `error_mapper.rs` | 错误分类 | 映射成统一错误码 |
| `database/dao/usage_rollup.rs` | 记账 | 按时间桶预聚合 |

**它做得比我们细的三处：**

- **熔断器三态**：`Open` 到期后进入 `HalfOpen`，放一个请求去试探，成功才 `Closed`。我们现在的「冷却到期直接恢复」会让一个还没好的 provider 立刻再吃一次失败。这个改进成本极低，收益明显。
- **`provider_supports_failover` 显式排除**：Codex 官方账号卡带着原生 `Authorization`，**绝不能**参与跨账号接力，否则会串号。这是 Tier C 的真实地雷，我们已经写进 §3.8.2，但它是「用代码排除」而不是「靠文档记得」。
- **`session.rs` / `affinity`**：同一会话粘在同一账号上，保住厂商的 prompt cache。magpie 也有（`affinity.go`）。**我们 §3.6 里没有这一条，是个缺口**——coding agent 的场景下，每次换账号都等于丢掉整段 prompt cache，成本和延迟都会炸。

---

## 4. 候选方案对比

| | A. 维持 Electron | B. Electron + 外置网关 | C. **Tauri 2 + Rust** | D. Wails v3 + Go | E. 纯 headless |
|---|---|---|---|---|---|
| 安装包（mac） | 85–100 MB | 105–125 MB | **15–35 MB** | **20–28 MB** | 15–25 MB |
| GUI 空载内存 | 200–300 MB | 200–300 MB | 90–140 MB | 80–130 MB | — |
| 常驻网关内存 | 55–75 MB（子进程） | 55–75 MB | **15–25 MB** | 20–35 MB | 15–25 MB |
| 关窗后继续服务 | 需托盘常驻 | ✅ | ✅（双形态） | ✅（双形态） | ✅ |
| 前端复用现有 10 页 | 100% | 100% | **95%**（换 IPC 桥） | 95% | 95% |
| 网关层重写量 | 0 | 0 | ~M1 的 1:1 重写 | 同上（Go 更省事） | 同上 |
| 自动更新 / 签名公证 | 成熟 | 成熟 | **官方插件 + tauri-action** | 需自建（magpie 自写数千行） | 不适用 |
| 系统 WebView 风险 | 无 | 无 | WebKit/WebView2 差异 | 同 | 无 |
| 开发语言门槛 | TS | TS | **Rust** | Go | Go/Rust |
| 参考实现 | — | — | **cc-switch（生产验证）** | **magpie（生产验证，但 v3 仍 beta）** | magpie `serve` |

### 为什么不是 B（Electron + 外置网关）

包体最大（Electron 90 MB + 网关 25 MB），要维护 TS 和 Rust/Go 两套运行时、两套依赖、两套加密库、两套 SQLite 绑定。**用最重的方案解决「太重」的问题，不成立。**

### 为什么不是 D（Wails v3 + Go）

Go 的开发效率确实比 Rust 高，magpie 也是极佳的样板。但决定性因素是**发布链路**：
- Wails v3 目前是 `v3.0.0-beta.24`，API 仍在变；
- 官方没有 Tauri 那种 `updater` + `tauri-action` + 公证模板的成套方案，magpie 为此自写了一个 `internal/update` 包（含差量、镜像、签名校验、回退），这是几千行的纯基础设施；
- 我们的商业模式要求「目录与规则发布即到」，更新链路的可靠性是产品能力，不是附属品。

把 beta 框架 + 自建更新器作为长期底座，风险高于 Rust 的学习成本。

### 为什么不是 E（纯 headless）

失去「桌面应用」的形态：托盘、开机自启的引导、截图/图标的品牌存在感、以及首次配置的引导体验。用户明确要求是桌面端（"应该要桌面端运行啊，不是网页端运行"）。

### 为什么是 C（Tauri 2 + Rust）

1. **包体与内存达标**：15–35 MB / 常驻 20 MB，和 magpie、cc-switch 同一量级。
2. **前端几乎零迁移**：现有 React + Vite + CSS 是纯静态产物（1.8 MB），Tauri 直接加载同一份 `dist/`。要改的只有 `bridge.ts` 一个文件——把 `window.freeport.*` 换成 `invoke()`。
3. **网关层是 1:1 重写，不是重新设计**：IR、路由、接力、账本这些设计已经定稿（§3.3–§3.6），Rust 只是换一种语言表达。M1 的 Node 实现一共约 1500 行，Rust 重写量可控。
4. **发布链路现成**：`tauri-plugin-updater`（签名 manifest + 差量）、`single-instance`、`deep-link`、`window-state`、`autostart`、`log` 全部官方维护。
5. **性能确定性**：SSE 转发是长连接 + 高并发，Rust 的 `hyper`/`axum` 在背压和内存上比 Node 更可预测；对「45s 墙钟预算 + 首字节撤销对冲」这种精细控制更友好。
6. **有生产验证的同类**：cc-switch 4.0.15 已经在 4.7 万次 macOS 下载中跑着同样的活。

---

## 5. 决策

### D1. 采用 Tauri 2 + Rust 作为运行时，废弃 Electron

### D2. 网关是 **lib crate**，不是 bin；产出**单二进制、双形态**

```
crates/
  freeport-core/      # 纯逻辑：IR、路由、账本、错误分类（无 IO，可单测）
  freeport-gateway/   # lib：HTTP server + 中间件链 + adapter（依赖 core）
  freeport-inject/    # lib：Agent 注入器（一文件一适配器）+ 备份/回滚
  freeport-catalog/   # lib：目录同步 + 签名校验 + patch
  freeport-probe/     # lib：探针 + 巡检调度
  freeport-cli/       # bin：CLI/TUI 入口（可选，M4 后）
src-tauri/            # bin：GUI 宿主，链接上面全部
```

两种运行形态，同一份代码：

| 形态 | 命令 | 谁托管 | 用途 |
|---|---|---|---|
| GUI | `freeport`（默认） | 用户点击 / 开机自启 | 进程内起网关，关窗时可选择「移交 daemon」 |
| Daemon | `freeport --headless` | launchd / systemd user unit / 计划任务 | 无窗口常驻，供开机即用、SSH、服务器 |

**移交语义**（这是 Electron 做不到、而产品必须有的）：
- 关窗时若「保持网关运行」= 开，GUI 进程 spawn `--headless` 子进程 → 等待 `/health` 就绪 → GUI 退出。端口用 `SO_REUSEPORT` 或先停后起（<300 ms 空窗，见 §3.6 的 45s 预算，agent 侧会重试）。
- 用户再次点图标 → 检测到已有 daemon → GUI 以「控制台模式」启动，所有写操作经 `POST /admin/*` 转给 daemon，不重复开监听。

### D3. 保留并强化「错误作用域矩阵」，但改成 cc-switch 的**三态熔断 + rest 自解释**

三态：`Closed → (连续失败达阈值/错误率超阈值) → Open → (timeout) → HalfOpen → (试探成功 N 次) → Closed`。
作用域不变（key / model / provider 三级），但 `Open` 的 `reason` 与 `resetAt` 必须落库并可渲染成人话——UI 上直接显示「预计 14:32 恢复 · 额度耗尽」，而不是一个红点。

### D4. 采纳 magpie 的两条路由规则，与现有三轴打分**分层**而非混合

```
第 1 层  Gate     ：三态熔断 + 四窗口账本 + 能力门禁（硬过滤，不算分）
第 2 层  Order    ：SWRR 平滑加权（保证 key/组的长期比例与短期均匀）
第 3 层  Pick     ：同权重档内，按「最早刷新优先 → 剩余最多 → Thompson 探索」
```

原来 `score = base × headroom × rateLimit × userOverride` 的连乘**取消**。原因：连乘不可解释，线上出问题时无法回答「为什么这次选了它」；而分层后每一层都能独立观测、独立关闭。Thompson 采样降级为「同档内 8% 探索」，不再承担主排序。

**新增（缺口）：`affinity` 会话粘性。** 同一 `sessionId` 在其有效期内优先落在上一跳成功的 provider/key 上，除非它已 rest。这是 prompt cache 的生死线。

### D5. 注入器放弃统一 DSL，改为「一文件一适配器 + 共享工具库」

12 个首批 Agent = 12 个文件，每个文件只描述：配置文件路径、格式（JSON/TOML/YAML/JSONC）、要写的键路径、托管块标记、备份策略。共享库提供原子写、托管块合并、幂等校验、回滚。**不设计配置 DSL，不为第 13 个 Agent 预先抽象。**

### D6. 目录改为「models.dev + 厂商 `/models` 为底，自建只做免费额度增量」

原方案要自建全量模型目录 + 五源发现 + 签名 feed。改为：

| 数据 | 来源 | 我们维护 |
|---|---|---|
| 模型清单 / 上下文窗口 / 价格 | models.dev + 厂商 `/models` | 否（跟随） |
| **免费额度（rpm/rpd/tpm/tpd）、是否需要卡、地区限制** | 官方文档 + 探针实测 | **是（Pro 的数据资产）** |
| 免费模型的可用性状态 | 巡检结果 | 是 |

签名 feed 只承载「增量 + 覆盖」，体积从数 MB 降到数十 KB，更新频率和可靠性都更好。

### D7. 前端保持不变，只换 IPC 桥

现有 10 页、设计 token、i18n 结构全部保留。改动清单：
- `src/lib/bridge.ts`：`window.freeport.*` → `invoke('gw_status')` 等；
- 删除 `electron/preload.cjs` 依赖与 `window.freeport` 全局类型；
- `vite.config.ts` 增加 Tauri 的 `clearScreen: false` + 固定端口 1420。

---

## 6. 迁移成本与窗口

### 6.1 为什么是现在

| 时点 | 网关代码量 | 注入器 | 迁移成本 |
|---|---|---|---|
| **现在（M1 末）** | ~1.5k 行 CJS | 0（仅有预设弹窗） | **~12 人日** |
| M2 末（目录 + 密钥 + 探针） | ~5k 行 | 0 | ~25 人日 |
| M3 末（注入 + Auth Broker） | ~9k 行 | 12 | ~40 人日 |
| M4 后 | ~15k 行 | 12 + 扩展宿主 | **不建议迁移** |

**当前是唯一的低成本窗口。** 越往后，迁移从「换语言重写」变成「重写 + 重新验证 60 个注入场景」。

### 6.2 拆解

| 工作 | 人日 | 可并行 |
|---|---|---|
| M0 spike：Tauri 起窗 + 内嵌网关 + 一条真实 SSE 转发 | 2 | — |
| `freeport-core`：IR + 流事件（1:1 移植 §3.3） | 3 | ✅ |
| `freeport-gateway`：axum server + 中间件链 + 2 个 adapter（mock + 1 真实） | 4 | ✅ |
| 壳层：托盘 / 单实例 / 窗位记忆 / 自启 / 关窗移交 | 2 | ✅ |
| 前端桥替换 + 10 页回归 | 1 | — |
| 构建：dmg/msi、签名、公证、updater manifest、CI 矩阵 | 3 | — |
| 回归：M1 已有的端到端用例（health/models/非流式/SSE/两跳接力） | 1 | — |
| **合计** | **16** | 并行后约 10–12 |

相比继续在 Electron 上做 M2–M4，**净增量约 +1.5 周**。换来的是包体 -70%、常驻内存 -70%、以及一个不再和 UI 绑死的网关。

### 6.3 回滚预案

- 分支隔离：`feat/tauri-runtime`，`main` 上 Electron 版本保持可发布状态直到 Tauri 版 M1 回归全绿。
- 契约不变：网关的 HTTP 契约（§3.7）与 DB schema（§3.10）在迁移中**一个字都不改**，因此两版可共用同一份配置与数据库，回滚不会丢用户数据。
- 判定点：spike 结束（第 2 天）若出现「WKWebView 无法承载当前 UI」或「Rust SSE 转发无法满足首字节预算」，则停止迁移，转方案 A + 优化（见 §7）。

---

## 7. 若最终不迁移，Electron 的最低补救

不迁移也必须做，否则「常驻」这件事不成立：

1. **网关独立成 Node 子进程**（`utilityProcess` 或 `child_process.fork`），GUI 只做控制台；关窗时 GUI 退出、子进程留下并由 launchd/systemd 托管。
2. **`app.commandLine.appendSwitch('js-flags', '--max-old-space-size=96')`** 限制渲染进程堆。
3. **懒加载页面** + 关闭时不销毁窗口而是 `webContents.destroy()`，需要时重建。
4. `backgroundThrottling: false` 只给必要页面。
5. 更新改为差量（blockmap），首装引导文案明确「安装包较大」。

即便全做，包体仍是 85–100 MB、常驻仍是 55–75 MB。**这是缓解，不是解决。**

---

## 8. 风险

| 风险 | 概率 | 影响 | 对策 |
|---|---|---|---|
| Rust 开发速度低于预期 | 中 | 进度 +2–3 周 | core 层保持纯逻辑、无 IO，全部单测；adapter 按 cc-switch 结构照抄 |
| WKWebView / WebView2 渲染差异 | 中 | UI 返工 | 现有 UI 只用 flex/grid + CSS 变量，无 Chromium 私有特性；CI 加 Safari 截图对比 |
| Windows 缺 WebView2 Runtime | 中 | 装完打不开 | Win11 自带；Win10 用 bootstrapper 检测并静默安装（体积 +2 MB，非 fixed-version） |
| 编译时间长拖慢迭代 | 高 | 开发体验 | `[profile.dev] opt-level=0` + `codegen-units=256`；网关逻辑放 core，改 UI 不重编 Rust |
| 迁移期间功能停摆 | 中 | 士气/节奏 | 分支并行，`main` 始终可发布；先迁 M1 已验收的能力，再开 M2 |
| 团队不熟 Rust | 中 | 质量 | 先做 2 天 spike 再决定；core 层不写 unsafe；用 `thiserror` 统一错误 |

---

## 9. 对现有文档的修订

| 章节 | 修订 |
|---|---|
| 标题 | v1.1 → **v1.2** |
| 决策记录 | 追加「2026-10-19 运行时复议」 |
| §3.1 进程模型 | 重画为「单二进制双形态」；删除「Data Plane 与 UI 同进程」的原决策 |
| §3.2 分层职责 | Shell 拆为 `GUI Host` / `Daemon Host`；Data Plane 标注为 lib |
| §3.6 | 三轴连乘改为「Gate → Order(SWRR) → Pick」三层；新增 affinity；错误矩阵改为三态熔断 |
| §3.8 | 注入器改为「一文件一适配器」，取消 DSL |
| §3.4 / §3.5.2 | 目录改为 models.dev 为底 + 免费额度增量 |
| §6 | 技术选型整节重写（Rust 栈），旧 Electron 方案移入「已废弃决策」并保留理由 |
| §6.1 | 发布架构改为 Tauri bundler + `tauri-action` + `tauri-plugin-updater` |
| §8 里程碑 | M2 前置 M1.5「运行时迁移」 |

---

## 10. 拍板结论

1. **接受 Tauri 2 + Rust**：M1.5 已完成，Electron 代码已从仓库移除，`app/electron/` 不再存在。
2. **接受「关窗移交 daemon」**：窗口 `CloseRequested` 只隐藏不退出，网关继续服务；托盘菜单才是真退出。
3. **接受路由三层可解释**（D4）：已写入 §3.6。

### 落地记录（M1.5）

| 项 | 结果 |
|---|---|
| Cargo workspace | `crates/freeport-core`（模型/错误/冷却/路由打分/DTO/预设）+ `crates/freeport-gateway`（store/providers/axum server）+ `src-tauri`（宿主） |
| 宿主双形态 | 默认 GUI 内嵌网关；`--headless` / `serve` 只跑网关 |
| 前端 | 页面与 store 零改动，只换 `src/lib/ipc.ts` 门面（Tauri `invoke`） |
| 命令面 | `app_info`、`win_*`、`gw_*` 共 20 个，与旧 preload 一一对应 |
| 托盘 | 打开 / 退出两项；关窗隐藏、左键切换显隐 |
| 已删除 | `electron/`（4 文件）、`electron-cache/`、Electron 相关 npm 脚本与依赖、`unzip-electron.py` |


---

## 附：一句话摘要

> Electron 的问题不是慢，是**它把一台常驻服务器做成了一个大号 GUI**。
> magpie 用 22 MB 证明了「网关 + 60 个注入 + GUI」可以只有一个二进制；
> cc-switch 用 33 MB 证明了 Tauri 能扛住三协议代理与 SSE 流式。
> 现在网关只有 1.5k 行，是重写成本最低的时刻——再晚三个月，就不是换壳，是重做。
