//! 本地密钥库。
//!
//! 不依赖系统钥匙串：主密钥是本机生成的 32 字节随机数，落盘为 0600 的
//! `vault.key`；密文落 `secrets.json`。每条密钥用 AES-256-GCM 单独加密，
//! provider id 作为 AAD，避免条目被互换后仍能解开。
//!
//! 取舍：这是「防止明文落盘 + 防止误传」的本地保护，不是抗取证方案——
//! 能读到用户目录的人也能读到 `vault.key`。真正的边界是文件权限。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;

use freeport_auth::cookie::CookieJar;
use freeport_core::model::{Account, AppData};

use crate::store;

const VAULT_FILE: &str = "vault.key";
const SECRETS_FILE: &str = "secrets.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SecretsFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    items: BTreeMap<String, String>,
}

fn default_version() -> u32 {
    1
}

struct Vault {
    master: [u8; 32],
    items: BTreeMap<String, String>,
}

static VAULT: OnceLock<Mutex<Vault>> = OnceLock::new();

fn vault() -> &'static Mutex<Vault> {
    VAULT.get_or_init(|| Mutex::new(load()))
}

fn vault_path() -> PathBuf {
    store::data_dir().join(VAULT_FILE)
}

fn secrets_path() -> PathBuf {
    store::data_dir().join(SECRETS_FILE)
}

fn load() -> Vault {
    Vault {
        master: load_master(),
        items: load_items(),
    }
}

/// 首次运行时生成本机主密钥；权限固定 0600。
fn load_master() -> [u8; 32] {
    let path = vault_path();
    if let Ok(bytes) = fs::read(&path) {
        if bytes.len() == 32 {
            let mut master = [0u8; 32];
            master.copy_from_slice(&bytes);
            return master;
        }
    }

    let mut master = [0u8; 32];
    OsRng.fill_bytes(&mut master);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = write_private(&path) {
        if f.write_all(&master).is_ok() {
            tracing::info!("已生成本机主密钥 {}", path.display());
        }
    }
    master
}

fn load_items() -> BTreeMap<String, String> {
    fs::read_to_string(secrets_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<SecretsFile>(&raw).ok())
        .map(|f| f.items)
        .unwrap_or_default()
}

fn persist(items: &BTreeMap<String, String>) {
    let path = secrets_path();
    let body = SecretsFile {
        version: default_version(),
        items: items.clone(),
    };
    let Ok(raw) = serde_json::to_string_pretty(&body) else {
        return;
    };
    if let Ok(mut f) = write_private(&path) {
        let _ = f.write_all(raw.as_bytes());
    }
}

/// 以 0600 创建（或截断）文件。
fn write_private(path: &std::path::Path) -> std::io::Result<fs::File> {
    let file = fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

fn cipher(master: &[u8; 32]) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(master).expect("32 字节主密钥")
}

fn encrypt(master: &[u8; 32], provider_id: &str, plain: &str) -> Result<String, String> {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let sealed = cipher(master)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain.as_bytes(),
                aad: provider_id.as_bytes(),
            },
        )
        .map_err(|_| "加密失败".to_string())?;

    let mut blob = Vec::with_capacity(12 + sealed.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(B64.encode(blob))
}

fn decrypt(master: &[u8; 32], provider_id: &str, blob: &str) -> Option<String> {
    let raw = B64.decode(blob).ok()?;
    if raw.len() <= 12 {
        return None;
    }
    let (nonce, sealed) = raw.split_at(12);
    let plain = cipher(master)
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: sealed,
                aad: provider_id.as_bytes(),
            },
        )
        .ok()?;
    String::from_utf8(plain).ok()
}

pub fn set_key(provider_id: &str, key: &str) -> Result<(), String> {
    let mut v = vault().lock().map_err(|_| "密钥库被占用".to_string())?;
    let blob = encrypt(&v.master, provider_id, key)?;
    v.items.insert(provider_id.to_string(), blob);
    persist(&v.items);
    Ok(())
}

pub fn get_key(provider_id: &str) -> Option<String> {
    let v = vault().lock().ok()?;
    let blob = v.items.get(provider_id)?;
    decrypt(&v.master, provider_id, blob)
}

/// 本机主密钥的 hex 形态，**只**给自动备份当口令派生源（见 `backup.rs`）。
///
/// 边界与密钥库本体一致：快照加密后把这段内存里的字符串丢掉；
/// 它不落盘、不过 IPC、不打日志。快照与 vault.key 同生死——
/// 这正是「本机备份」的语义，跨机器迁移请走口令迁移码。
pub fn master_hex() -> Option<String> {
    let v = vault().lock().ok()?;
    Some(v.master.iter().map(|b| format!("{b:02x}")).collect())
}

/* ── OAuth 凭据 ──────────────────────────────────────
 * 和 API key 分开存：同一条凭据将来可能既有 key 又有 token，
 * 「退出登录」也只该清 token，不该把 key 一起删掉。
 * 槽位带 `#oauth` 后缀，AAD 同步带上，避免两类密文被互换。 */

