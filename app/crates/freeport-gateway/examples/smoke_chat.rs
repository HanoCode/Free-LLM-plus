//! 真机冒烟：起网关，走一遍完整 HTTP 链路，打到真实上游。
//!
//! 和单测的区别：这里没有假上游。Key 校验、Gate、SWRR、账本、Copilot 的
//! 额外请求头、流式收尾，全都是真的。
//!
//! ```text
//! cargo run -p freeport-gateway --example smoke_chat -- [model]
//! ```

use freeport_gateway::providers;
use freeport_gateway::Gateway;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), String> {
    let model = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "gpt-4.1".to_string());

    let gw = Gateway::new();
    gw.load().await;
    let port = gw.start().await?;
    let status = gw.status().await;
    println!("网关已起在 127.0.0.1:{port}（requireAuth={}）", status.require_auth);

    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": "只回复两个字：通了" }],
        "max_tokens": 32,
        "stream": false
    });

    let t0 = std::time::Instant::now();
    let resp = providers::client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth(&status.auth_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;

    let code = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("\nHTTP {} · {} ms", code, t0.elapsed().as_millis());
    println!("{}", &text[..text.len().min(1200)]);

    if !code.is_success() {
        gw.stop().await;
        return Err(format!("上游没通（{code}）"));
    }

    // 日志里应能看到是哪条凭据答的
    for e in gw.logs().await.iter().rev().take(3) {
        println!(
            "\n  log  {} · {} · {} · {} ms",
            e.provider.clone().unwrap_or_else(|| "-".into()),
            e.model.clone().unwrap_or_else(|| "-".into()),
            e.status,
            e.ms
        );
        if let Some(a) = &e.account_name {
            println!("       凭据 {a}");
        }
        for a in &e.attempts {
            println!(
                "       attempt {} {} · {} · {} · {} ms",
                a.provider,
                a.model,
                a.account.clone().unwrap_or_else(|| "-".into()),
                a.status,
                a.ms
            );
        }
    }

    gw.stop().await;
    Ok(())
}
