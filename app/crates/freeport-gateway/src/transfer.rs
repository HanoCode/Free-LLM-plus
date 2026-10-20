//! 配置迁移：把一台机器上的「接入信息」搬到另一台。
//!
//! 迁移码里**包含明文 API key**，所以它必须是一个加密信封，而不是一段 JSON。
//! 口令不落盘、不上传，只用来派生 AES-256-GCM 的密钥；丢了就解不开，
//! 这是有意的——能解开它的人就能拿走你所有的 key。
//!
//! 装什么：
//! - 厂商、凭据（含四项限额）、凭据下的密钥
//! - 虚拟 key（只搬 `key_hash`，所以原来发给 Agent 的 `fp-...` 迁移后**继续可用**）
//! - 选路配置、巡检配置
//!
//! 不装什么：
//! - 用量台账、冷却、健康度（这些是「这台机器发生过什么」，搬过去只会骗自己）
//! - 模型目录（导入后按需重拉，带上会让迁移码大十倍）
//! - 网关端口与鉴权 token（与本机网络环境绑定）
//! - OAuth token（刷新令牌与设备绑定，换机会被上游判定异常；Tier C 再单独处理）
//! - 本机安装码与许可证（license 可绑机器，搬过去也是废纸；导入后原 license 保留）
//!
//! 格式稳定性：信封自带 KDF 参数，将来换参数也能解开老迁移码；`v` 是格式版本，
//! 遇到不认识的主版本直接拒绝，不做「尽力解析」。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload as AeadPayload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use freeport_core::model::{ApiKey, AppData, Health, PatrolConfig, Provider, RoutingConfig};

use crate::secrets;

/// 信封格式标识。改它等于换一种文件类型，旧版本会直接拒绝而不是解出乱码。
const FORMAT: &str = "freeport.transfer";
/// 当前格式版本。
const VERSION: u32 = 1;
/// 口令下限。短口令在 Argon2 面前也撑不了几分钟，不如一开始就拒绝。
pub const MIN_PASSPHRASE: usize = 8;

/// 迁移包文件的扩展名。内容就是迁移码原文，只是存成了文件。
pub const FILE_EXT: &str = "fpk";
/// 正常的迁移包 1–20 KB；给一个宽松上限，避免把 2GB 的日志当迁移包读进内存。
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 默认 KDF 参数（OWASP 2024 对 Argon2id 的推荐量级）。
/// 单次约 0.3–0.8s，桌面端一次性导出/导入可以接受。
const KDF_M: u32 = 19_456;
const KDF_T: u32 = 2;
const KDF_P: u32 = 1;
/// 手写一个信封骗我们分配 4GiB 内存是很容易的，所以参数必须在可信区间内。
const KDF_M_MAX: u32 = 262_144;

/// 迁移码（加密信封）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    format: String,
    v: u32,
    kdf: String,
    /// Argon2id 的内存开销（KiB）
    m: u32,
    /// 迭代次数
    t: u32,
    /// 并行度
    p: u32,
    salt: String,
    nonce: String,
    ct: String,
}

/// 信封里真正被加密的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Body {
    /// 导出时间，只用于展示
    #[serde(rename = "exportedAt")]
    exported_at: String,
    /// 来源描述，只用于展示（「Freeport 1.3 · macOS」）
    source: String,
    payload: Payload,
    /// account_id -> 明文 API key。**只存在于密文里**。
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// 可迁移的数据子集。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    #[serde(default)]
    providers: Vec<Provider>,
    #[serde(default)]
    routing: RoutingConfig,
    #[serde(default)]
    patrol: PatrolConfig,
    #[serde(default, rename = "apiKeys")]
    api_keys: Vec<ApiKey>,
}

/// 导入策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只补充本地没有的厂商/凭据/key；本地已有的同 id 条目一律保留
    Merge,
    /// 整体替换（会清掉本地多余的凭据与对应的密文）
    Replace,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "merge" => Ok(Mode::Merge),
            "replace" => Ok(Mode::Replace),
            other => Err(format!("不认识的导入方式：{other}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Merge => "merge",
            Mode::Replace => "replace",
        }
    }
}

