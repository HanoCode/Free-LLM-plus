//! Pro 许可证：存储、状态计算、激活。
//!
//! 这里刻意**不**做任何功能拦截。许可证只回答「现在是不是 Pro」，
//! 转发链路一个字节都不看它——产品红线是「失效只降级，不罢工」。
//! 真要限制能力，也该由服务端按 tier 决定返回什么数据，而不是在客户端
//! 写一串 `if !pro { return 403 }`（那既拦不住破解，又会误伤付费用户）。

use freeport_core::dto::GwLicense;
use freeport_core::model::{AppData, LicenseCfg};
use freeport_license as verifier;
use freeport_license::Status;

use crate::server::Gateway;
use crate::store;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn machine_of(data: &AppData) -> String {
    verifier::machine_id(&data.install_id)
}

fn to_dto(l: &verifier::License, machine: String) -> GwLicense {
    let claims = l.claims.as_ref();
    GwLicense {
        status: l.status_str().to_string(),
        tier: l.tier().to_string(),
        licensed_tier: l.licensed_tier().to_string(),
        pro: l.is_pro(),
        subject: claims.map(|c| c.sub.clone()),
        // 失效的 token 不下发能力清单：UI 少一处「看起来还能用」的误导
        features: if l.is_pro() {
            claims.map(|c| c.feat.clone()).unwrap_or_default()
        } else {
            Vec::new()
        },
        expires_at: l.expires_at,
        grace_until: l.grace_until,
        days_left: l.days_left,
        seats: claims.and_then(|c| c.seats),
        machine_id: machine,
        message: l.error.clone(),
    }
}

/// 按当前配置现算状态。没有缓存——缓存会让「把系统时间调回去」变成续期手段。
pub fn state(data: &AppData) -> GwLicense {
    let machine = machine_of(data);
    match data.license.token() {
        Some(token) => to_dto(&verifier::verify(token, now(), &machine), machine),
        None => to_dto(&verifier::License::missing(), machine),
    }
}

/// 数据面身份：本机是什么档 + 证明它用的 token。
///
/// 这里**只**产出一份声明，不判断任何功能能不能用。要不要多给数据，
/// 由服务端按这份声明决定（见 `feed.rs` 的 `x-freeport-tier` 与 `Authorization`）。
/// 转发链路不看它，所以许可证过期不会让任何一条请求失败。
pub struct Identity {
    /// `free` / `pro`
    pub tier: &'static str,
    /// 只有 Pro（含宽限）才有
    pub token: Option<String>,
}

pub fn identity(data: &AppData) -> Identity {
    let Some(token) = data.license.token() else {
        return Identity { tier: "free", token: None };
    };
    let l = verifier::verify(token, now(), &machine_of(data));
    if l.is_pro() {
        Identity { tier: "pro", token: Some(token.to_string()) }
    } else {
        Identity { tier: "free", token: None }
    }
}

impl Gateway {
    pub async fn license(&self) -> GwLicense {
        let d = self.st.data.read().await;
        state(&d)
    }

    /// 激活。**签名不对 / 绑了别的机器 / 已过宽限期**都直接拒绝并说明原因，
    /// 不落盘——否则配置里躺着一份无效 token，下次启动还是要再解释一遍。
    pub async fn activate_license(&self, token: &str) -> Result<GwLicense, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("请先粘贴许可证".into());
        }
        let mut d = self.st.data.write().await;
        let machine = machine_of(&d);
        let l = verifier::verify(token, now(), &machine);
        match l.status {
            Status::Valid | Status::Grace => {}
            Status::Expired => {
                return Err(format!(
                    "这份许可证已经过期（含 {} 天宽限期），请续费后再试",
                    freeport_license::GRACE_SECS / 86400
                ))
            }
            _ => {
                return Err(l
                    .error
                    .clone()
                    .unwrap_or_else(|| "许可证无法验证".to_string()))
            }
        }

        d.license = LicenseCfg {
            token: Some(token.to_string()),
        };
        store::save(&d);
        tracing::info!(
            target: "license",
            status = l.status_str(),
            subject = l.claims.as_ref().map(|c| c.sub.as_str()).unwrap_or("-"),
            "许可证已激活"
        );
        Ok(to_dto(&l, machine))
    }

    pub async fn clear_license(&self) -> GwLicense {
        let mut d = self.st.data.write().await;
        d.license = LicenseCfg::default();
        store::save(&d);
        state(&d)
    }
}