const OAUTH_SUFFIX: &str = "#oauth";

fn oauth_slot(account_id: &str) -> String {
    format!("{account_id}{OAUTH_SUFFIX}")
}

pub fn set_oauth(account_id: &str, payload: &str) -> Result<(), String> {
    let slot = oauth_slot(account_id);
    let mut v = vault().lock().map_err(|_| "密钥库被占用".to_string())?;
    let blob = encrypt(&v.master, &slot, payload)?;
    v.items.insert(slot, blob);
    persist(&v.items);
    Ok(())
}

pub fn get_oauth(account_id: &str) -> Option<String> {
    let slot = oauth_slot(account_id);
    let v = vault().lock().ok()?;
    let blob = v.items.get(&slot)?;
    decrypt(&v.master, &slot, blob)
}

pub fn has_oauth(account_id: &str) -> bool {
    get_oauth(account_id).is_some()
}

/* ── Web 会话凭据（Cookie） ──────────────────────────
 * 只给「没有公开 API、只认浏览器会话」的厂商用。jar 整坨加密存，
 * AAD 带上槽位后缀，和 key / token 三类密文互不通用。
 *
 * 不自动续期是**产品决定**：会话刷新要靠真实浏览行为，程序去模拟就是
 * 在绕人家的风控。失效就如实报「请重新登录」。 */

const COOKIE_SUFFIX: &str = "#cookie";

fn cookie_slot(account_id: &str) -> String {
    format!("{account_id}{COOKIE_SUFFIX}")
}

pub fn set_cookie(account_id: &str, payload: &str) -> Result<(), String> {
    let slot = cookie_slot(account_id);
    let mut v = vault().lock().map_err(|_| "密钥库被占用".to_string())?;
    let blob = encrypt(&v.master, &slot, payload)?;
    v.items.insert(slot, blob);
    persist(&v.items);
    Ok(())
}

pub fn get_cookie(account_id: &str) -> Option<String> {
    let slot = cookie_slot(account_id);
    let v = vault().lock().ok()?;
    let blob = v.items.get(&slot)?;
    decrypt(&v.master, &slot, blob)
}

pub fn has_cookie(account_id: &str) -> bool {
    get_cookie(account_id).is_some()
}

pub fn delete_cookie(account_id: &str) {
    let Ok(mut v) = vault().lock() else { return };
    if v.items.remove(&cookie_slot(account_id)).is_some() {
        persist(&v.items);
    }
}

/* ── 许可证体系的本机秘密 ────────────────────────────
 * 设备密钥（激活时登记公钥、续签时自证身份）与时间高水位（防时钟回拨）。
 * 都挂在 `license#` 前缀下，AAD 绑定槽位名，与 provider key / oauth /
 * cookie 三类凭据互不通用。设备密钥丢了 = 续签过不了设备校验，
 * 重新激活即可（激活会登记新公钥）。 */

const LICENSE_DEVICE_SLOT: &str = "license#device";
const LICENSE_HWM_SLOT: &str = "license#hwm";

pub fn set_device_key(payload: &str) -> Result<(), String> {
    let mut v = vault().lock().map_err(|_| "密钥库被占用".to_string())?;
    let blob = encrypt(&v.master, LICENSE_DEVICE_SLOT, payload)?;
    v.items.insert(LICENSE_DEVICE_SLOT.to_string(), blob);
    persist(&v.items);
    Ok(())
}

pub fn get_device_key() -> Option<String> {
    let v = vault().lock().ok()?;
    let blob = v.items.get(LICENSE_DEVICE_SLOT)?;
    decrypt(&v.master, LICENSE_DEVICE_SLOT, blob)
}

pub fn set_license_hwm(ts: i64) -> Result<(), String> {
    let mut v = vault().lock().map_err(|_| "密钥库被占用".to_string())?;
    let blob = encrypt(&v.master, LICENSE_HWM_SLOT, &ts.to_string())?;
    v.items.insert(LICENSE_HWM_SLOT.to_string(), blob);
    persist(&v.items);
    Ok(())
}

pub fn get_license_hwm() -> Option<i64> {
    let v = vault().lock().ok()?;
    let blob = v.items.get(LICENSE_HWM_SLOT)?;
    decrypt(&v.master, LICENSE_HWM_SLOT, blob)?.parse().ok()
}

/// 这条凭据能用来发请求吗——API key、OAuth token、web 会话，有一个就算。
///
/// web 会话**全过期就不算**：会话不自动续，让它继续占着候选位只会把权重
/// 浪费在必然 401 的一跳上。界面上会显示成「需要重新登录」。
pub fn has_credential(account_id: &str) -> bool {
    if has_oauth(account_id) || get_key(account_id).is_some() {
        return true;
    }
    match get_cookie(account_id)
        .and_then(|raw| serde_json::from_str::<CookieJar>(&raw).ok())
    {
        Some(jar) => !jar.all_expired(chrono::Utc::now().timestamp()),
        None => false,
    }
}

