//! 把密钥库里「有 token、没账号条目」的 OAuth 凭据挂回去。
//!
//! 什么时候会用到：配置被回滚、迁移包只带了密文、或者上次保存没替换成功。
//! 这时 `secrets.json` 里 `acct_x#oauth` 还在，但 `freeport.json` 里没有对应
//! 的 provider/account —— 界面看不到，额度也永远不会被用上。
//!
//! ```text
//! cargo run -p freeport-gateway --example repair_oauth -- <provider_id> <account_id>
//! ```

use freeport_core::model::{Account, Health, HealthStatus, Provider};
use freeport_gateway::cred::Credential;
use freeport_gateway::providers;
use freeport_gateway::store;
use freeport_gateway::tokens::Broker;

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (provider_id, account_id) = match args.as_slice() {
        [p, a] => (p.clone(), a.clone()),
        _ => return Err("用法：repair_oauth <provider_id> <account_id>".into()),
    };

    let blob = Broker::load(&account_id)
        .ok_or_else(|| format!("密钥库里没有 {account_id} 的 OAuth 凭据"))?;
    println!("找到凭据：kind={} login={:?}", blob.kind, blob.login);

    let mut data = store::load();
    let base = if provider_id == "copilot" {
        freeport_auth::copilot::API_BASE.to_string()
    } else {
        return Err(format!("暂不支持 {provider_id}，base_url 需要显式指定"));
    };

    let idx = match data.providers.iter().position(|p| p.id == provider_id) {
        Some(i) => i,
        None => {
            data.providers.push(Provider {
                id: provider_id.clone(),
                name: "GitHub Copilot".into(),
                base_url: base.clone(),
                tier: "C".into(),
                homepage: Some("https://github.com/features/copilot".into()),
                note: Some("用自己的 GitHub 账号登录，免填 key".into()),
                key_mask: "gh-oauth".into(),
                enabled: true,
                added_at: chrono::Utc::now().to_rfc3339(),
                proxy: None,
                wire: None,
                models: Vec::new(),
                quota: None,
                accounts: Vec::new(),
                health: Health {
                    status: HealthStatus::Probing,
                    ..Default::default()
                },
            });
            data.providers.len() - 1
        }
    };

    if data.providers[idx].accounts.iter().any(|a| a.id == account_id) {
        println!("账号条目已存在，无需修复");
        return Ok(());
    }

    // 顺手用真 token 拉一次模型：能拉到才说明这条凭据还活着
    let probe = Provider {
        base_url: base.clone(),
        ..data.providers[idx].clone()
    };
    let models = providers::list_models(
        &providers::client(),
        &probe,
        &Credential::bearer(blob.tokens.access_token.clone()),
    )
    .await;
    let (models, label) = match &models {
        Ok(m) => (m.clone(), format!("可用，{} 个模型", m.len())),
        Err(e) => (Vec::new(), format!("拉模型失败（{}）", e.status)),
    };
    println!("凭据状态：{label}");

    let acc = Account {
        id: account_id.clone(),
        name: blob.login.clone().unwrap_or_else(|| "github".into()),
        key_mask: "gh-oauth".into(),
        enabled: true,
        added_at: chrono::Utc::now().to_rfc3339(),
        quota: None,
        health: Some(Health {
            status: if models.is_empty() {
                HealthStatus::Degraded
            } else {
                HealthStatus::Healthy
            },
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
            model_count: Some(models.len()),
            ..Default::default()
        }),
        purposes: Vec::new(),
    };

    let p = &mut data.providers[idx];
    p.accounts.push(acc);
    if !models.is_empty() {
        p.models = models;
        p.health = Health {
            status: HealthStatus::Healthy,
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
            model_count: Some(p.models.len()),
            ..Default::default()
        };
    }
    if p.base_url.trim().is_empty() {
        p.base_url = base;
    }

    if !store::save(&data) {
        return Err("写配置失败，看日志".into());
    }
    println!(
        "已挂回：{} / {} · {} 个模型",
        provider_id,
        account_id,
        data.providers[idx].models.len()
    );
    Ok(())
}
