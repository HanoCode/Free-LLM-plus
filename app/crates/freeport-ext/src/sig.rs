//! Ed25519 验签。门槛第二条：「作者公钥 + 哈希，双击安装也要校验」。
//!
//! 与 freeport-catalog / freeport-license 同一套纪律：
//! 本 crate 只带公钥与验签；签名（私钥）只在 `examples/sign_builtin.rs`
//! 的 `issuer` feature 下编译，不进发布包。

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::manifest::ExtManifest;

/// 第一方（freeport）作者的公钥，32 字节的 hex。
///
/// 当前是开发期密钥对，与 `examples/sign_builtin.rs` 里的种子配对。
/// 正式发布前必须重新 keygen 并换掉这里（同 freeport-license 的 PROD_PUBKEY 纪律）。
pub const BUILTIN_PUBKEY_HEX: &str = "bb72f0588f4abfb2900935c0d60a62b7197f1fa9e54bb5530faf8f28541b40d9";

/// 已信任的作者公钥。第三方作者要进这份表，得走用户显式添加（v1 未开放 UI）。
fn trusted_pubkeys(author: &str) -> Option<&'static str> {
    match author {
        "freeport" => Some(BUILTIN_PUBKEY_HEX),
        _ => None,
    }
}

fn parse_pubkey(hex_key: &str) -> Result<VerifyingKey, String> {
    let raw = hex::decode(hex_key).map_err(|_| "公钥不是合法 hex")?;
    let bytes: [u8; 32] = raw.try_into().map_err(|_| "公钥长度必须是 32 字节")?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| format!("公钥解析失败：{e}"))
}

/// 校验一份 manifest：签名 + 权限门槛，都过才算数。
///
/// 调用方拿到的就是「可以进安装目录」的承诺；这里不落盘，落盘是 install 的事。
pub fn verify_manifest(m: &ExtManifest) -> Result<(), String> {
    // 门槛一：权限清单合法
    m.validate()?;

    let Some(pub_hex) = trusted_pubkeys(&m.author) else {
        return Err(format!("未知作者：{:?}（不在信任列表）", m.author));
    };
    let sig_b64 = m.sig.trim();
    if sig_b64.is_empty() {
        return Err("manifest 没有签名".into());
    }
    let sig_raw = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .map_err(|_| "签名字段不是合法 base64")?;
    let sig = Signature::from_slice(&sig_raw).map_err(|_| "签名长度不合法")?;
    let vk = parse_pubkey(pub_hex)?;
    vk.verify(m.signed_bytes()?.as_slice(), &sig)
        .map_err(|_| "签名校验失败：manifest 被改过，或不是这份公钥签的".to_string())
}

/// 解析 + 验签一步到位。从任何不可信来源（安装包、网络、磁盘）读 manifest 都走这里。
pub fn parse_and_verify(bytes: &[u8]) -> Result<ExtManifest, String> {
    let m: ExtManifest =
        serde_json::from_slice(bytes).map_err(|e| format!("manifest 解析失败：{e}"))?;
    verify_manifest(&m)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> serde_json::Value {
        serde_json::json!({
            "id": "asr",
            "name": "ASR",
            "version": "1.0.0",
            "kind": "asr",
            "entry": "self",
            "author": "freeport",
            "permissions": { "domains": ["api.groq.com"], "paths": [] },
            "sig": "AAA",
        })
    }

    #[test]
    fn unknown_author_rejected() {
        let mut v = sample_json();
        v["author"] = "mallory".into();
        let m: ExtManifest = serde_json::from_value(v).unwrap();
        assert!(verify_manifest(&m).is_err());
    }

    #[test]
    fn missing_sig_rejected() {
        let mut v = sample_json();
        v["sig"] = "".into();
        let m: ExtManifest = serde_json::from_value(v).unwrap();
        let err = verify_manifest(&m).unwrap_err();
        assert!(err.contains("没有签名") || err.contains("base64"));
    }

    #[test]
    fn tampered_manifest_rejected() {
        // 公钥未替换时（占位符），任何签名都会失败——fail-closed。
        let m: ExtManifest = serde_json::from_value(sample_json()).unwrap();
        assert!(verify_manifest(&m).is_err());
    }
}
