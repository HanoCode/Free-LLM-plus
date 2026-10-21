//! 签名目录 feed：把「发布即到」做成一条可验证的链路。
//!
//! 一段可信的更新是这样的：
//!
//! ```text
//! CI 生成快照 → 用 Ed25519 私钥签 JCS(manifest) → 客户端取 manifest
//!   → 验签 → 校验 sha256 → 过安全阀 → 合并进目录 → 新模型以 candidate 灰度
//! ```
//!
//! 四道闸门，任何一道不过就**只报错、不改目录**：
//!
//! 1. **验签**：签名覆盖 `version/channel/seq/publishedAt/sha256/url`，
//!    连地址一起签，防止把客户端指到别人的服务器上。
//! 2. **sha256**：快照原文的摘要，防传输损坏与中间人换包。
//! 3. **seq 单调**：小于本地的版本一律丢弃（回滚攻击 / 镜像不同步）。
//! 4. **变更幅度**：单次删除超 15%、新增超 30% 就停下来等人工确认。
//!
//! 合并语义（谁说话算数）：
//!
//! - 厂商级「免费额度情报」以 feed 为准——这是 Pro 的资产，也是 models.dev 没有的
//! - 模型级事实只在 feed 明确给出时覆盖，缺省字段保留本地值（避免一次 feed 把上下文窗口清空）
//! - feed 新加进来的模型标 `candidate`，路由只给 0.3 倍权重，跑满
//!   [`freeport_core::CANARY_OKS`] 次成功才转正
//! - 墓碑（下架）立刻生效：路由在 Gate 层直接拒绝，不等下一次目录同步

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, VerifyingKey};
use freeport_core::model::{FeedEvent, CANARY_OKS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::jcs;
use crate::types::{model_is_free, Catalog, CatalogModel, CatalogProvider, Quota};

/// 发布前把公钥填进来。留空表示「没有官方 feed」，界面会显示未配置。
///
/// 私钥只在 CI，客户端**永远**只有公钥；签名代码也不进发布包（只在
/// `examples/sign_feed.rs` 里，cargo 不会把它编进二进制）。
pub const BUILTIN_PUBKEY: &str = "";

pub const POLICY_AUTO: &str = "auto";
pub const POLICY_NOTIFY: &str = "notify";
pub const POLICY_PINNED: &str = "pinned";

pub const CHANNEL_STABLE: &str = "stable";
pub const CHANNEL_BETA: &str = "beta";

/// 单次允许删除的比例上限
pub const MAX_DELETE_RATIO: f64 = 0.15;
/// 单次允许新增的比例上限
pub const MAX_ADD_RATIO: f64 = 0.30;
/// 目录小于这个规模时，百分比没有统计意义（加 1 个就是 +8%），改用绝对条数
const MIN_FOR_RATIO: usize = 50;
const ABS_DELETE: usize = 8;
const ABS_ADD: usize = 40;

/* ── manifest ─────────────────────────────────── */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub channel: String,
    pub seq: u64,
    #[serde(default)]
    pub published_at: String,
    /// 快照原文的 sha256（小写 hex）
    pub sha256: String,
    #[serde(default)]
    pub sig: String,
    /// 快照地址，可相对 manifest 所在目录
    pub url: String,
    /// 服务端标注这份快照的档位（`free` / `pro`）。**只用于展示**：
    /// 客户端不拿它做功能判断——给什么数据由服务端决定，验过签名就照单全收。
    /// 但它参与签名：否则中间人可以把免费包标成 Pro 包，让界面说谎。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// 按 `feat` 下发的字段包清单。**快照本身不动**，所以 sha256 与补丁链
    /// 原样有效；字段跟着名字与摘要单独下发。
    ///
    /// 名字与摘要都进签名：中间人既不能把包塞进来，也不能把地址换掉。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feat: Vec<FeatRef>,
    /// 增量补丁链。客户端本地 `seq` 落在其中某一环的 `fromSeq` 上时，可以只下补丁；
    /// 否则回退全量快照。**空表示发布端没做增量**，老客户端照旧。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchRef>,
}

/// 一份字段包：某几个字段的值，按能力名下发。
///
/// 为什么不做成「快照的第三种切法」（free/pro 两份之外再切）：
/// 快照摘要与增量补丁都钉在字节上，切一刀就要重新序列化、重新签名，
/// 补丁链立刻作废。字段包是**旁路文档**，主链路一个字节都不用动。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeatRef {
    /// 与许可证 `feat` 同名：`quota.intel` 之类
    pub name: String,
    /// 可相对 manifest 所在目录
    pub url: String,
    /// 包文件原文的 sha256
    pub sha256: String,
}

/// manifest 里的一环补丁。地址和摘要都进签名，否则中间人可以换补丁内容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchRef {
    pub from_seq: u64,
    pub to_seq: u64,
    /// 可相对 manifest 所在目录
    pub url: String,
    /// 补丁文件原文的 sha256
    pub sha256: String,
}

impl Manifest {
    /// 被签名的对象。`sig` 自己当然不在里面，`notes` 是可选的展示文案，
    /// 不进签名免得改个错别字就要重签。
    ///
    /// `tier` 只在存在时进载荷：老 manifest 不带这个字段，签名照样能对上。
    pub fn signing_payload(&self) -> Value {
        let mut v = json!({
            "version": self.version,
            "channel": self.channel,
            "seq": self.seq,
            "publishedAt": self.published_at,
            "sha256": self.sha256,
            "url": self.url,
        });
        if let Some(t) = self.tier.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            v["tier"] = json!(t);
        }
        // 补丁链也进签名。不然中间人能把 url 换成自己的服务器，
        // 交一份「只删模型、不加模型」的补丁——摘要校验挡不住，因为摘要也是他给的。
        if !self.patches.is_empty() {
            v["patches"] = json!(self.patches);
        }
        // 字段包同理。少了这一条，中间人可以在免费包的 manifest 里
        // 塞一个 `quota.intel`——客户端只会验摘要，摘要正是他给的。
        if !self.feat.is_empty() {
            v["feat"] = json!(self.feat);
        }
        v
    }

    pub fn verify(&self, pubkey_hex: &str) -> Result<(), Reject> {
        if self.sig.trim().is_empty() {
            return Err(Reject::invalid("manifest 没有签名，拒绝应用"));
        }
        let vk = parse_pubkey(pubkey_hex)?;
        let payload = jcs::canonical(&self.signing_payload())
            .map_err(|e| Reject::invalid(format!("签名载荷无法规范化：{e}")))?;
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, self.sig.trim())
            .map_err(|_| Reject::invalid("签名不是合法的 base64"))?;
        let sig = Signature::from_slice(&raw).map_err(|_| Reject::invalid("签名长度不对"))?;
        vk.verify_strict(payload.as_bytes(), &sig)
            .map_err(|_| Reject::invalid("feed 签名校验失败：内容可能被篡改"))
    }
}

fn parse_pubkey(hex_str: &str) -> Result<VerifyingKey, Reject> {
    let s = hex_str.trim();
    if s.is_empty() {
        return Err(Reject::invalid(
            "没有可用的 feed 公钥：内置公钥为空，且未配置自建公钥",
        ));
    }
    let bytes = hex::decode(s).map_err(|_| Reject::invalid("公钥不是合法的 hex"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Reject::invalid("公钥长度必须是 32 字节"))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| Reject::invalid("公钥不是合法的 Ed25519 点"))
}

/* ── 快照 ─────────────────────────────────────── */

/// 快照里的厂商。字段全 `Option`：**没给**和**给了 false** 必须区分开，
/// 否则一份只写模型的增量快照会把基线的 `freeTier` 抹成 false。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedProvider {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_tier: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
    #[serde(default)]
    pub models: Vec<FeedModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeedModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_in: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_out: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    /// 在快照里直接标注下架，效果等同顶层 `tombstones`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub providers: Vec<FeedProvider>,
    #[serde(default)]
    pub events: Vec<FeedEvent>,
    /// `provider:model`
    #[serde(default)]
    pub tombstones: Vec<String>,
}

