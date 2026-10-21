# Pro：卖时效与情报，不卖权限

> 状态：数据面已落地（`crates/freeport-catalogd` + 客户端按档位取快照 + 按 `feat` 取字段包）。
> 关联：`docs/ARCHITECTURE.md` §7.1–7.3、`crates/freeport-license`、`crates/freeport-catalog`。

## 1. 三条原则（写进代码，不靠自觉）

| 原则 | 落地方式 |
|---|---|
| 免费版是完整产品 | 网关、路由、注入、MCP、协议面**没有任何** `if !is_pro()`；Pro 只改「服务端给什么数据」 |
| 卖时效，不卖解锁 | 唯一差异是快照的**新鲜度与深度**：`live` vs `monthly` |
| 失效只降级，不罢工 | 许可证过期 / 目录服务挂 / 断网 → 继续用最近一次快照，界面显示「N 天未更新」 |

判定只看 **Bearer**，不看 `x-freeport-tier`：后者是客户端自述，可以随便写。
头部说 pro 但 token 验不过 → 仍然发免费包。

## 2. Pro 能控制什么

### 2.1 服务端可硬控（真闸门）

| 能力 | Free | Pro | 执行点 |
|---|---|---|---|
| 目录快照 | `monthly.json`，随版本发布 | `live.json`，随发布即时 | `catalogd` 按 token 选文件；`/v1/snapshot/live.json` 本身也验 token |
| 增量补丁链 | 不下发 | `manifest.patches` 只给「下一环」 | 免费包是另一份文档，实时补丁对不上摘要 |
| 轮询节奏（礼貌值） | ≥30 分钟 | ≥1 分钟 | `FeedState::due_for`，可被用户改配置绕过，**不承担权限职责** |
| 字段级情报（`quota.intel`） | 不下发 | 按令牌 `feat` 逐项下发 | `manifest.feat[]` + `/v1/feat/{name}.json`，双重验令牌 |
| 云端探针 / 多机同步 / 日志保留 | 无 | 独立服务端能力 | 各自端点按 `feat` 放行 |
| 评测分数（`model.scores`） | 不下发 | 传输已就绪，数据管线待做 | 同上；客户端不认识时记进 `featsSkipped` |

### 2.2 只做展示（客户端不强制）

- `seats`：许可证里带，服务端做统计与人工核验；客户端不拦。
  在客户端拦席位等于「付费用户换个网卡就用不了」，误伤远大于收益。

### 2.3 永不控制

转发链路、重试/并发/超时、注入器、MCP、协议面、本地日志、本地路由策略。
这些一旦上锁，第一周就会把用户推去盗版或替代品。

## 3. 实现

```
客户端 feed_poll
  ├─ 身份：license::identity() → tier + token（宽限期也算 pro）
  ├─ 节奏：FeedState::due_for(now, tier)
  └─ 请求：GET /v1/latest?since=<seq>
             x-freeport-tier: free|pro      （提示，不参与鉴权）
             Authorization: Bearer <jwt>    （仅 pro 且 https/回环）
        ↓
catalogd
  ├─ verify_claims(pubkey, token, now) → tier=pro ? live : monthly
  ├─ since >= seq → 304
  ├─ manifest = {seq, version, sha256, tier, patches[下一环], feat[按令牌 feat 挑]} + Ed25519 签名
  ├─ GET /v1/snapshot/{live|monthly}.json、GET /v1/patch/{from}-{to}.json
  └─ GET /v1/feat/{name}.json（要令牌，且 feat 点名这一项）
        ↓
客户端
  ├─ Manifest::verify(内置公钥)
  ├─ 增量可用则打补丁，否则回退全量（任何失败都回退）
  ├─ 字段包：先查「认不认识」，再验 sha256，再合并；跳过要记名字
  └─ servedTier / servedFeats 只写进状态供展示，不参与任何本地分支
```

### 3.1 字段包：为什么不切快照

快照的字节被 `sha256` 与增量补丁链钉死了。每多一种切法（free / pro / pro+intel …）
就要多一套签名、多一条补丁链、多一次「给谁哪份」的判断，而且切完的文档要重新
序列化——里面有浮点价格，JCS 直接拒绝。

所以字段包是**旁路文档**：

```
manifest.feat = [ {name:"quota.intel", url:"feat/quota.intel.json", sha256:"…"} ]
```

- 名字与摘要都进签名 → 中间人既不能塞包，也不能换地址（有测试：多塞一个包必验不过）
- 快照一个字节不动 → 老客户端照旧工作，补丁链照旧有效
- 客户端只认签名里那份清单，且**必须自己认识这个包名**才去拉（「先信后懂」等于把解析器交给发布方）
- 不认识的包不丢：记进 `featsSkipped`，界面照实说「这次少了什么」

`quota.intel` 的载荷是 `{"providers":{"groq":{"rpm":30,"rpd":1000}}}`，只补既有厂商，
包里点到目录里没有的厂商直接跳过——情报没有能力凭空造出目录本体。

**刻意不做**：不把情报自动写进账本的限额。目录数字会变，写进账本就意味着
「一张过期的情报能拦住一次本来能成的请求」，违背「宁可只记账不误拦」。
情报只做展示（发现页的 RPM/RPD 就是它），要不要写进账本由用户决定。