/// 导入结果。UI 要能说清「到底加进来了什么、跳过了什么」。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub mode: String,
    /// 新增的厂商数
    pub providers_added: usize,
    /// 本地已有、按策略保留的厂商数
    pub providers_kept: usize,
    pub accounts_added: usize,
    pub accounts_kept: usize,
    pub keys_added: usize,
    pub keys_kept: usize,
    /// 真正写进密钥库的条数
    pub secrets_written: usize,
    /// 密文里带了、但本地没有对应凭据的条目（丢弃）
    pub secrets_orphaned: usize,
    /// 因为本机墓碑而跳过的厂商数（用户在这台机器上删过）
    pub providers_skipped: usize,
    /// 因为本机墓碑而跳过的凭据数
    pub accounts_skipped: usize,
    /// 导出时间与来源，UI 用来提示「这是哪台机器什么时候的」
    pub exported_at: String,
    pub source: String,
}

/// 把一台机器的配置封成迁移码。
pub fn export(data: &AppData, passphrase: &str, source: &str) -> Result<String, String> {
    check_passphrase(passphrase)?;

    // 明文 key 只在内存里过一遍，随后立刻进 AES-GCM。
    let mut secrets_map = BTreeMap::new();
    for p in &data.providers {
        for a in &p.accounts {
            if let Some(k) = secrets::get_key(&a.id) {
                secrets_map.insert(a.id.clone(), k);
            }
        }
    }

    let body = Body {
        exported_at: chrono::Utc::now().to_rfc3339(),
        source: source.to_string(),
        payload: Payload {
            // 模型目录重拉即可；健康度与用量是这台机器的历史，不该跟着走
            providers: data.providers.iter().map(portable).collect(),
            routing: data.routing.clone(),
            patrol: data.patrol.clone(),
            api_keys: data.api_keys.clone(),
        },
        secrets: secrets_map,
    };
    let plain = serde_json::to_vec(&body).map_err(|e| format!("序列化失败：{e}"))?;

    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let key = derive(passphrase, &salt, KDF_M, KDF_T, KDF_P)?;

    let env = Envelope {
        format: FORMAT.into(),
        v: VERSION,
        kdf: "argon2id".into(),
        m: KDF_M,
        t: KDF_T,
        p: KDF_P,
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        // 占位，下面用带 AAD 的加密结果覆盖
        ct: String::new(),
    };
    let ct = seal(&key, &nonce, &aad(&env), &plain)?;

    let mut env = env;
    env.ct = B64.encode(ct);
    serde_json::to_string(&env).map_err(|e| format!("信封序列化失败：{e}"))
}

/// 解开迁移码。纯函数：不写任何文件，先让用户看到「里面有什么」再决定要不要导入。
pub fn open(blob: &str, passphrase: &str) -> Result<Body, String> {
    let env: Envelope = serde_json::from_str(blob.trim())
        .map_err(|_| "这不是一段 Freeport 迁移码（JSON 解析失败）".to_string())?;
    if env.format != FORMAT {
        return Err("这不是 Freeport 迁移码".into());
    }
    if env.v != VERSION {
        return Err(format!(
            "迁移码版本 {} 不认识（本机支持 {VERSION}），请升级 Freeport",
            env.v
        ));
    }
    if env.kdf != "argon2id" {
        return Err(format!("不支持的密钥派生方式：{}", env.kdf));
    }
    if env.m == 0 || env.m > KDF_M_MAX || env.t == 0 || env.t > 16 || env.p == 0 || env.p > 8 {
        return Err("迁移码里的 KDF 参数不合理，已拒绝解析".into());
    }

    let salt = B64
        .decode(env.salt.trim())
        .map_err(|_| "迁移码损坏（salt）".to_string())?;
    let nonce = B64
        .decode(env.nonce.trim())
        .map_err(|_| "迁移码损坏（nonce）".to_string())?;
    let ct = B64
        .decode(env.ct.trim())
        .map_err(|_| "迁移码损坏（密文）".to_string())?;
    if nonce.len() != 12 {
        return Err("迁移码损坏（nonce 长度不对）".into());
    }
    let key = derive(passphrase, &salt, env.m, env.t, env.p)?;
    let plain = open_sealed(&key, &nonce, &aad(&env), &ct)
        .map_err(|_| "口令不对，或迁移码被改过".to_string())?;
    let body: Body =
        serde_json::from_slice(&plain).map_err(|e| format!("迁移码内容损坏：{e}"))?;
    Ok(body)
}