/* ── 应用结果 ─────────────────────────────────── */

#[derive(Debug, Clone)]
pub struct Applied {
    pub catalog: Catalog,
    pub events: Vec<FeedEvent>,
    pub tombstones: BTreeSet<String>,
    pub added: usize,
    /// 本次新增的 `provider:model`。网关要把它们塞进厂商模型表，
    /// 否则「目录里有了」和「用户能看到、路由能用」还是两件事。
    pub added_keys: Vec<String>,
    pub deleted: usize,
    pub seq: u64,
    pub version: String,
    /// 快照里声明「仍然有效」的模型。用来把本地墓碑撤销——
    /// 模型下架后又回来了，feed 明确列出来就是撤销信号。
    pub active: Vec<String>,
    /// 服务端标注的档位，仅用于界面展示「这份目录是 Pro 档下发的」。
    pub tier: Option<String>,
    /// 本次真正应用的字段包名。**这是「服务端给了什么」的实录**，
    /// 不是本机判档的结果——客户端从来不按自己的档位决定收不收。
    pub feats: Vec<String>,
    /// 服务端给了但没应用的（不认识 / 摘要不符 / 形状不对），带原因。
    /// 静默丢弃会让「Pro 面板为什么是空的」变成一个查不出来的现象。
    pub feats_skipped: Vec<String>,
    /// 本次实际生效的完整快照（增量拼出来的也算完整）。
    /// 调用方或写入磁盘当下一轮的补丁基线，或什么都不做。
    pub snapshot: Snapshot,
}

/// 没应用成功。`halted` 区分「被安全阀拦下」与「数据不可信」，
/// 界面上一类叫「需要人工确认」，另一类叫「已丢弃」。
#[derive(Debug, Clone)]
pub struct Reject {
    pub halted: bool,
    pub message: String,
}

impl Reject {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self { halted: false, message: msg.into() }
    }
    pub fn halted(msg: impl Into<String>) -> Self {
        Self { halted: true, message: msg.into() }
    }
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/* ── 增量补丁（RFC 6902 子集） ─────────────────
 *
 * 全量快照现在 500KB+，一天同步几次还行，等目录长到几千个模型就是纯浪费。
 * 增量只带「变了的那几段」，其余沿用本地基线。
 *
 * 三条约束，缺一不可：
 * 1. **补丁地址与摘要进 manifest 签名**——否则中间人换 url、换内容都验不出来。
 * 2. **每一环都要接得上**：`fromSeq` 必须等于当前 seq，不能跳环，不能回环。
 * 3. **每环应用后校验摘要**：对「反序列化成 Snapshot 再序列化」的字节取 sha256。
 *    这里不能用 JCS——快照里有 `costIn/costOut` 浮点，JCS 会直接拒绝。
 *
 * 只支持 add / remove / replace / test。move / copy 不做：它们能从一个路径
 * 读值写到另一个路径，等于给了补丁「读取整份文档任意位置」的能力，收益却很小。
 */

/// 单个补丁文件能带的最大操作数。正常一次发布是几十条，给到 4096 已经很宽松。
const MAX_PATCH_OPS: usize = 4096;
/// 补丁文件大小上限。超过它一定是出了问题，不是「目录太大」。
const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
/// 全量快照上限。现在 500KB 出头，给到 64MB 是防「配置被指到一台乱发东西的
/// 服务器」时把内存吃干——摘要校验在下载**之后**才做，不设上限就没机会了。
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
/// 一条链最多几环。断链超过这个数就直接走全量，别为省流量下十几个包。
const MAX_CHAIN_LEN: usize = 24;
/// JSON Pointer 的最大深度，挡掉 `//a//a//a...` 这种构造
const MAX_POINTER_DEPTH: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFile {
    pub from_seq: u64,
    pub to_seq: u64,
    /// RFC 6902 操作数组
    pub ops: Vec<Value>,
    /// 应用之后快照的 sha256（`serde_json::to_vec(&Snapshot)`）
    pub sha256_after: String,
}

/// 快照的稳定摘要。字段顺序由结构体定，所以同一个快照无论从哪条路径得到，
/// 序列化字节都一样——这才是能拿来比的东西。
pub fn snapshot_digest(snap: &Snapshot) -> String {
    match serde_json::to_vec(snap) {
        Ok(b) => hex::encode(Sha256::digest(&b)),
        Err(_) => String::new(),
    }
}

pub fn snapshot_value(snap: &Snapshot) -> Value {
    serde_json::to_value(snap).unwrap_or(Value::Null)
}

/// 从 manifest 的补丁列表里，挑出一条从 `since` 连续走到 `target` 的链。
/// 走不到就返回 `None`（调用方回退全量），不是错误。
pub fn plan_chain(patches: &[PatchRef], since: u64, target: u64) -> Option<Vec<&PatchRef>> {
    if since == 0 || patches.is_empty() || target <= since {
        return None;
    }
    let mut chain = Vec::new();
    let mut cur = since;
    while cur < target {
        if chain.len() >= MAX_CHAIN_LEN {
            return None;
        }
        let next = patches.iter().find(|p| p.from_seq == cur)?;
        // 一环必须往前走，否则 `fromSeq == toSeq` 会让 while 变成死循环
        if next.to_seq <= cur || next.to_seq > target {
            return None;
        }
        chain.push(next);
        cur = next.to_seq;
    }
    (cur == target).then_some(chain)
}

/// 应用一串 RFC 6902 操作。
pub fn apply_patch(doc: &mut Value, ops: &[Value]) -> Result<(), Reject> {
    if ops.len() > MAX_PATCH_OPS {
        return Err(Reject::invalid(format!(
            "补丁带了 {} 个操作，超过上限 {MAX_PATCH_OPS}",
            ops.len()
        )));
    }
    for (i, op) in ops.iter().enumerate() {
        apply_one(doc, op).map_err(|e| Reject::invalid(format!("第 {} 个操作无效：{}", i + 1, e.message)))?;
    }
    Ok(())
}

fn apply_one(doc: &mut Value, op: &Value) -> Result<(), Reject> {
    let kind = op
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Reject::invalid("操作缺少 op 字段"))?;
    let ptr = op
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Reject::invalid("操作缺少 path 字段"))?;
    let path = parse_pointer(ptr)?;

    match kind {
        "add" => {
            let val = op
                .get("value")
                .cloned()
                .ok_or_else(|| Reject::invalid("add 缺少 value"))?;
            if path.is_empty() {
                *doc = val;
                return Ok(());
            }
            let (last, parent) = path.split_last().expect("非空");
            match cursor(doc, parent)? {
                Value::Object(map) => {
                    map.insert(last.clone(), val);
                    Ok(())
                }
                Value::Array(arr) => {
                    let i = if last == "-" {
                        arr.len()
                    } else {
                        index(last, arr.len(), true)?
                    };
                    arr.insert(i, val);
                    Ok(())
                }
                _ => Err(Reject::invalid(format!("{ptr} 的父节点不是对象或数组"))),
            }
        }
        "remove" => {
            if path.is_empty() {
                return Err(Reject::invalid("不能删除整个文档"));
            }
            let (last, parent) = path.split_last().expect("非空");
            match cursor(doc, parent)? {
                Value::Object(map) => {
                    map.remove(last)
                        .map(|_| ())
                        .ok_or_else(|| Reject::invalid(format!("{ptr} 不存在，无法删除")))
                }
                Value::Array(arr) => {
                    let i = index(last, arr.len(), false)?;
                    arr.remove(i);
                    Ok(())
                }
                _ => Err(Reject::invalid(format!("{ptr} 的父节点不是对象或数组"))),
            }
        }
        "replace" => {
            let val = op
                .get("value")
                .cloned()
                .ok_or_else(|| Reject::invalid("replace 缺少 value"))?;
            if path.is_empty() {
                *doc = val;
                return Ok(());
            }
            let (last, parent) = path.split_last().expect("非空");
            match cursor(doc, parent)? {
                Value::Object(map) => {
                    if !map.contains_key(last) {
                        return Err(Reject::invalid(format!("{ptr} 不存在，无法替换")));
                    }
                    map.insert(last.clone(), val);
                    Ok(())
                }
                Value::Array(arr) => {
                    let i = index(last, arr.len(), false)?;
                    arr[i] = val;
                    Ok(())
                }
                _ => Err(Reject::invalid(format!("{ptr} 的父节点不是对象或数组"))),
            }
        }
        "test" => {
            let want = op
                .get("value")
                .ok_or_else(|| Reject::invalid("test 缺少 value"))?;
            let got = cursor(doc, &path)?;
            if got == want {
                Ok(())
            } else {
                Err(Reject::invalid(format!("{ptr} 的前置条件不成立")))
            }
        }
        other => Err(Reject::invalid(format!(
            "不支持的补丁操作 `{other}`（只接受 add/remove/replace/test）"
        ))),
    }
}

