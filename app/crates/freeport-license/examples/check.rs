//! 校验一份许可证（支持工具，不需要私钥）。
//!
//! ```bash
//! cargo run -p freeport-license --example check -- "<token>"
//! echo "<token>" | cargo run -q -p freeport-license --example check
//! ```
//!
//! 用户报「激活不了」时，先用它确认是 token 的问题还是构建的问题：
//! `status=invalid / 公钥不匹配` 说明内置公钥与签发私钥不是一对。

use std::io::Read;

fn main() {
    let token = std::env::args().nth(1).unwrap_or_else(|| {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).ok();
        s
    });

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // 不传安装码：绑机器的 license 也能在这里看状态
    let l = freeport_license::verify(&token, now, "");
    println!("status      = {}", l.status_str());
    println!("tier        = {}", l.tier());
    println!("licensed    = {}", l.licensed_tier());
    println!("subject     = {}", l.claims.as_ref().map(|c| c.sub.as_str()).unwrap_or("-"));
    println!("expires_at  = {:?}", l.expires_at);
    println!("days_left   = {:?}", l.days_left);
    println!("features    = {:?}", l.claims.as_ref().map(|c| c.feat.clone()).unwrap_or_default());
    if let Some(e) = l.error {
        println!("message     = {e}");
    }
}