/// 只清 token，保留同一凭据下的 API key。界面上「退出登录」走这里；
/// 删凭据才用 [`delete_credential`]。
pub fn delete_oauth(account_id: &str) {
    let Ok(mut v) = vault().lock() else { return };
    if v.items.remove(&oauth_slot(account_id)).is_some() {
        persist(&v.items);
    }
}

/// 删掉一条凭据的全部密文（key + token + web 会话）。删厂商/删凭据都走这里。
pub fn delete_credential(account_id: &str) {
    let Ok(mut v) = vault().lock() else { return };
    let mut changed = v.items.remove(account_id).is_some();
    changed |= v.items.remove(&oauth_slot(account_id)).is_some();
    changed |= v.items.remove(&cookie_slot(account_id)).is_some();
    if changed {
        persist(&v.items);
    }
}

/// 把「一厂商一 key」的老配置升级成「一厂商多凭据」。
///
/// 老凭据的 id **直接沿用 provider id**：密钥库里的条目不用搬家，也不用解密再加密，
/// 少一次「迁移中途崩了密钥全丢」的机会。
pub fn migrate_accounts(data: &mut AppData) -> bool {
    let mut changed = false;
    for p in data.providers.iter_mut() {
        if !p.accounts.is_empty() {
            continue;
        }
        // 既没有密文也没有掩码：这条 provider 本来就没接 key（匿名端点也可能有掩码）
        if get_key(&p.id).is_none() && p.key_mask.trim().is_empty() {
            continue;
        }
        p.accounts.push(Account {
            id: p.id.clone(),
            name: "default".into(),
            key_mask: p.key_mask.clone(),
            enabled: true,
            added_at: p.added_at.clone(),
            quota: p.quota.take(),
            health: None,
            purposes: Vec::new(),
        });
        changed = true;
    }
    changed
}

pub fn delete_key(provider_id: &str) {
    let Ok(mut v) = vault().lock() else { return };
    if v.items.remove(provider_id).is_some() {
        persist(&v.items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_dir<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        // 和 exts 的用例共用一把锁：两边改的是同一个进程级变量，
        // 模块私有的锁彼此看不见——不同步就会互相把目录改到别处
        let _env = crate::store::env_lock();
        let dir = std::env::temp_dir().join(format!("freeport-secrets-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        std::env::set_var("FREEPORT_DATA_DIR", &dir);
        let out = f();
        let _ = fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn roundtrip_and_delete() {
        with_temp_dir("roundtrip", || {
            set_key("groq", "sk-live-123").unwrap();
            assert_eq!(get_key("groq").as_deref(), Some("sk-live-123"));
            // 覆盖写
            set_key("groq", "sk-live-456").unwrap();
            assert_eq!(get_key("groq").as_deref(), Some("sk-live-456"));
            // 只影响指定条目
            assert!(get_key("cerebras").is_none());
            delete_key("groq");
            assert!(get_key("groq").is_none());
        });
    }

    #[test]
    fn ciphertext_is_bound_to_provider_id() {
        with_temp_dir("aad", || {
            let master = [7u8; 32];
            let blob = encrypt(&master, "groq", "sk-live-123").unwrap();
            assert_eq!(decrypt(&master, "groq", &blob).as_deref(), Some("sk-live-123"));
            assert!(decrypt(&master, "other", &blob).is_none());
        });
    }

    #[test]
    fn tampering_is_rejected() {
        with_temp_dir("tamper", || {
            let master = [9u8; 32];
            let mut blob = encrypt(&master, "groq", "sk-live-123").unwrap();
            // 改动最后一个 base64 字符
            let last = blob.pop().unwrap();
            blob.push(if last == 'A' { 'B' } else { 'A' });
            assert!(decrypt(&master, "groq", &blob).is_none());
        });
    }

    #[test]
    fn oauth_lives_beside_the_key_not_on_top_of_it() {
        with_temp_dir("oauth-slot", || {
            set_key("acct_1", "sk-live-123").unwrap();
            set_oauth("acct_1", r#"{"kind":"copilot"}"#).unwrap();

            // 两个槽位互不覆盖
            assert_eq!(get_key("acct_1").as_deref(), Some("sk-live-123"));
            assert_eq!(get_oauth("acct_1").as_deref(), Some(r#"{"kind":"copilot"}"#));
            assert!(has_credential("acct_1"));

            // 另一个凭据只拿到自己的那份
            assert!(get_oauth("acct_2").is_none());

            delete_credential("acct_1");
            assert!(get_key("acct_1").is_none());
            assert!(get_oauth("acct_1").is_none());
            assert!(!has_credential("acct_1"));
        });
    }

    #[test]
    fn oauth_only_account_still_counts_as_usable() {
        with_temp_dir("oauth-only", || {
            set_oauth("acct_gh", "{}").unwrap();
            assert!(has_credential("acct_gh"), "只有 token 的凭据也要能路由");
            assert!(get_key("acct_gh").is_none());
        });
    }
}
