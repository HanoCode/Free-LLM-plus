//! 生成一对签发密钥。**只在本地跑**，不要把它接进 App 构建。
//!
//! ```bash
//! cargo run -p freeport-license --features issuer --example keygen
//! ```
//!
//! 输出两行：私钥（存进密码管理器，只给签发工具用）与公钥（替换
//! `src/lib.rs` 里的 `PROD_PUBKEY`）。

use freeport_license::issuer::Issuer;

fn main() {
    let iss = Issuer::generate();
    println!("secret_b64 = {}", iss.secret_b64());
    println!("public_b64 = {}", iss.public_b64());
}
