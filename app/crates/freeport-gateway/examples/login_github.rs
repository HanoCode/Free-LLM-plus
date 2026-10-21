//! 真机验证：GitHub 设备码登录（Copilot）。
//!
//! 这不是测试替身——它连真实 github.com，写真实的数据目录。
//! 用途只有一个：在没人手点界面的时候，也能把这条链路走一遍。
//!
//! ```text
//! cargo run -p freeport-gateway --example login_github
//! ```

use std::time::{Duration, Instant};

use freeport_core::dto::ProviderInput;
use freeport_gateway::Gateway;

fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let gw = Gateway::new();
    gw.load().await;

    let has = gw.providers().await.iter().any(|p| p.id == "copilot");
    if !has {
        let r = gw
            .add_provider(ProviderInput {
                id: "copilot".into(),
                name: Some("GitHub Copilot".into()),
                base_url: "https://api.githubcopilot.com".into(),
                api_key: String::new(),
                tier: Some("C".into()),
            })
            .await?;
        println!(
            "[{}] 已接入厂商 {}（目录模型 {} 个），等待登录",
            now_hms(),
            r.provider.name,
            r.model_count
        );
    } else {
        println!("[{}] 厂商 copilot 已存在，直接发起登录", now_hms());
    }

    let start = gw.login_start("copilot").await?;
    println!();
    println!("  ┌──────────────────────────────────────────────");
    println!(
        "  │  浏览器打开：{}",
        start.verification_uri.clone().unwrap_or_default()
    );
    println!(
        "  │  输入代码：  {}",
        start.user_code.clone().unwrap_or_default()
    );
    println!("  │  有效期：    {} 秒", start.expires_in);
    println!("  └──────────────────────────────────────────────");
    if let Some(u) = &start.verification_uri_complete {
        println!("  （已尝试自动打开 {}）", u);
        let _ = std::process::Command::new("open").arg(u).spawn();
    }
    println!();

    let deadline = Instant::now() + Duration::from_secs(start.expires_in.max(60));
    let mut interval = start.interval.unwrap_or(5).max(5);

    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if Instant::now() >= deadline {
            return Err("设备码已过期（没人确认授权）".into());
        }

        let st = gw.login_poll(&start.login_id).await;
        match st.status.as_str() {
            "pending" => {
                if let Some(m) = &st.message {
                    println!("[{}] {}（下次间隔 {}s）", now_hms(), m, interval);
                    if m.contains("放慢") {
                        interval += 5;
                    }
                } else {
                    println!("[{}] 仍在等待授权……（每 {}s 查一次）", now_hms(), interval);
                }
            }
            "ready" => {
                println!(
                    "[{}] 授权成功：GitHub 账号 {}，凭据 {}",
                    now_hms(),
                    st.login.clone().unwrap_or_else(|| "?".into()),
                    st.account_id.clone().unwrap_or_else(|| "?".into())
                );
                break;
            }
            other => {
                return Err(format!(
                    "登录未完成：{other} {}",
                    st.message.unwrap_or_default()
                ))
            }
        }
    }

    // 落库之后回头看一眼真实状态：凭据形态、到期时间、模型列表
    let models: Vec<_> = gw
        .models()
        .await
        .into_iter()
        .filter(|m| {
            m.freeport
                .as_ref()
                .and_then(|f| f.provider.as_deref())
                .map(|p| p == "copilot")
                .unwrap_or(false)
        })
        .collect();

    for p in gw.providers().await {
        if p.id != "copilot" {
            continue;
        }
        println!();
        println!("  provider   {}", p.name);
        println!("  base_url   {}", p.base_url);
        for a in &p.accounts {
            println!(
                "  account    {} · {} · auth={} · hasKey={} · expiresAt={:?}",
                a.name,
                a.id,
                a.auth.clone().unwrap_or_else(|| "-".into()),
                a.has_key,
                a.expires_at
            );
        }
        println!("  models     {} 个", p.models);
        for m in models.iter().take(8) {
            println!("             - {}", m.id);
        }
        if models.len() > 8 {
            println!("             …另有 {} 个", models.len() - 8);
        }
    }

    Ok(())
}