/// 把解出来的内容合并进 `data`，并把密钥写进本机密钥库。
///
/// 调用方负责 `store::save`。这里只做「数据层可验证」的那一半，
/// 好处是测试可以直接构造 `AppData` 断言合并语义。
pub fn apply(body: Body, data: &mut AppData, mode: Mode) -> Summary {
    let Body {
        exported_at,
        source,
        payload,
        secrets: incoming,
    } = body;

    let mut sum = Summary {
        mode: mode.as_str().into(),
        exported_at,
        source,
        ..Default::default()
    };

    // 这台机器上的凭据 id，用于最后清理「导入后不再存在的密文」
    let before: Vec<String> = data
        .providers
        .iter()
        .flat_map(|p| p.accounts.iter().map(|a| a.id.clone()))
        .collect();

    match mode {
        Mode::Replace => {
            sum.providers_added = payload.providers.len();
            sum.accounts_added = payload
                .providers
                .iter()
                .map(|p| p.accounts.len())
                .sum::<usize>();
            sum.keys_added = payload.api_keys.len();
            data.providers = payload.providers;
            data.routing = payload.routing;
            data.patrol = payload.patrol;
            data.api_keys = payload.api_keys;
            // 换掉整份配置时，旧凭据的密文必须一起清，否则会留下再也用不到的
            // 明文副本——用户以为「覆盖导入了」，实际上旧 key 还躺在磁盘上。
            for id in &before {
                secrets::delete_credential(id);
            }
        }
        Mode::Merge => {
            for mut incoming_p in payload.providers {
                // 本机删过的厂商：合并时跳过。墓碑就是为了让「删掉」这个决定
                // 在跨机同步里活下来，否则下次同步又从另一台机器加回来
                if data.tombstones.is_buried(&incoming_p.id) {
                    sum.providers_skipped += 1;
                    sum.accounts_skipped += incoming_p.accounts.len();
                    continue;
                }
                match data.providers.iter_mut().find(|p| p.id == incoming_p.id) {
                    None => {
                        // 新厂商也要按凭据墓碑过滤：用户可能只删了某一条 key，
                        // 整包搬进来等于把那条 key 又写回密钥库
                        let before = incoming_p.accounts.len();
                        incoming_p
                            .accounts
                            .retain(|a| !data.tombstones.accounts.contains_key(&a.id));
                        sum.accounts_skipped += before - incoming_p.accounts.len();
                        sum.providers_added += 1;
                        sum.accounts_added += incoming_p.accounts.len();
                        data.providers.push(incoming_p);
                    }
                    Some(local) => {
                        sum.providers_kept += 1;
                        for a in std::mem::take(&mut incoming_p.accounts) {
                            if data.tombstones.accounts.contains_key(&a.id) {
                                sum.accounts_skipped += 1;
                            } else if local.accounts.iter().any(|x| x.id == a.id) {
                                sum.accounts_kept += 1;
                            } else {
                                sum.accounts_added += 1;
                                local.accounts.push(a);
                            }
                        }
                    }
                }
            }
            for k in payload.api_keys {
                // 同一个 key 的哈希相同 = 同一把 key，不需要再来一份
                if data.api_keys.iter().any(|x| x.id == k.id || x.key_hash == k.key_hash) {
                    sum.keys_kept += 1;
                } else {
                    sum.keys_added += 1;
                    data.api_keys.push(k);
                }
            }
            // 合并模式下选路/巡检是本地偏好，只有本地还是空的才接受对方的
            if data.routing.chain.is_empty() {
                data.routing = payload.routing;
            }
        }
    }

    let live: std::collections::HashSet<String> = data
        .providers
        .iter()
        .flat_map(|p| p.accounts.iter().map(|a| a.id.clone()))
        .collect();

    for (account_id, key) in incoming {
        if !live.contains(&account_id) {
            sum.secrets_orphaned += 1;
            continue;
        }
        if secrets::set_key(&account_id, &key).is_ok() {
            sum.secrets_written += 1;
        }
    }
    sum
}