/// 沿指针走到底，返回可变引用。中间任何一段缺失都算失败。
fn cursor<'a>(doc: &'a mut Value, path: &[String]) -> Result<&'a mut Value, Reject> {
    let mut cur = doc;
    for seg in path {
        cur = match cur {
            Value::Object(map) => map
                .get_mut(seg.as_str())
                .ok_or_else(|| Reject::invalid(format!("路径段 `{seg}` 不存在")))?,
            Value::Array(arr) => {
                let i = index(seg, arr.len(), false)?;
                arr.get_mut(i)
                    .ok_or_else(|| Reject::invalid(format!("数组下标 {i} 越界")))?
            }
            _ => return Err(Reject::invalid("路径中途遇到非对象/数组")),
        };
    }
    Ok(cur)
}

/// 数组下标。`allow_end` 为 true 时允许等于长度（add 到末尾）。
fn index(seg: &str, len: usize, allow_end: bool) -> Result<usize, Reject> {
    if seg.is_empty() || (seg.len() > 1 && seg.starts_with('0')) {
        return Err(Reject::invalid(format!("`{seg}` 不是合法的数组下标")));
    }
    let i: usize = seg
        .parse()
        .map_err(|_| Reject::invalid(format!("`{seg}` 不是合法的数组下标")))?;
    if i > len || (!allow_end && i == len) {
        return Err(Reject::invalid(format!("数组下标 {i} 越界（长度 {len}）")));
    }
    Ok(i)
}

fn parse_pointer(ptr: &str) -> Result<Vec<String>, Reject> {
    if ptr.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = ptr.strip_prefix('/') else {
        return Err(Reject::invalid(format!("`{ptr}` 不是合法的 JSON Pointer")));
    };
    let segs: Vec<String> = rest
        .split('/')
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect();
    if segs.len() > MAX_POINTER_DEPTH {
        return Err(Reject::invalid("JSON Pointer 层级过深"));
    }
    Ok(segs)
}

/// 应用一环补丁并校验。`doc` 必须是上一环的产物（或本地基线快照）。
pub fn apply_patch_file(doc: &mut Value, pf: &PatchFile) -> Result<(), Reject> {
    let cur = doc.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
    if cur != pf.from_seq {
        return Err(Reject::invalid(format!(
            "补丁基线不匹配：本地 seq {cur}，补丁要求 {}",
            pf.from_seq
        )));
    }
    apply_patch(doc, &pf.ops)?;

    // seq 由补丁元数据强制推进：不指望发布端记得在 ops 里写它，
    // 写漏了就会得到一个「内容新、seq 旧」的快照，下一次同步从头再来。
    if let Value::Object(map) = doc {
        map.insert("seq".into(), Value::from(pf.to_seq));
    }

    let snap: Snapshot = serde_json::from_value(doc.clone())
        .map_err(|e| Reject::invalid(format!("补丁产出不是合法快照：{e}")))?;
    let got = snapshot_digest(&snap);
    if !got.eq_ignore_ascii_case(pf.sha256_after.trim()) {
        return Err(Reject::invalid(format!(
            "补丁应用后摘要不匹配（期望 {}，实际 {}），已丢弃",
            short(&pf.sha256_after),
            short(&got)
        )));
    }
    Ok(())
}

/* ── 补丁生成（发布端用，sign_feed 调它） ───── */

/// 算出 `prev → next` 的 RFC 6902 操作。
///
/// **带自检**：生成完先自己套一遍，套不上就直接退回「整体 replace」。
/// 宁可下一个全量的补丁，也不能让客户端拿到一份拼错的目录——
/// 那种错误不会报错，只会让模型列表悄悄少几条。
pub fn diff(prev: &Value, next: &Value) -> Vec<Value> {
    let mut ops = Vec::new();
    diff_at(prev, next, "", &mut ops);

    let mut probe = prev.clone();
    if apply_patch(&mut probe, &ops).is_ok() && &probe == next {
        return ops;
    }
    vec![json!({ "op": "replace", "path": "", "value": next })]
}

