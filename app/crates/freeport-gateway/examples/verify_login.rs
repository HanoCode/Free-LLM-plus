//! 真机验证：登录之后到底成了没成。
//!
//! 三段检查，全部打真实接口、读真实文件：
//! 1. 账号与凭据形态（`secrets.json` 的 `#oauth` 槽位）
//! 2. 用换来的 Copilot token 拉一次模型列表
//! 3. 把 `expiresAt` 拨到过去，看 `Broker` 会不会自己续期
//!
//! ```text
//! cargo run -p freeport-gateway --example verify_login
//! ```

use freeport_gateway::tokens::Broker;
use freeport_gateway::Gateway;

fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn mask_token(t: &str) -> String {
    let head: String = t.chars().take(6).collect();
    format!("{head}…（{} 字符）", t.chars().count())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let gw = Gateway::new();
    gw.load().await;

    /* 1. 配置里的账号 */
    let providers = gw.providers().await;
    let copilot = providers
        .iter()
        .find(|p| p.id == "copilot")
        .ok_or("配置里没有 copilot 厂商——登录没落库")?;

    println!("[{}] provider {} · {} 个模型", now_hms(), copilot.name, copilot.models);
    println!("         base_url = {}", copilot.base_url);
    let acc = copilot
        .accounts
        .first()
        .ok_or("copilot 下没有凭据")?
        .clone();
    println!(
        "         account  = {} · {} · auth={} · hasKey={} · expiresAt={:?}",
        acc.name,
        acc.id,
        acc.auth.clone().unwrap_or_else(|| "-".into()),
        acc.has_key,
        acc.expires_at
    );

    let blob = Broker::load(&acc.id).ok_or("密钥库里读不到 OAuth blob")?;
    println!(
        "         blob     = kind={} login={:?} access={} refresh={}",
        blob.kind,
        blob.login,
        mask_token(&blob.tokens.access_token),
        blob.tokens
            .refresh_token
            .as_deref()
            .map(mask_token)
            .unwrap_or_else(|| "（无）".into())
    );

    /* 2. 真实模型列表 */
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
    println!("\n[{}] /v1/models 里 copilot 相关 {} 条（含别名行）", now_hms(), models.len());
    for m in models.iter().take(6) {
        println!(
            "         - {:<34} ctx={:?}",
            m.id,
            m.context_length
        );
    }

    /* 3. 强制过期，看会不会续期 */
    println!("\n[{}] 把 expiresAt 拨到 60 秒前，触发续期……", now_hms());
    let before = blob.tokens.access_token.clone();
    let mut expired = blob.clone();
    expired.tokens.expires_at = Some(chrono::Utc::now().timestamp() - 60);
    Broker::save(&acc.id, &expired)?;

    let cred = gw.credential(&acc.id).await.ok_or("续期失败：拿不到凭据")?;
    let header = cred
        .header(&copilot.base_url)
        .ok_or("续期后没有可发的认证头")?;
    println!("         认证头   = {}", mask_token(header.trim_start_matches("Bearer ")));

    let after = Broker::load(&acc.id).ok_or("续期后读不到 blob")?;
    println!(
        "         新 token {} · expiresAt={:?}",
        if after.tokens.access_token != before {
            "与旧值不同 ✓"
        } else {
            "与旧值相同 ✗（没真续期）"
        },
        after.tokens.expires_at
    );
    if after.tokens.expires_at.unwrap_or(0) <= chrono::Utc::now().timestamp() {
        return Err("续期后 expiresAt 仍在过去".into());
    }

    /* 4. 用新 token 验一次活 */
    let probe = freeport_gateway::providers::list_models(
        &freeport_gateway::providers::client(),
        &freeport_core::model::Provider {
            id: copilot.id.clone(),
            name: copilot.name.clone(),
            base_url: copilot.base_url.clone(),
            tier: copilot.tier.clone(),
            homepage: None,
            note: None,
            key_mask: String::new(),
            enabled: true,
            added_at: String::new(),
            models: Vec::new(),
            quota: None,
            accounts: Vec::new(),
            health: Default::default(),
        },
        &cred,
    )
    .await;
    match probe {
        Ok(m) => println!("\n[{}] 续期后的 token 可用：拉到 {} 个模型", now_hms(), m.len()),
        Err(e) => println!(
            "\n[{}] 续期后的 token 拉模型失败：status={} {}",
            now_hms(),
            e.status,
            e.body.chars().take(160).collect::<String>()
        ),
    }

    Ok(())
}