/* ── 迁移包文件（.fpk） ─────────────────────────
   文件里装的就是迁移码原文。做成文件的理由不是格式，而是传输：
   一段 1–20KB 的 JSON 粘进微信/邮件会被截断，用户也没有「该放哪」的直觉。 */

/// 把迁移包写到文件。
///
/// 先写同目录临时文件、`sync_all` 之后再 rename：断电或拔盘时要么是完整文件，
/// 要么什么都没有——最坏的结果不是「保存失败」，是留下一个看起来能导入、
/// 实际解不开的半截文件。权限收到 `0600`：它虽然是密文，也不该让同机其他
/// 用户随手拷走。对话框已经问过覆盖，所以这里直接替换。
pub fn write_file(path: &Path, blob: &str) -> Result<(), String> {
    let Some(name) = path.file_name().filter(|n| !n.is_empty()) else {
        return Err("保存位置不合法：需要一个文件名".into());
    };
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !dir.is_dir() {
        return Err(format!("目录不存在：{}", dir.display()));
    }

    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("无法写入 {}：{e}", tmp.display()))?;
        f.write_all(blob.as_bytes())
            .map_err(|e| format!("写入失败：{e}"))?;
        f.sync_all().map_err(|e| format!("落盘失败：{e}"))?;
    }

    // Windows 上 rename 不覆盖已有文件；先删再改名
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("覆盖原文件失败：{e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存失败：{e}")
    })
}

/// 读迁移包。只做「读成字符串 + 大小闸门」，格式校验交给 [`open`]，
/// 这样错误信息仍然是那句「这不是一段 Freeport 迁移码」，而不是另一套说辞。
pub fn read_file(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读不到文件：{e}"))?;
    if !meta.is_file() {
        return Err("选中的不是一个文件".into());
    }
    if meta.len() == 0 {
        return Err("文件是空的".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "文件有 {} MB，不像迁移包（正常几十 KB）——可能选错了文件",
            meta.len() / 1024 / 1024
        ));
    }
    std::fs::read_to_string(path).map_err(|_| "这个文件不是文本，或者不是 Freeport 迁移包".to_string())
}

/// 只保留「换个地方仍然成立」的字段。
fn portable(p: &Provider) -> Provider {
    let mut c = p.clone();
    c.models.clear();
    c.health = Health::default();
    for a in c.accounts.iter_mut() {
        a.health = None;
    }
    c
}

fn check_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(format!(
            "口令至少 {MIN_PASSPHRASE} 位——迁移码里有明文密钥，短口令等于没加密"
        ));
    }
    Ok(())
}

/// AAD 把 KDF 参数和盐一起绑进密文：改任何一项都会解不开，
/// 而不是「改完还能解出一份被降级的口令派生」。
fn aad(env: &Envelope) -> Vec<u8> {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        env.format, env.v, env.kdf, env.m, env.t, env.p, env.salt, env.nonce
    )
    .into_bytes()
}

fn derive(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| format!("KDF 参数无效：{e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| format!("口令派生失败：{e}"))?;
    Ok(out)
}

fn seal(key: &[u8; 32], nonce: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .encrypt(
            Nonce::from_slice(nonce),
            AeadPayload {
                msg: plain,
                aad,
            },
        )
        .map_err(|_| "加密失败".to_string())
}

fn open_sealed(key: &[u8; 32], nonce: &[u8], aad: &[u8], ct: &[u8]) -> Result<Vec<u8>, ()> {
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce), AeadPayload { msg: ct, aad })
        .map_err(|_| ())
}
