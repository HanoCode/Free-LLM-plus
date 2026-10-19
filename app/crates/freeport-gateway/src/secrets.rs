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
}