fn diff_at(a: &Value, b: &Value, path: &str, out: &mut Vec<Value>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys() {
                if !y.contains_key(k) {
                    out.push(json!({ "op": "remove", "path": ptr_join(path, k) }));
                }
            }
            for (k, v) in y {
                match x.get(k) {
                    None => out.push(json!({ "op": "add", "path": ptr_join(path, k), "value": v })),
                    Some(old) => diff_at(old, v, &ptr_join(path, k), out),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if identifiable(x) && identifiable(y) => {
            let ykeys: Vec<&str> = y.iter().map(id_of).collect();
            // ① 共有元素按原始下标递归：此时数组还没被改动，下标都还对得上
            for (i, item) in x.iter().enumerate() {
                let k = id_of(item);
                if let Some(j) = ykeys.iter().position(|v| *v == k) {
                    diff_at(item, &y[j], &ptr_join(path, &i.to_string()), out);
                }
            }
            // ② 倒序删：删后面不会挪动前面元素的下标
            for i in (0..x.len()).rev() {
                if !ykeys.contains(&id_of(&x[i])) {
                    out.push(json!({ "op": "remove", "path": ptr_join(path, &i.to_string()) }));
                }
            }
            // ③ 追加新增
            for item in y.iter() {
                let k = id_of(item);
                if !x.iter().any(|v| id_of(v) == k) {
                    out.push(json!({ "op": "add", "path": ptr_join(path, "-"), "value": item }));
                }
            }
        }
        _ => {
            if a != b {
                out.push(json!({ "op": "replace", "path": path, "value": b }));
            }
        }
    }
}

fn ptr_join(base: &str, seg: &str) -> String {
    let seg = seg.replace('~', "~0").replace('/', "~1");
    format!("{base}/{seg}")
}

fn identifiable(items: &[Value]) -> bool {
    !items.is_empty()
        && items
            .iter()
            .all(|v| v.get("id").and_then(|i| i.as_str()).is_some_and(|s| !s.is_empty()))
}

fn id_of(v: &Value) -> &str {
    v.get("id").and_then(|i| i.as_str()).unwrap_or("")
}

/* ── 合并 ─────────────────────────────────────── */

/// 把快照合并进当前目录。纯函数：不碰网络、不碰磁盘，可以逐条单测。
pub fn apply_snapshot(current: &Catalog, snap: &Snapshot) -> Result<Applied, Reject> {
    apply_snapshot_with(current, snap, false)
}

/// `force = true` 时跳过安全阀——用户看过差量后点「仍然应用」才走这条路。
pub fn apply_snapshot_with(
    current: &Catalog,
    snap: &Snapshot,
    force: bool,
) -> Result<Applied, Reject> {
    let old_total = current.providers.iter().map(|p| p.models.len()).sum::<usize>();

    // ① 墓碑先落地：快照里同时「下架 X」与「列出 X」时，以下架为准。
    let mut tombstones: BTreeSet<String> = BTreeSet::new();
    for t in &snap.tombstones {
        if valid_key(t) {
            tombstones.insert(t.clone());
        }
    }
    for p in &snap.providers {
        for m in &p.models {
            if m.withdrawn == Some(true) {
                tombstones.insert(format!("{}:{}", p.id, m.id));
            }
        }
    }

    let mut cat = current.clone();
    let mut deleted = 0usize;
    for key in &tombstones {
        if remove_model(&mut cat, key) {
            deleted += 1;
        }
    }

    // ② 合并厂商与模型
    let mut added = 0usize;
    let mut added_keys: Vec<String> = Vec::new();
    let mut active: Vec<String> = Vec::new();
    for fp in &snap.providers {
        let idx = match cat.providers.iter().position(|p| p.id == fp.id) {
            Some(i) => i,
            None => {
                cat.providers.push(CatalogProvider {
                    id: fp.id.clone(),
                    name: fp.name.clone().unwrap_or_else(|| fp.id.clone()),
                    api: fp.api.clone(),
                    docs: fp.docs.clone(),
                    tier: fp.tier.clone().unwrap_or_else(|| "C".into()),
                    free_tier: fp.free_tier.unwrap_or(false),
                    free_note: fp.free_note.clone(),
                    quota: fp.quota,
                    models: Vec::new(),
                    source: "feed".into(),
                });
                cat.providers.len() - 1
            }
        };

        {
            let p = &mut cat.providers[idx];
            if let Some(v) = &fp.name {
                if !v.is_empty() {
                    p.name = v.clone();
                }
            }
            if fp.api.is_some() {
                p.api = fp.api.clone();
            }
            if fp.docs.is_some() {
                p.docs = fp.docs.clone();
            }
            if let Some(v) = &fp.tier {
                if !v.is_empty() {
                    p.tier = v.clone();
                }
            }
            if let Some(v) = fp.free_tier {
                p.free_tier = v;
            }
            if fp.free_note.is_some() {
                p.free_note = fp.free_note.clone();
            }
            if fp.quota.is_some() {
                p.quota = fp.quota;
            }
            if p.source == "baseline" || p.source == "models.dev" {
                p.source = "both".into();
            }
        }

        for fm in &fp.models {
            let key = format!("{}:{}", fp.id, fm.id);
            if tombstones.contains(&key) {
                continue;
            }
            active.push(key.clone());
            let p = &mut cat.providers[idx];
            match p.models.iter_mut().find(|m| m.id == fm.id) {
                Some(m) => {
                    if fm.name.is_some() {
                        m.name = fm.name.clone().unwrap_or_default();
                    }
                    if fm.context.is_some() {
                        m.context = fm.context;
                    }
                    if fm.output_limit.is_some() {
                        m.output_limit = fm.output_limit;
                    }
                    if fm.cost_in.is_some() {
                        m.cost_in = fm.cost_in;
                    }
                    if fm.cost_out.is_some() {
                        m.cost_out = fm.cost_out;
                    }
                    if fm.tools.is_some() {
                        m.tools = fm.tools;
                    }
                    if fm.vision.is_some() {
                        m.vision = fm.vision;
                    }
                    if fm.reasoning.is_some() {
                        m.reasoning = fm.reasoning;
                    }
                    if fm.released_at.is_some() {
                        m.released_at = fm.released_at.clone();
                    }
                    // 免费判定重算：显式标注优先，否则看价格。
                    // 只在新模型或价格/标注变了的时候才改，避免 feed 里
                    // 只写 context 的条目把 free 从 true 翻成 false。
                    if fm.free.is_some() || fm.cost_in.is_some() || fm.cost_out.is_some() {
                        m.free = model_is_free(m.cost_in, m.cost_out, fm.free);
                    }
                }
                None => {
                    let mut m = CatalogModel {
                        id: fm.id.clone(),
                        name: fm.name.clone().unwrap_or_else(|| fm.id.clone()),
                        context: fm.context,
                        output_limit: fm.output_limit,
                        cost_in: fm.cost_in,
                        cost_out: fm.cost_out,
                        free: model_is_free(fm.cost_in, fm.cost_out, fm.free),
                        tools: fm.tools,
                        vision: fm.vision,
                        reasoning: fm.reasoning,
                        released_at: fm.released_at.clone(),
                        candidate: true,
                    };
                    if m.name.is_empty() {
                        m.name = m.id.clone();
                    }
                    added += 1;
                    added_keys.push(key);
                    p.models.push(m);
                }
            }
        }
    }

    // ③ 安全阀：两个比例都按「变更前」的规模算，避免互相污染
    if !force && exceeds(deleted, old_total, MAX_DELETE_RATIO, ABS_DELETE) {
        return Err(Reject::halted(format!(
            "本次要删除 {deleted} 个模型（原目录 {old_total} 个，超过 {}% 上限），已暂停；确认无误后可手动应用",
            (MAX_DELETE_RATIO * 100.0) as u32
        )));
    }
    if !force && exceeds(added, old_total, MAX_ADD_RATIO, ABS_ADD) {
        return Err(Reject::halted(format!(
            "本次要新增 {added} 个模型（原目录 {old_total} 个，超过 {}% 上限），已暂停；确认无误后可手动应用",
            (MAX_ADD_RATIO * 100.0) as u32
        )));
    }

    cat.recount();

    // ④ 事件：feed 没给就按差量补，避免界面上一片空白
    let mut events = snap.events.clone();
    if events.is_empty() {
        events = synthesize(snap.seq, &added_keys, &tombstones);
    }
    for (i, e) in events.iter_mut().enumerate() {
        if e.id.is_empty() {
            e.id = format!("{}:{}:{}", snap.seq, e.kind, i);
        }
        if e.seq == 0 {
            e.seq = snap.seq;
        }
    }

    Ok(Applied {
        catalog: cat,
        events,
        tombstones,
        added,
        added_keys,
        deleted,
        seq: snap.seq,
        version: snap.version.clone(),
        active,
        tier: None,
        feats: Vec::new(),
        feats_skipped: Vec::new(),
        snapshot: snap.clone(),
    })
}

/* ── 字段包（按 feat 下发） ─────────────────────
 *
 * 快照是「大家都能看到的那份事实」，字段包是「按能力开的那几项情报」。
 * 分成两种文档而不是两份快照，是因为快照的字节被摘要和补丁链钉住了：
 * 每多一种切法，就要多一套签名、多一条补丁链、多一次「给谁哪份」的判断。
 *
 * 客户端只认 manifest 里那份签名过的清单：不在清单里的包不会去拉，
 * 清单里的包一律先验摘要再解析。**不看本机档位**——服务端给什么就是什么。
 */

/// 厂商级额度情报：`{"providers":{"groq":{"rpm":30,"rpd":1000}}}`
pub const FEAT_QUOTA_INTEL: &str = "quota.intel";

/// 本版本认识的字段包。不认识的包**不应用但记名字**：将来服务端先上、
/// 客户端后升级时，用户看到的是「这个字段本版本还不支持」，而不是一片空白。
pub const KNOWN_FEATS: &[&str] = &[FEAT_QUOTA_INTEL];

/// 单个字段包的大小上限。情报是几十个数字，4MB 已经离谱到只可能是配置错了。
const MAX_FEAT_BYTES: usize = 4 * 1024 * 1024;

/// 把一份字段包合进目录。返回被改到的厂商数。
///
/// 只做「补齐既有条目」：包里点到目录里没有的厂商就跳过，不新建。
/// 字段包是情报，不是目录本体——它不该有能力凭空造出一个厂商。
pub fn apply_feat(cat: &mut Catalog, name: &str, v: &Value) -> Result<usize, String> {
    match name {
        FEAT_QUOTA_INTEL => {
            let Some(map) = v.get("providers").and_then(Value::as_object) else {
                return Err("缺少 providers 对象".into());
            };
            let mut touched = 0usize;
            for (pid, q) in map {
                let Some(p) = cat.providers.iter_mut().find(|p| &p.id == pid) else {
                    continue;
                };
                let (rpm, rpd) = (
                    q.get("rpm").and_then(as_count),
                    q.get("rpd").and_then(as_count),
                );
                if rpm.is_none() && rpd.is_none() {
                    continue;
                }
                let slot = p.quota.get_or_insert_with(Quota::default);
                // 只覆盖包里明说的字段：少写一项不等于「那一项没有限制」
                if rpm.is_some() {
                    slot.rpm = rpm;
                }
                if rpd.is_some() {
                    slot.rpd = rpd;
                }
                touched += 1;
            }
            Ok(touched)
        }
        other => Err(format!("本版本不认识字段包 {other}")),
    }
}

fn as_count(v: &Value) -> Option<u32> {
    let f = v.as_f64()?;
    if !f.is_finite() || f < 0.0 || f > u32::MAX as f64 {
        return None;
    }
    Some(f.round() as u32)
}

fn exceeds(count: usize, total: usize, ratio: f64, abs: usize) -> bool {
    if total >= MIN_FOR_RATIO {
        (count as f64) > (total as f64) * ratio
    } else {
        count >= abs
    }
}

fn valid_key(key: &str) -> bool {
    key.split_once(':').is_some_and(|(p, m)| !p.is_empty() && !m.is_empty())
}

fn remove_model(cat: &mut Catalog, key: &str) -> bool {
    let Some((pid, mid)) = key.split_once(':') else {
        return false;
    };
    let Some(p) = cat.providers.iter_mut().find(|p| p.id == pid) else {
        return false;
    };
    let before = p.models.len();
    p.models.retain(|m| m.id != mid);
    p.models.len() != before
}

fn synthesize(seq: u64, added: &[String], tombstones: &BTreeSet<String>) -> Vec<FeedEvent> {
    let at = chrono::Local::now().to_rfc3339();
    let mut out = Vec::new();
    for k in added.iter().take(20) {
        out.push(FeedEvent {
            id: format!("{seq}:model.added:{k}"),
            seq,
            kind: "model.added".into(),
            title: k.clone(),
            detail: format!("新模型进入 canary，{CANARY_OKS} 次成功后转正"),
            at: at.clone(),
            severity: None,
        });
    }
    for k in tombstones.iter().take(20) {
        out.push(FeedEvent {
            id: format!("{seq}:model.removed:{k}"),
            seq,
            kind: "model.removed".into(),
            title: k.clone(),
            detail: "已下架，路由立即不再选中".into(),
            at: at.clone(),
            severity: Some("down".into()),
        });
    }
    out
}

/* ── 网络 ─────────────────────────────────────── */

pub struct PollCfg<'a> {
    pub url: &'a str,
    /// `stable` / `beta`
    pub channel: &'a str,
    /// `auto` / `notify` / `pinned`
    pub policy: &'a str,
    /// 空则用 `BUILTIN_PUBKEY`
    pub pubkey: &'a str,
    /// 本地已应用的 seq
    pub since: u64,
    /// 跳过安全阀（用户确认后手动应用）
    pub force: bool,
    /// 本机档位（`free` / `pro`），随请求自报。服务端据此决定下发粒度——
    /// **客户端不做任何功能判断**，给什么就验什么。
    pub tier: &'a str,
    /// 许可证 JWT。只有 Pro（含宽限）才给；服务端据此认人。
    pub token: Option<&'a str>,
    /// 本地保存的上一份完整快照。只有它的 `seq` 等于 `since` 时才能走增量；
    /// `None` 就老老实实下全量。
    pub base: Option<&'a Snapshot>,
}

