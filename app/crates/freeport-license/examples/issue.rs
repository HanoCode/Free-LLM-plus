//! 签发一份许可证。签发端本地工具，不随 App 发布。
//!
//! ```bash
//! cargo run -p freeport-license --features issuer --example issue -- \
//!     --sub user@example.com --days 365 [--mid m_xxxx] [--tier pro]
//! ```
//!
//! 私钥从环境变量 `FREEPORT_LICENSE_KEY` 读；没有就退回 `app/.dev/license-issuer.key`。

use freeport_license::issuer::Issuer;
use freeport_license::Claims;

fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn main() {
    let secret = std::env::var("FREEPORT_LICENSE_KEY")
        .ok()
        .or_else(|| std::fs::read_to_string(".dev/license-issuer.key").ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            eprintln!("缺少私钥：设置 FREEPORT_LICENSE_KEY 或写入 .dev/license-issuer.key");
            std::process::exit(2);
        });

    let iss = Issuer::from_secret_b64(&secret).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });

    let sub = arg("--sub").unwrap_or_else(|| "dev@local".into());
    let days: i64 = arg("--days")
        .and_then(|v| v.parse().ok())
        .unwrap_or(365);
    let tier = arg("--tier").unwrap_or_else(|| "pro".into());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut claims = Claims::pro(&sub, days, now);
    claims.tier = tier;
    claims.mid = arg("--mid");
    claims.seats = arg("--seats").and_then(|v| v.parse().ok());

    println!("{}", iss.sign(&claims));
}