### 3.2 快照目录格式

```
FREEPORT_CATALOG_DIR/
├── meta.json      {"seq":12,"version":"2026.10.21","publishedAt":"...","channel":"stable"}
├── live.json      Snapshot（Pro 取这份）
├── monthly.json   Snapshot（免费取这份；内容是 live 的 30 天前切片）
├── patches/       可选，文件名 2-3.json 表示 seq 2 → 3
└── feats/         可选，文件名即能力名：quota.intel.json、model.scores.json
```

`Floor::load` 缺 live/monthly 任一份都**直接报错退出**：一个只配了 live 的服务上线后，
所有免费用户会拿到 404，日志里只是一串 404，很难联想到配置。字段包相反——
没有 `feats/` 就是「这次没有情报可发」，不算配置错误。

### 3.3 启动

```bash
export FREEPORT_CATALOG_DIR=./catalog
export FREEPORT_CATALOG_SEED=<hex 32B>          # Ed25519 私钥种子，必填
export FREEPORT_CATALOG_ADDR=127.0.0.1:8787     # 默认回环
cargo run -p freeport-catalogd
```

客户端把 `feed.url` 指向 `http://127.0.0.1:8787/v1/latest`（回环才允许带 Bearer），
`feed.pubkey` 填对应公钥 hex。

发布侧用 `examples/sign_feed.rs`：

```bash
cargo run -p freeport-catalog --example sign_feed -- \
  --key $FEED_KEY --snapshot snapshots/2026.10.21.json \
  --version 2026.10.21 --seq 12 --tier pro \
  --feats catalog-feats/ --out feed/
```

`--feats` 目录下每个 `<能力名>.json` 会被复制到 `feed/feat/` 并把摘要写进 manifest。
**manifest 里的快照/补丁/字段包地址都是相对 manifest 解析的**，所以服务端同时注册了
`/v1/{snapshot,patch,feat}/…` 与根路径两组——只注册根路径时症状是「manifest 拿得到、快照 404」。

## 4. 降级行为（必须逐条可验）

| 场景 | 行为 |
|---|---|
| 无许可证 | 拉 `monthly`，界面标「公共目录」，`manifest.feat` 为空 |
| 许可证过期 8 天 | 服务端判 free → `monthly`；客户端 `licensedTier=pro` 仍然显示「已到期」 |
| 宽限期内 3 天 | 仍给 `live`（付费用户不该卡在到期那一刻） |
| 许可证到期后 | 服务端不再下发字段包；**已到手的留在本地目录里不删**（客户端不判档，也不做反向删除） |
| 令牌 `feat` 没写某一项 | 该项不列、也拿不到（`/v1/feat/…` 直接 403） |
| manifest 列了客户端不认识的包 | 不下载、记进 `featsSkipped`，界面显示「本版本不支持」 |
| 字段包摘要不符 | 整份丢弃并记原因；快照照常应用（一份情报坏掉不该拖垮整次同步） |
| 目录服务 500 / 断网 | 保留旧目录，`lastError` 写原因，**路由不受影响** |
| 补丁链断环 | 自动回退全量，日志写明原因 |
| 快照与 manifest 不同源 | 不带 Bearer（凭据不送第三方域名） |

## 5. 测试闸（已落地）

- `freeport-catalogd`：token 缺失/伪造/过期/宽限期四档判定；`tier` 进签名（改成 free 必验不过）；
  `feat` 清单进签名（多塞一个包、换一个地址都验不过）；`feats_for` 只按令牌名单挑、忽略大小写。
- `freeport-catalog`：`quota.intel` 不造厂商、只覆盖明说字段、坏形状报错。
- `freeport-gateway/tests/feed_tier_e2e.rs`：免费档永不带 Bearer；Pro 带 Bearer 且 `servedTier=pro`；跨源不带 Bearer。
- `freeport-gateway/tests/feat_e2e.rs`：**真实 catalogd 对打真实网关**——免费档拿不到包；
  无令牌时 `/v1/snapshot/live.json` 与 `/v1/feat/*` 均 403；激活后只应用令牌里写了且客户端认识的包，
  不认识的进 `featsSkipped`。
- 代码级红线：网关里搜不到 `is_pro()` 的分支（只有 `license.rs` 构造状态时用）。

## 6. 还没做（按优先级）

| 项 | 为什么先不做 |
|---|---|
| `model.scores` 的真实数据 | 传输层已就绪（发布 `feats/model.scores.json` 即生效），但要有实测样本才有分；客户端现在会如实记「本版本不支持」 |
| 席位计数与自助换机 | 需要订单库；当前靠 `mid` 绑机器 + 人工 |
| `sync.cloud` | 复用迁移码的 bundle 格式，代价是账号体系与存储，属于独立项目 |
| `log.90d` | 需要上传通道与保留策略，先确认用户愿意上传日志 |
| 自助计费门户 | 先验证续费率，再决定接不接 Stripe |

## 7. 定价（不变）

Free ¥0；Pro ¥29/月 或 ¥199/年；Team 按席位（只同步策略，不同步 key）。
明确不做：卖 key、转售额度、按 token 计费、代注册账号。