/// 一次请求的身份。两个头各管一件事：`x-freeport-tier` 说明本机是什么档，
/// `Authorization` 证明这份声明是真的（服务端验签）。
struct Identity<'a> {
    tier: &'a str,
    token: Option<&'a str>,
}

impl Identity<'_> {
    fn apply(&self, req: reqwest::RequestBuilder, url: &str) -> reqwest::RequestBuilder {
        let mut req = req.header("x-freeport-tier", self.tier);
        if let Some(t) = self.token.filter(|_| allow_bearer(url)) {
            req = req.bearer_auth(t);
        }
        req
    }
}

/// 只有 https 或本机回环才带许可证。明文公网地址上带 JWT 等于把凭据
/// 广播给路上每一个人；自建 feed 用 `http://localhost:8080` 是常见做法，得放行。
fn allow_bearer(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority); // 去掉 userinfo
    let host = if let Some(after) = host.strip_prefix('[') {
        after.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// `scheme://host:port`。快照与 manifest 不同源时不再带许可证：
/// 地址虽在签名里，但配置是用户填的，没必要把 token 送到第三方域名。
fn origin(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let (scheme, rest) = lower.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}


pub enum Poll {
    /// 没有更新（`seq` 相同或服务端 304）
    Unchanged,
    /// `notify` 策略下只报信，等用户确认
    Pending(Manifest),
    Applied(Box<Applied>),
}

pub async fn poll(
    client: &reqwest::Client,
    cfg: &PollCfg<'_>,
    current: &Catalog,
) -> Result<Poll, Reject> {
    if cfg.url.trim().is_empty() {
        return Err(Reject::invalid("feed 地址为空"));
    }
    if cfg.policy == POLICY_PINNED {
        return Err(Reject::invalid("目录已锁定（pinned），不接收 feed"));
    }

    let id = Identity {
        tier: if cfg.tier.trim().is_empty() { "free" } else { cfg.tier },
        token: cfg.token,
    };

    let Some(manifest) = fetch_manifest(client, cfg.url, cfg.since, &id).await? else {
        return Ok(Poll::Unchanged);
    };

    let pk = if cfg.pubkey.trim().is_empty() { BUILTIN_PUBKEY } else { cfg.pubkey };
    manifest.verify(pk)?;

    // 频道：stable 客户端不吃 beta 的包，beta 客户端两者都吃。
    if manifest.channel == CHANNEL_BETA && cfg.channel != CHANNEL_BETA {
        return Err(Reject::invalid("收到 beta 频道的包，当前订阅 stable，已忽略"));
    }

    if manifest.seq < cfg.since {
        return Err(Reject::invalid(format!(
            "seq 回滚：本地 {}，远端 {}，已丢弃",
            cfg.since, manifest.seq
        )));
    }
    if manifest.seq == cfg.since {
        return Ok(Poll::Unchanged);
    }

    if cfg.policy == POLICY_NOTIFY {
        return Ok(Poll::Pending(manifest));
    }

    let snap = match try_incremental(client, cfg, &manifest, &id).await {
        Ok(Some(s)) => {
            tracing::info!(from = cfg.since, to = manifest.seq, "目录走增量补丁更新");
            s
        }
        Ok(None) => fetch_snapshot(client, cfg.url, &manifest, &id).await?,
        Err(e) => {
            // 增量只是省流量，不是真相来源：补丁链有任何不对就退回全量。
            // 但原因必须留痕——否则「每次都下全量」永远查不出为什么。
            tracing::warn!(
                from = cfg.since,
                to = manifest.seq,
                "增量补丁链不可用，回退全量：{e}"
            );
            fetch_snapshot(client, cfg.url, &manifest, &id).await?
        }
    };
    if snap.seq != 0 && snap.seq != manifest.seq {
        return Err(Reject::invalid(format!(
            "快照 seq {} 与 manifest seq {} 不一致，已丢弃",
            snap.seq, manifest.seq
        )));
    }
    let mut applied = apply_snapshot_with(current, &snap, cfg.force)?;
    applied.tier = manifest.tier.clone();
    // 字段包放在快照之后：主链路先成，情报后到。任何一份包出问题
    // 都只写进 `feats_skipped`，不推翻这次已经成功的目录更新。
    if !manifest.feat.is_empty() {
        apply_feats(client, cfg.url, &manifest, &id, &mut applied).await;
    }
    Ok(Poll::Applied(Box::new(applied)))
}

/// 逐份拉取并应用字段包。
///
/// 每一份都单独算账：下载失败 / 摘要不符 / 形状不对 → 记原因跳过，
/// 其余照常应用。这里刻意不用 `?`，因为「少一份情报」与「目录同步失败」
/// 是两件严重程度完全不同的事。
async fn apply_feats(
    client: &reqwest::Client,
    base: &str,
    m: &Manifest,
    id: &Identity<'_>,
    applied: &mut Applied,
) {
    for f in &m.feat {
        // manifest 里的名字已经过了签名，但客户端必须先确认自己懂它——
        // 「先信后懂」等于把解析器交给发布方
        if !KNOWN_FEATS.contains(&f.name.as_str()) {
            applied.feats_skipped.push(format!("{}（本版本不支持）", f.name));
            continue;
        }
        let url = resolve_url(base, &f.url);
        let bytes = match fetch_verified(
            client,
            base,
            &url,
            &f.sha256,
            MAX_FEAT_BYTES,
            id,
            "字段包",
        )
        .await
        {
            Ok(b) => b,
            Err(e) => {
                applied.feats_skipped.push(format!("{}（{e}）", f.name));
                continue;
            }
        };
        let v: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                applied.feats_skipped.push(format!("{}（不是合法 JSON：{e}）", f.name));
                continue;
            }
        };
        match apply_feat(&mut applied.catalog, &f.name, &v) {
            Ok(_) => applied.feats.push(f.name.clone()),
            Err(e) => applied.feats_skipped.push(format!("{}（{e}）", f.name)),
        }
    }
}

