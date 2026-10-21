//! 内置扩展 manifest 的签名工具。只在 `--features issuer` 下编译。
//!
//! 用法：
//! ```text
//! cargo run -p freeport-ext --features issuer --example sign_builtin -- gen        # 生成密钥对并打印
//! cargo run -p freeport-ext --features issuer --example sign_builtin -- sign <file> # 原地补签名
//! ```
//!
//! 当前使用固定的开发期种子（写在本文件里，仅用于本地开发与测试）。
//! 正式发布前必须用 `rand_core::OsRng` 重新生成，并把新公钥写进
//! `src/sig.rs` 的 `BUILTIN_PUBKEY_HEX`——与 freeport-license 的
//! PROD_PUBKEY 同一条纪律。

use ed25519_dalek::{Signer, SigningKey};

/// 开发期固定种子。**不得用于生产**：私钥在源码里等于没有私钥，
/// 它存在的意义是让本地构建与测试能端到端跑通验签链路。
const DEV_SEED_HEX: &str = "3f7a1c9d0e5b48a29d63f1c7b04a58e29c6d17f3a8b2e45c90d7f6a3b1c8e254";

fn dev_key() -> SigningKey {
    let seed: [u8; 32] = hex::decode(DEV_SEED_HEX)
        .expect("dev seed hex")
        .try_into()
        .expect("dev seed len");
    SigningKey::from_bytes(&seed)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key = dev_key();
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            println!("pubkey(hex) = {}", hex::encode(key.verifying_key().as_bytes()));
            println!("dev seed    = {DEV_SEED_HEX}");
        }
        Some("sign") => {
            let path = args.get(2).expect("用法：sign <manifest.json>");
            let raw = std::fs::read_to_string(path).expect("读文件");
            let mut v: serde_json::Value = serde_json::from_str(&raw).expect("解析 JSON");
            // 与 ExtManifest::signed_bytes 同构：sig 字段整个去掉后的规范字节
            if let Some(obj) = v.as_object_mut() {
                obj.remove("sig");
            }
            let bytes = serde_json::to_vec(&v).expect("序列化");
            let sig = key.sign(&bytes);
            use base64::Engine;
            v["sig"] = serde_json::Value::String(
                base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
            );
            std::fs::write(path, serde_json::to_string_pretty(&v).unwrap() + "\n").expect("写文件");
            println!("已签名：{path}");
        }
        _ => {
            eprintln!("用法：sign_builtin <gen|sign <file>>");
            std::process::exit(2);
        }
    }
}