async fn fetch_manifest(
    client: &reqwest::Client,
    url: &str,
    since: u64,
    id: &Identity<'_>,
) -> Result<Option<Manifest>, Reject> {
    let full = with_since(url, since);
    let resp = id
        .apply(client.get(&full), &full)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| Reject::invalid(format!("拉取 manifest 失败：{e}")))?;

    // 304 / 204：服务端按 `?since=` 判断没有更新
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED
        || resp.status() == reqwest::StatusCode::NO_CONTENT
    {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(Reject::invalid(format!(
            "拉取 manifest 失败：HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Reject::invalid(format!("读取 manifest 失败：{e}")))?;
    let m = parse_manifest(&bytes)?;
    Ok(Some(m))
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, Reject> {
    serde_json::from_slice(bytes).map_err(|e| Reject::invalid(format!("manifest 不是合法 JSON：{e}")))
}

async fn fetch_snapshot(
    client: &reqwest::Client,
    base: &str,
    m: &Manifest,
    id: &Identity<'_>,
) -> Result<Snapshot, Reject> {
    let url = resolve_url(base, &m.url);
    let bytes = fetch_verified(
        client,
        base,
        &url,
        &m.sha256,
        MAX_SNAPSHOT_BYTES,
        id,
        "快照",
    )
    .await?;
    serde_json::from_slice(&bytes).map_err(|e| Reject::invalid(format!("快照不是合法 JSON：{e}")))
}

/// 下载一份被摘要钉住的文件。快照与补丁共用这条路：
/// 「同源才带许可证」和「摘要不符就丢」两条规则只该写一遍。
async fn fetch_verified(
    client: &reqwest::Client,
    base: &str,
    url: &str,
    sha256: &str,
    max_bytes: usize,
    id: &Identity<'_>,
    what: &str,
) -> Result<Vec<u8>, Reject> {
    // 只有和 manifest 同源才带许可证：签名保证了地址是发布方写的，
    // 但「发布方把快照放 CDN」与「有人把配置指到别人的服务器」看起来一样，
    // 后者不该收到本机的 Pro 凭据。
    let same_origin = origin(url).is_some() && origin(url) == origin(base);
    let id = Identity {
        tier: id.tier,
        token: id.token.filter(|_| same_origin),
    };
    let resp = id
        .apply(client.get(url), url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|e| Reject::invalid(format!("拉取{what}失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(Reject::invalid(format!(
            "拉取{what}失败：HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Reject::invalid(format!("读取{what}失败：{e}")))?;
    if bytes.len() > max_bytes {
        return Err(Reject::invalid(format!(
            "{what}有 {} 字节，超过上限 {max_bytes}",
            bytes.len()
        )));
    }

    // 摘要对不上就停：可能是传输损坏，也可能是被换了包
    let digest = hex::encode(Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(sha256.trim()) {
        return Err(Reject::invalid(format!(
            "{what}摘要不匹配（期望 {}，实际 {}），已丢弃",
            short(sha256),
            short(&digest)
        )));
    }
    Ok(bytes.to_vec())
}

/// 本地基线 + 签名过的补丁链 → 新快照。
///
/// 返回 `Ok(None)` 表示「这次不适合走增量」（没有基线、链不连续），
/// 调用方直接下全量，不算错误。
async fn try_incremental(
    client: &reqwest::Client,
    cfg: &PollCfg<'_>,
    m: &Manifest,
    id: &Identity<'_>,
) -> Result<Option<Snapshot>, Reject> {
    let Some(base) = cfg.base else {
        return Ok(None);
    };
    if base.seq != cfg.since {
        // 本地缓存与 feed.seq 对不上（可能是手动改过配置），别硬拼
        return Ok(None);
    }
    let Some(chain) = plan_chain(&m.patches, cfg.since, m.seq) else {
        return Ok(None);
    };

    let mut doc = snapshot_value(base);
    for p in chain {
        let url = resolve_url(cfg.url, &p.url);
        let bytes = fetch_verified(
            client,
            cfg.url,
            &url,
            &p.sha256,
            MAX_PATCH_BYTES,
            id,
            "补丁",
        )
        .await?;
        let pf: PatchFile = serde_json::from_slice(&bytes)
            .map_err(|e| Reject::invalid(format!("补丁不是合法 JSON：{e}")))?;
        if pf.from_seq != p.from_seq || pf.to_seq != p.to_seq {
            return Err(Reject::invalid(format!(
                "补丁声明的区间 {}→{} 与 manifest 的 {}→{} 不一致",
                pf.from_seq, pf.to_seq, p.from_seq, p.to_seq
            )));
        }
        apply_patch_file(&mut doc, &pf)?;
    }

    let snap: Snapshot = serde_json::from_value(doc)
        .map_err(|e| Reject::invalid(format!("补丁链产出不是合法快照：{e}")))?;
    if snap.seq != m.seq {
        return Err(Reject::invalid(format!(
            "补丁链停在 seq {}，manifest 是 {}",
            snap.seq, m.seq
        )));
    }
    Ok(Some(snap))
}

fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}…", &s[..12])
    }
}

fn with_since(url: &str, since: u64) -> String {
    if since == 0 {
        return url.to_string();
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}since={since}")
}

/// 相对地址按 manifest 所在目录解析。绝对地址原样返回（已参与签名）。
pub fn resolve_url(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    match base.rfind('/') {
        Some(i) => format!("{}/{}", &base[..i], rel.trim_start_matches('/')),
        None => rel.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CatalogProvider;
    use ed25519_dalek::{Signer, SigningKey};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_hex(k: &SigningKey) -> String {
        hex::encode(k.verifying_key().to_bytes())
    }

    fn signed(m: &mut Manifest, k: &SigningKey) {
        let payload = jcs::canonical(&m.signing_payload()).unwrap();
        m.sig = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            k.sign(payload.as_bytes()).to_bytes(),
        );
    }

    fn manifest(seq: u64) -> Manifest {
        Manifest {
            version: format!("2026.10.19.{seq}"),
            channel: "stable".into(),
            seq,
            published_at: "2026-10-19T03:00:00Z".into(),
            sha256: "00".repeat(32),
            sig: String::new(),
            url: "snap.json".into(),
            tier: None,
            notes: None,
            feat: Vec::new(),
            patches: Vec::new(),
        }
    }

    fn catalog_with(n: usize) -> Catalog {
        let models = (0..n)
            .map(|i| CatalogModel {
                id: format!("m{i}"),
                name: format!("M{i}"),
                free: true,
                ..Default::default()
            })
            .collect();
        let mut c = Catalog {
            providers: vec![CatalogProvider {
                id: "p".into(),
                name: "P".into(),
                tier: "A".into(),
                source: "baseline".into(),
                models,
                ..Default::default()
            }],
            ..Default::default()
        };
        c.recount();
        c
    }

    fn snap(seq: u64, providers: Vec<FeedProvider>) -> Snapshot {
        Snapshot { seq, version: format!("v{seq}"), providers, ..Default::default() }
    }

    #[test]
    fn manifest_signature_binds_url_and_seq() {
        let k = key(7);
        let mut m = manifest(3);
        signed(&mut m, &k);
        assert!(m.verify(&pubkey_hex(&k)).is_ok());

        // 改 seq → 签名失效
        let mut tampered = m.clone();
        tampered.seq = 4;
        assert!(tampered.verify(&pubkey_hex(&k)).is_err());

        // 把地址指到别处 → 签名失效（否则可以把客户端引到攻击者服务器）
        let mut repointed = m.clone();
        repointed.url = "https://evil.example/snap.json".into();
        assert!(repointed.verify(&pubkey_hex(&k)).is_err());

        // 换一把公钥 → 失效
        assert!(m.verify(&pubkey_hex(&key(8))).is_err());
        // 没有签名 → 直接拒
        assert!(manifest(3).verify(&pubkey_hex(&k)).is_err());
    }

    #[test]
    fn tier_is_signed_when_present() {
        let k = key(7);
        let mut m = manifest(3);
        m.tier = Some("pro".into());
        signed(&mut m, &k);
        assert!(m.verify(&pubkey_hex(&k)).is_ok());

        // 把免费包改标成 Pro 包 → 签名失效（界面不该被中间人说谎）
        let mut relabeled = m.clone();
        relabeled.tier = Some("free".into());
        assert!(relabeled.verify(&pubkey_hex(&k)).is_err());

        // 不带 tier 的老 manifest 仍然可验：字段缺省不进载荷
        let old = {
            let mut o = manifest(3);
            signed(&mut o, &k);
            o
        };
        assert!(old.signing_payload().get("tier").is_none());
        assert!(old.verify(&pubkey_hex(&k)).is_ok());
    }

    #[test]
    fn license_never_leaves_over_plaintext_http() {
        assert!(allow_bearer("https://feed.freeport.dev/latest.json"));
        assert!(allow_bearer("http://127.0.0.1:8080/latest.json"));
        assert!(allow_bearer("http://localhost/latest.json"));
        assert!(allow_bearer("http://[::1]:9000/latest.json"));
        // 公网明文一律不带
        assert!(!allow_bearer("http://feed.example.com/latest.json"));
        assert!(!allow_bearer("http://user:pw@feed.example.com/x"));
        assert!(!allow_bearer("ftp://feed.example.com/x"));
        assert!(!allow_bearer("not a url"));
    }

    #[test]
    fn snapshot_token_requires_same_origin() {
        assert_eq!(
            origin("https://Feed.Example.com/a/latest.json").as_deref(),
            Some("https://feed.example.com")
        );
        assert_eq!(
            origin("http://127.0.0.1:9/x").as_deref(),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(origin("nonsense"), None);
        assert_ne!(
            origin("https://cdn.example.com/snap.json"),
            origin("https://feed.example.com/latest.json")
        );
    }

    #[test]
    fn new_models_enter_as_candidates_and_tombstones_win() {
        let current = catalog_with(60);
        let mut s = snap(
            5,
            vec![FeedProvider {
                id: "p".into(),
                models: vec![
                    FeedModel { id: "brand-new".into(), context: Some(8192), free: Some(true), ..Default::default() },
                    FeedModel { id: "m1".into(), withdrawn: Some(true), ..Default::default() },
                ],
                ..Default::default()
            }],
        );
        s.tombstones = vec!["p:m2".into()];

        let out = apply_snapshot(&current, &s).unwrap();
        assert_eq!(out.added, 1);
        assert_eq!(out.deleted, 2);
        let p = out.catalog.provider("p").unwrap();
        let new = p.models.iter().find(|m| m.id == "brand-new").unwrap();
        assert!(new.candidate, "feed 新加的模型必须先进 canary");
        assert!(p.models.iter().all(|m| m.id != "m1" && m.id != "m2"));
        assert!(out.tombstones.contains("p:m1"));
        assert!(out.tombstones.contains("p:m2"));
    }

    #[test]
    fn provider_level_free_intel_overrides_but_absent_fields_do_not() {
        let current = catalog_with(3);
        let s = snap(
            1,
            vec![FeedProvider {
                id: "p".into(),
                free_tier: Some(true),
                free_note: Some("1000 RPD".into()),
                // 没给 tier / api，就不该被清空
                models: vec![FeedModel { id: "m0".into(), context: Some(4096), ..Default::default() }],
                ..Default::default()
            }],
        );
        let out = apply_snapshot(&current, &s).unwrap();
        let p = out.catalog.provider("p").unwrap();
        assert!(p.free_tier);
        assert_eq!(p.free_note.as_deref(), Some("1000 RPD"));
        assert_eq!(p.tier, "A", "没给的字段必须保留本地值");
        assert_eq!(p.models.len(), 3, "只更新，不新增");
        assert_eq!(p.models[0].context, Some(4096));
        assert!(p.models[0].free, "只给 context 不能把 free 翻成 false");
        assert!(!p.models[0].candidate, "老模型不进入 canary");
    }

    #[test]
    fn safety_valve_halts_large_deletions() {
        let current = catalog_with(100);
        let mut s = snap(2, vec![]);
        s.tombstones = (0..20).map(|i| format!("p:m{i}")).collect();
        let err = apply_snapshot(&current, &s).unwrap_err();
        assert!(err.halted);
        assert!(err.message.contains("删除 20"));
    }

    #[test]
    fn safety_valve_halts_massive_additions() {
        // 100 个模型的目录一次加 40 个（+40%），超过 30% 上限，必须停下来等人工确认
        let current = catalog_with(100);
        let models: Vec<FeedModel> = (0..40)
            .map(|i| FeedModel { id: format!("new{i}"), ..Default::default() })
            .collect();
        let s = snap(2, vec![FeedProvider { id: "p".into(), models, ..Default::default() }]);
        let err = apply_snapshot(&current, &s).unwrap_err();
        assert!(err.halted, "一次新增 40% 必须停下来");
        assert!(err.message.contains("新增 40"));
    }

    #[test]
    fn small_catalog_uses_absolute_floor_instead_of_ratio() {
        // 3 个模型加 1 个是 +33%，但绝对值很小，不该触发人工确认
        let current = catalog_with(3);
        let s = snap(
            2,
            vec![FeedProvider {
                id: "p".into(),
                models: vec![FeedModel { id: "new".into(), ..Default::default() }],
                ..Default::default()
            }],
        );
        assert!(apply_snapshot(&current, &s).is_ok());
    }

    #[test]
    fn events_are_synthesized_when_feed_omits_them() {
        let current = catalog_with(3);
        let s = snap(
            9,
            vec![FeedProvider {
                id: "p".into(),
                models: vec![FeedModel { id: "n1".into(), free: Some(true), ..Default::default() }],
                ..Default::default()
            }],
        );
        let out = apply_snapshot(&current, &s).unwrap();
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].kind, "model.added");
        assert_eq!(out.events[0].seq, 9);
        assert!(!out.events[0].id.is_empty());
    }

    #[test]
    fn relative_snapshot_url_resolves_against_manifest_dir() {
        assert_eq!(
            resolve_url("https://f.example/catalog/latest.json", "snapshots/a.json"),
            "https://f.example/catalog/snapshots/a.json"
        );
        assert_eq!(
            resolve_url("https://f.example/catalog/latest.json", "https://cdn.example/a.json"),
            "https://cdn.example/a.json"
        );
        assert_eq!(
            with_since("https://f.example/latest.json?channel=stable", 7),
            "https://f.example/latest.json?channel=stable&since=7"
        );
        assert_eq!(with_since("https://f.example/latest.json", 0), "https://f.example/latest.json");
    }

    /* ── 增量补丁 ── */

    fn fsnap(seq: u64, ids: &[&str]) -> Snapshot {
        snap(
            seq,
            vec![FeedProvider {
                id: "acme".into(),
                name: Some("Acme".into()),
                models: ids
                    .iter()
                    .map(|id| FeedModel { id: (*id).into(), ..Default::default() })
                    .collect(),
                ..Default::default()
            }],
        )
    }

    #[test]
    fn patch_supports_the_four_safe_ops() {
        let mut doc = json!({"seq": 1, "a": {"b": [1, 2]}, "drop": true});
        apply_patch(
            &mut doc,
            &[
                json!({"op": "test", "path": "/a/b/0", "value": 1}),
                json!({"op": "add", "path": "/a/b/-", "value": 3}),
                json!({"op": "replace", "path": "/seq", "value": 2}),
                json!({"op": "remove", "path": "/drop"}),
            ],
        )
        .unwrap();
        assert_eq!(doc, json!({"seq": 2, "a": {"b": [1, 2, 3]}}));

        // 转义：`~0` → `~`，`~1` → `/`
        let mut esc = json!({"a/b": 1, "c~d": 2});
        apply_patch(&mut esc, &[json!({"op": "remove", "path": "/a~1b"})]).unwrap();
        assert_eq!(esc, json!({"c~d": 2}));
    }

    #[test]
    fn patch_refuses_unsafe_ops_and_broken_paths() {
        let base = json!({"a": [1]});
        let cases = [
            json!({"op": "move", "path": "/a", "from": "/a"}),
            json!({"op": "copy", "path": "/a", "from": "/a"}),
            json!({"op": "replace", "path": "/nope", "value": 1}), // 目标不存在
            json!({"op": "remove", "path": "/a/5"}),               // 越界
            json!({"op": "add", "path": "a", "value": 1}),         // 缺前导斜杠
            json!({"op": "add", "path": "/a/01", "value": 1}),     // 非法下标
            json!({"op": "test", "path": "/a/0", "value": 9}),     // 前置条件不成立
            json!({"op": "add", "path": "/a/b/c", "value": 1}),    // 中途类型不对
        ];
        for op in cases {
            let mut doc = base.clone();
            assert!(apply_patch(&mut doc, &[op.clone()]).is_err(), "{op}");
        }
    }

    #[test]
    fn patch_file_checks_baseline_and_digest() {
        let prev = fsnap(1, &["m1", "m2"]);
        let next = fsnap(2, &["m1", "m2", "m3"]);
        let ops = diff(&snapshot_value(&prev), &snapshot_value(&next));
        let pf = PatchFile {
            from_seq: 1,
            to_seq: 2,
            ops,
            sha256_after: snapshot_digest(&next),
        };

        let mut doc = snapshot_value(&prev);
        apply_patch_file(&mut doc, &pf).unwrap();
        let got: Snapshot = serde_json::from_value(doc).unwrap();
        assert_eq!(got.seq, 2);
        assert_eq!(got.providers[0].models.len(), 3);

        // 基线不对：本地是 seq 9，补丁要求 1
        let mut wrong = snapshot_value(&prev);
        wrong["seq"] = json!(9);
        assert!(apply_patch_file(&mut wrong, &pf).is_err());

        // 摘要被改：内容改对了也要拒
        let mut bad = pf.clone();
        bad.sha256_after = "00".repeat(32);
        assert!(apply_patch_file(&mut snapshot_value(&prev), &bad).is_err());
    }

    /// diff 的不变式：套回 `prev` 必须**逐字段等于** `next`。
    /// 顺序变化这种 diff 表达不了的情形，会退化成整体 replace，所以也得过。
    #[test]
    fn diff_always_roundtrips() {
        let cases = vec![
            (fsnap(1, &["m1", "m2", "m3"]), fsnap(2, &["m1", "m2", "m3", "m4"])),
            (fsnap(1, &["m1", "m2", "m3"]), fsnap(2, &["m1", "m3"])),
            // 顺序变了：按 id 增量拼不回去，必须整体替换
            (fsnap(1, &["m1", "m2", "m3"]), fsnap(2, &["m3", "m1"])),
            // 改字段
            (
                fsnap(1, &["m1"]),
                {
                    let mut s = fsnap(2, &["m1"]);
                    s.providers[0].models[0].context = Some(8192);
                    s
                },
            ),
            // 整家厂商消失
            (fsnap(1, &["m1"]), snap(2, vec![])),
        ];

        for (a, b) in cases {
            let (va, vb) = (snapshot_value(&a), snapshot_value(&b));
            let ops = diff(&va, &vb);
            let mut doc = va.clone();
            apply_patch(&mut doc, &ops).unwrap();
            assert_eq!(doc, vb, "ops = {ops:?}");
        }
    }

    #[test]
    fn plan_chain_needs_contiguous_hops() {
        let p = |f: u64, t: u64| PatchRef {
            from_seq: f,
            to_seq: t,
            url: format!("{f}-{t}.json"),
            sha256: String::new(),
        };
        let all = vec![p(1, 2), p(2, 3), p(3, 4)];
        assert_eq!(plan_chain(&all, 1, 4).unwrap().len(), 3);
        assert_eq!(plan_chain(&all, 2, 4).unwrap().len(), 2);
        assert!(plan_chain(&all, 4, 4).is_none(), "不需要更新");
        assert!(plan_chain(&all, 0, 4).is_none(), "没有基线");
        assert!(plan_chain(&all, 1, 5).is_none(), "走不到目标");
        assert!(plan_chain(&[p(1, 2), p(3, 4)], 1, 4).is_none(), "断链");
        assert!(plan_chain(&[p(1, 1)], 1, 2).is_none(), "空转环");
    }

    /* ── 字段包 ─────────────────────────────────── */

    /// 情报只补既有条目：包里点到目录里没有的厂商，跳过就行。
    /// 让它新建厂商等于给了数据面造目录本体的能力。
    #[test]
    fn quota_intel_never_invents_providers() {
        let mut cat = catalog_with(1);
        let pack = json!({
            "providers": {
                "p": { "rpm": 30, "rpd": 1000 },
                "ghost": { "rpm": 999 }
            }
        });
        assert_eq!(apply_feat(&mut cat, FEAT_QUOTA_INTEL, &pack).unwrap(), 1);
        assert_eq!(cat.providers.len(), 1, "不能凭空多出一家");
        let q = cat.providers[0].quota.as_ref().expect("额度情报该落进来");
        assert_eq!((q.rpm, q.rpd), (Some(30), Some(1000)));
    }

    /// 包里少写一项 ≠ 那一项没有限制：只覆盖明说的字段。
    #[test]
    fn quota_intel_only_overwrites_the_fields_it_states() {
        let mut cat = catalog_with(1);
        cat.providers[0].quota = Some(Quota { rpm: Some(5), rpd: None });

        let pack = json!({ "providers": { "p": { "rpd": 500 } } });
        apply_feat(&mut cat, FEAT_QUOTA_INTEL, &pack).unwrap();
        let q = cat.providers[0].quota.clone().unwrap();
        assert_eq!(q.rpm, Some(5), "包没提 rpm，不能把它清掉");
        assert_eq!(q.rpd, Some(500));

        // 形状不对要报错，而不是「什么都没改但装作成功」
        assert!(apply_feat(&mut cat, FEAT_QUOTA_INTEL, &json!({"x": 1})).is_err());
        // 完全不认识的包名同样要报错，让调用方记进 featsSkipped
        assert!(apply_feat(&mut cat, "model.scores", &json!({})).is_err());
    }

    /// 值可能是整数也可能是浮点（JSON 没区分）：都要能读，负数与非数要丢。
    #[test]
    fn quota_intel_accepts_json_numbers_and_rejects_nonsense() {
        let mut cat = catalog_with(1);
        let pack = json!({ "providers": { "p": { "rpm": 30.0, "rpd": -1 } } });
        apply_feat(&mut cat, FEAT_QUOTA_INTEL, &pack).unwrap();
        let q = cat.providers[0].quota.clone().unwrap();
        assert_eq!(q.rpm, Some(30));
        assert_eq!(q.rpd, None, "负数不是额度");
    }
}
