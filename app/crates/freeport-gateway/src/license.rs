//! Pro 许可证：存储、状态计算、激活。
//!
//! 这里刻意**不**做任何功能拦截。许可证只回答「现在是不是 Pro」，
//! 转发链路一个字节都不看它——产品红线是「失效只降级，不罢工」。
//! 真要限制能力，也该由服务端按 tier 决定返回什么数据，而不是在客户端
//! 写一串 `if !pro { return 403 }`（那既拦不住破解，又会误伤付费用户）。
//!
//! 激活有两条路（v1.11 起）：
//! - **在线激活**（默认）：用户粘贴激活码，本机向 license 服务器换一份
//!   短时 JWT（72h），存下来后验签、续签全走本地——离线宽限、降级语义
//!   与旧路径完全一致。防逆向的关键不在客户端校验，而在 token 短：
//!   被拷走也只在 TTL 内有效，吊销最迟 TTL+宽限 到位。
//! - **离线 JWT**（兼容）：直接粘贴签发工具出的三段式 token，行为同旧版。
//!   两个入口最终都落到同一个 `LicenseCfg.token`，后续链路零分叉。

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use freeport_core::dto::{GwDevice, GwLicense, GwLicenseDevices, GwSubscribeStart, GwSubscribeState};
use freeport_core::model::{AppData, LicenseCfg};
use freeport_license as verifier;
use freeport_license::Status;

use crate::server::Gateway;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 离线激活通道允许的最大有效期。约 13 个月：覆盖「年度订阅 + 停产前
/// 最后一次离线签发」的合理场景，同时保证泄露的离线 token 终会过期。
const OFFLINE_MAX_SECS: i64 = 400 * 86400;

fn machine_of(data: &AppData) -> String {
    verifier::machine_id(&data.install_id)
}

/* ── 设备密钥（Ed25519）──────────────────────────────────
 * 机器指纹（install_id + 主机名哈希）是可克隆的：拷两个值到第二台
 * 机器就是同一个「设备」，席位限制形同虚设。设备密钥不可克隆——
 * 私钥只在本机密钥库（AES-256-GCM，见 secrets.rs），激活时把公钥
 * 登记到服务器，续签时对服务器下发的 nonce 签名自证身份。
 * 密钥库被清（重装系统/删数据目录）= 丢密钥，重新激活即可：
 * 激活会登记新公钥，顶掉旧记录。 */

fn device_key() -> Option<SigningKey> {
    let seed = crate::secrets::get_device_key()?;
    let raw = B64URL.decode(seed.trim()).ok()?;
    let arr: [u8; 32] = raw.as_slice().try_into().ok()?;
    SigningKey::from_bytes(&arr).into()
}

/// 取（或首次生成）设备密钥，返回公钥 base64url。生成失败不阻断激活：
/// 老服务器不认 pubkey 字段，密钥校验是增量加固。
fn ensure_device_pubkey() -> Option<String> {
    let key = match device_key() {
        Some(k) => k,
        None => {
            let k = SigningKey::generate(&mut rand::rngs::OsRng);
            let seed = B64URL.encode(k.to_bytes());
            if crate::secrets::set_device_key(&seed).is_err() {
                tracing::warn!(target: "license", "设备密钥写入密钥库失败，本台设备将无法通过续签校验");
                return None;
            }
            tracing::info!(target: "license", "已生成本机设备密钥");
            k
        }
    };
    Some(B64URL.encode(key.verifying_key().to_bytes()))
}

/// 对服务器 nonce 签名。密钥不存在（密钥库被清）返回 None——
/// 续签请求会不带签名发出，服务器按「设备校验不过」处理。
fn device_sign(msg: &[u8]) -> Option<String> {
    let key = device_key()?;
    Some(B64URL.encode(key.sign(msg).to_bytes()))
}

/// license 服务器地址。默认官方端点，env 覆盖给自托管/测试用。
fn license_api() -> String {
    std::env::var("FREEPORT_LICENSE_API")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://license.freeport.app".into())
}

/// 回环地址判定（容忍 `http://127.0.0.1:8790` / `localhost:8790` / `[::1]:8790`）。
fn is_loopback_base(base: &str) -> bool {
    let after = base.split("://").nth(1).unwrap_or(base);
    let hostport = after.split('/').next().unwrap_or("");
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next() == Some("::1");
    }
    let host = hostport.split(':').next().unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// 回环直连客户端。本机 dev license 服务器绝不能被环境/系统代理截走：
/// 代理连不上目标时回的是纯文本错误页，客户端只会看到一句
/// 「响应解析失败」，排查方向直接被带偏。`providers::client()` 的
/// no_proxy 白名单只覆盖它自己拼的显式代理，系统代理（macOS
/// SystemConfiguration）不在此列——回环干脆整体绕开。
fn loopback_client() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .build()
            .expect("reqwest client")
    })
    .clone()
}

/// license api 的错误：`code` 是服务端给的机读码，`msg` 是网络层原文。
struct ApiErr {
    code: Option<String>,
    msg: String,
}

impl ApiErr {
    /// 机读码 → 用户能看懂的话。网络失败单列——「连不上」和「码不对」
    /// 的处置完全不同，混成一句「激活失败」等于把排查线索吞掉。
    fn humanize(&self) -> String {
        match self.code.as_deref() {
            Some("unknown_key") => "激活码不存在，请核对是否输入完整".into(),
            Some("revoked") => "该激活码已被吊销，如有疑问请联系支持".into(),
            Some("subscription_expired") => "订阅已到期，请续费后重试".into(),
            Some("seat_limit") => "设备数已达上限，请先在其他设备上解除激活".into(),
            Some("machine_mismatch") => "许可证绑定的是另一台设备".into(),
            Some("device_proof") => "设备校验未通过：本机设备密钥与激活记录不符。重装或清理过数据目录时，重新输入激活码即可".into(),
            Some("device_moved") => "检测到许可证在多台设备间漂移，已暂停续签；如是本人在多地使用，请联系支持".into(),
            Some("invalid_token") => "许可证无效或已彻底过期，请重新输入激活码".into(),
            Some("rate_limit") => "尝试太频繁，请稍后再试".into(),
            Some("bad_endpoint") => "激活服务器返回的不是 JSON：多半是 FREEPORT_LICENSE_API 指错了端点（如占位域名/官网首页）".into(),
            Some("bad_request") => "激活码或设备标识不完整".into(),
            None => format!("连不上激活服务器：{}。请检查网络后重试", self.msg),
            Some(other) => format!("激活失败（{other}）"),
        }
    }
}

async fn license_post(path: &str, token: Option<&str>, body: serde_json::Value) -> Result<serde_json::Value, ApiErr> {
    let base = license_api();
    let url = format!("{}{path}", base);
    // 回环地址直连，其余照用环境代理（自托管公网端点可能真的需要代理可达）
    let http = if is_loopback_base(&base) {
        loopback_client()
    } else {
        crate::providers::client()
    };
    let mut req = http
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| ApiErr {
        code: None,
        msg: e.to_string(),
    })?;
    // 服务器时钟是唯一不受本机控制的锚：每次往返都喂进时间高水位。
    // 吊销/到期后哪怕一次在线往返，都会把高水位推过旧 token 的 exp，
    // 此后回拨系统时钟再也救不回它（见 hwm.rs）。
    if let Some(d) = resp.headers().get(reqwest::header::DATE).and_then(|v| v.to_str().ok()) {
        crate::hwm::note_http_date(d);
    }
    let status = resp.status();
    // 端点指错时（如域名 301 到官网首页）回来的是 HTML/纯文本，硬解析
    // 只会留下一句「响应解析失败」，把「端点不对」误报成「网络不通」
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ctype.contains("json") {
        return Err(ApiErr {
            code: Some("bad_endpoint".into()),
            msg: format!("{base}{path} 未返回 JSON（content-type: {ctype}）"),
        });
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| ApiErr {
        code: None,
        msg: format!("响应解析失败：{e}"),
    })?;
    if !status.is_success() {
        return Err(ApiErr {
            code: json.get("error").and_then(|v| v.as_str()).map(str::to_string),
            msg: json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误")
                .to_string(),
        });
    }
    Ok(json)
}

/// 判断是不是三段式离线 JWT。激活码是短的不透明串（`FP-` 开头），
/// JWT 至少两个点、上百字符——用一个宽松的形状判断分流即可。
fn looks_like_jwt(s: &str) -> bool {
    s.matches('.').count() == 2 && s.len() >= 64 && !s.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::is_loopback_base;

    #[test]
    fn loopback_shapes() {
        assert!(is_loopback_base("http://127.0.0.1:8790"));
        assert!(is_loopback_base("http://localhost:8790"));
        assert!(is_loopback_base("http://[::1]:8790"));
        assert!(is_loopback_base("127.0.0.1:8790"));
        assert!(!is_loopback_base("https://license.freeport.app"));
        assert!(!is_loopback_base("http://127.0.0.1.evil.com:8790"));
    }
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
/// 系统时钟比本机高水位早超过容差 = 被回拨：直接判无效（时间冻结的
/// token 永远不会过期，钳制等于没防）。只影响许可证，不影响转发。
pub fn state(data: &AppData) -> GwLicense {
    // 免费开放时代：不做许可证校验，以 Pro 形态示人。`status=free_era`
    // 是 UI 的显示钩子（区别于真实激活的 `valid`），激活/订阅端点保持
    // 可用——将来翻回收费，这里删掉这个分支就恢复原语义。
    if crate::pro::FREE_FOR_ALL {
        return GwLicense {
            status: "free_era".to_string(),
            tier: "pro".to_string(),
            licensed_tier: "free".to_string(),
            pro: true,
            subject: None,
            features: Vec::new(),
            expires_at: None,
            grace_until: None,
            days_left: None,
            seats: None,
            machine_id: machine_of(data),
            message: None,
        };
    }
    let machine = machine_of(data);
    if crate::hwm::rolled_back() {
        return to_dto(
            &verifier::License::invalid("系统时钟被回拨：请校正系统时间后重试"),
            machine,
        );
    }
    match data.license.token() {
        Some(token) => to_dto(&verifier::verify(token, now(), &machine), machine),
        None => to_dto(&verifier::License::missing(), machine),
    }
}

/// 功能门禁用：`feature` 是 token 能力清单里的能力名（见 `pro.rs`）。
/// pro 之外还要清单里真的带这份能力——patch 掉 is_pro 的破解拿不到
/// claims，什么功能都解不开。旧清单通配与细粒度判断共用
/// `verifier::feat_grants`，和 `License::grants` 一个口径。
pub fn grants(data: &AppData, feature: &str) -> bool {
    if crate::pro::FREE_FOR_ALL {
        return true;
    }
    let l = state(data);
    l.pro && verifier::feat_grants(&l.features, feature)
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
    // 时钟被回拨的机器不给 pro 身份：喂给服务端的声明必须是可信时间下的
    if crate::hwm::rolled_back() {
        return Identity { tier: "free", token: None };
    }
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

    /// 激活。两条路：
    /// - 在线：激活码 → license 服务器换短时 token，落盘的是服务器签的 JWT。
    /// - 离线（兼容旧版）：直接粘贴 JWT，本地验签。
    ///
    /// **签名不对 / 绑了别的机器 / 已过宽限期 / 服务器拒绝**都直接拒绝并说明
    /// 原因，不落盘——否则配置里躺着一份无效 token，下次启动还是要再解释一遍。
    pub async fn activate_license(&self, key: &str) -> Result<GwLicense, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("请先输入激活码".into());
        }
        // 时钟被回拨时不激活：这份 token 的有效期在可信时间下还没核实过，
        // 落盘等于替回拨者预存一份「未来可用」的许可证。
        if crate::hwm::rolled_back() {
            return Err("系统时钟被回拨：请校正系统时间后再激活".into());
        }
        if looks_like_jwt(key) {
            return self.activate_offline(key).await;
        }
        // 激活码的形状是我们自己定的：`FP-` 开头。不认识的形状当场报
        // 格式错，不发给服务器——把明显的垃圾发出去只会换来一句
        // 「连不上」，把排查方向带偏。
        if key.len() >= 8 && key.get(..2).map(|p| p.eq_ignore_ascii_case("FP")).unwrap_or(false) {
            return self.activate_online(key).await;
        }
        Err("激活码格式不对：应形如 FP-XXXX-XXXX-XXXX，或粘贴完整的许可证 token".into())
    }

    /// 在线激活。写锁**不跨 await**：HTTP 往返在锁外完成，拿到 token 再进
    /// 锁落盘——写锁里等一个 15s 的网络请求等于把整个网关配置面卡死。
    async fn activate_online(&self, key: &str) -> Result<GwLicense, String> {
        let machine = {
            let d = self.st.data.read().await;
            machine_of(&d)
        };
        // 设备公钥随激活上报登记：此后续签必须由本机私钥对服务器 nonce
        // 签名自证。指纹可以克隆，私钥不能。
        let pubkey = ensure_device_pubkey();
        let mut body = serde_json::json!({ "key": key, "machineId": machine });
        if let Some(pk) = pubkey {
            body["pubkey"] = serde_json::json!(pk);
        }
        let resp = license_post("/v1/license/activate", None, body)
            .await
            .map_err(|e| e.humanize())?;
        let Some(token) = resp.get("token").and_then(|v| v.as_str()) else {
            return Err("服务器响应缺少许可证内容".into());
        };
        let token = token.to_string();

        // 服务器签的 token 也要过一遍本地验签才算数：公钥对不上（比如
        // env 指到了别人的服务器）就当场拒绝，不留一份验不过的配置。
        let l = verifier::verify(&token, now(), &machine);
        match l.status {
            Status::Valid | Status::Grace => {}
            _ => {
                return Err(l
                    .error
                    .clone()
                    .unwrap_or_else(|| "服务器签发的许可证无法通过本地验签".to_string()))
            }
        }

        let mut d = self.st.data.write().await;
        d.license = LicenseCfg { token: Some(token) };
        crate::server::save_here(&self.st, &d);
        tracing::info!(
            target: "license",
            status = l.status_str(),
            subject = l.claims.as_ref().map(|c| c.sub.as_str()).unwrap_or("-"),
            "许可证已激活（在线签发）"
        );
        Ok(to_dto(&l, machine))
    }

    /// 离线 JWT 激活——v1.10 及之前的原始路径。v1.14 起加有效期上限：
    /// 离线通道没有服务器兜底（无吊销、无续签），一份被泄露或转手的
    /// 长期 token 永远杀不死。终身档等长周期授权一律走在线激活——
    /// 终身 = 订阅永不到期，token 仍 72h 一换，吊销照样到位。
    async fn activate_offline(&self, token: &str) -> Result<GwLicense, String> {
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
        if let Some(c) = &l.claims {
            if c.exp.saturating_sub(c.iat) > OFFLINE_MAX_SECS {
                return Err(format!(
                    "有效期超过 {} 天的许可证不支持离线激活，请使用在线激活（输入 FP- 开头的激活码）",
                    OFFLINE_MAX_SECS / 86400
                ));
            }
        }

        d.license = LicenseCfg {
            token: Some(token.to_string()),
        };
        crate::server::save_here(&self.st, &d);
        tracing::info!(
            target: "license",
            status = l.status_str(),
            subject = l.claims.as_ref().map(|c| c.sub.as_str()).unwrap_or("-"),
            "许可证已激活（离线验签）"
        );
        Ok(to_dto(&l, machine))
    }

    /// 到期前续签。拿到新 token 换旧的，失败就什么都不动——旧 token
    /// 顶到过期为止，宽限语义不变。**绝不让续签失败影响任何请求。**
    /// `force` 跳过「离到期还早」的余量判断（手动续签与测试用）。
    pub async fn refresh_once(&self, force: bool) {
        let (machine, token, expires_at, pro) = {
            let d = self.st.data.read().await;
            let machine = machine_of(&d);
            match d.license.token() {
                Some(t) => {
                    let l = verifier::verify(t, now(), &machine);
                    let pro = l.is_pro();
                    (machine, t.to_string(), l.expires_at, pro)
                }
                None => return,
            }
        };
        if !pro {
            return;
        }
        // 还早就不打扰服务器：剩余超过一天半的不续
        let Some(exp) = expires_at else { return };
        if !force && exp - now() > 36 * 3600 {
            return;
        }
        let mut body = serde_json::json!({ "machineId": machine });
        // 设备自证：先领服务器 nonce，再拿本机私钥签名。老服务器没有
        // 挑战端点 → 拿不到 nonce 就照旧裸续签；已登记公钥的新服务器
        // 会拒掉裸续签，客户端给出「重新激活」的人话。
        match license_post(
            "/v1/license/challenge",
            Some(&token),
            serde_json::json!({ "machineId": machine }),
        )
        .await
        {
            Ok(ch) => {
                let nonce = ch.get("nonce").and_then(|v| v.as_str()).map(str::to_string);
                let sig = nonce.as_deref().and_then(|n| device_sign(n.as_bytes()));
                match (nonce, sig) {
                    (Some(n), Some(s)) => {
                        body["nonce"] = serde_json::json!(n);
                        body["sig"] = serde_json::json!(s);
                    }
                    (Some(_), None) => {
                        tracing::warn!(target: "license", "本机设备密钥缺失，若服务器已登记公钥则本次续签会被拒绝")
                    }
                    _ => {}
                }
            }
            Err(e) => {
                tracing::debug!(target: "license", code = e.code.as_deref().unwrap_or("-"), "服务器无挑战端点或暂时不可达，按旧协议续签")
            }
        }
        match license_post("/v1/license/refresh", Some(&token), body).await {
            Ok(resp) => {
                if let Some(new_token) = resp.get("token").and_then(|v| v.as_str()) {
                    let l = verifier::verify(new_token, now(), &machine);
                    if l.is_pro() {
                        let mut d = self.st.data.write().await;
                        d.license = LicenseCfg {
                            token: Some(new_token.to_string()),
                        };
                        crate::server::save_here(&self.st, &d);
                        tracing::info!(target: "license", "许可证已续签");
                    }
                }
            }
            // 服务器明确说订阅没了/被吊销：也什么都不做。本地 token 顶到
            // 自然过期再降级——「失效只降级」由时间窗保证，不需要立刻撕票。
            Err(e) => {
                tracing::warn!(target: "license", code = e.code.as_deref().unwrap_or("-"), "续签未成功，沿用现有许可证");
            }
        }
    }

    /// 周期续签循环。随网关 start 拉起；每 6 小时醒一次，真正发请求前
    /// 还有过期余量判断，所以绝大多数醒来只是看一眼就睡。
    pub fn spawn_license_refresh(&self) {
        // 后台任务只需要 State_，构造一个同源的轻量句柄即可
        let gw = Gateway { st: self.st.clone() };
        tokio::spawn(async move {
            // 启动先等一会儿，别和网关自身初始化抢网络
            tokio::time::sleep(std::time::Duration::from_secs(45)).await;
            loop {
                if !gw.st.running.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                // 每次醒来先把高水位顶到当前时间：这台机器最近一次真实
                // 在场时刻。回拨检测用的锚就靠它和服务器 Date 头。
                crate::hwm::note_now();
                gw.refresh_once(false).await;
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
    }

    pub async fn clear_license(&self) -> GwLicense {
        // 顺手通知服务器解绑本机，腾出席位。fire-and-forget：本地清理
        // 不等它，服务器连不上就留着这条绑定（席位由 deactivate 补）。
        let (machine, token) = {
            let d = self.st.data.read().await;
            (machine_of(&d), d.license.token().map(str::to_string))
        };
        if let Some(token) = token {
            let body = serde_json::json!({ "machineId": machine });
            tokio::spawn(async move {
                let _ = license_post("/v1/license/deactivate", Some(&token), body).await;
            });
        }
        let mut d = self.st.data.write().await;
        d.license = LicenseCfg::default();
        crate::server::save_here(&self.st, &d);
        state(&d)
    }
}

/* ── 订阅（立即订阅 → 浏览器支付 → 回跳激活） ─────────────────
 *
 * 渠道是 Buy Me a Coffee（PWYW 任意金额）。支付发生在 BMAC 静态
 * 捐赠页，成功后**不会**回跳我们——发码由服务端 webhook（按付款
 * 邮箱匹配挂单）驱动，客户端这边的降级链是：
 * 1. 轮询主路径：每轮 wait 问一次订单状态，付完 4s 内感知
 * 2. 回环回调：订阅页拿到激活码后跳 127.0.0.1:<port>/callback，
 *    只对「页面还开着」的场景加速，不触发也无害
 * 3. 手动兜底：订阅页可反复领取激活码，走「已有激活码」输入框
 *
 * state 是 256-bit 随机值，只在本会话内存里和浏览器 URL 里出现；
 * 服务端只存它的哈希。回调里**没有**激活码——真正发码在服务端，
 * 客户端凭 state 去 claim，激活码明文全程只走 https + 本机回环。 */

pub(crate) struct PendingSubscribe {
    pub state: String,
    pub port: Option<u16>,
    pub at: i64,
}

/// 订阅会话有效期 = 订单窗口（15 分钟）。订单过期后服务端会拒绝 claim，
/// 这里同步收掉，避免界面拿着死会话一直轮询。
const SUBSCRIBE_TTL: i64 = 15 * 60;

impl Gateway {
    /// 发起订阅：生成 state、占一个回环端口、拼订阅页地址。
    /// 端口绑不上不算失败——降级成轮询 + 手动贴码。
    pub async fn subscribe_start(&self) -> Result<GwSubscribeStart, String> {
        let subscribe_id = uuid::Uuid::new_v4().simple().to_string();
        // 两个 uuid 拼成 64 hex（256 bit）。uuid 简单格式就是全 hex，
        // 服务端对 state 的形状校验（≥32 hex）天然通过。
        let state = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let port = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l.local_addr().ok().map(|a| a.port()),
            Err(_) => None,
        };
        let mut url = format!("{}/subscribe?state={state}", license_api());
        if let Some(p) = port {
            url.push_str(&format!("&port={p}"));
        }
        {
            let mut map = self.st.subscribes.lock().unwrap_or_else(|e| e.into_inner());
            let now = now();
            map.retain(|_, v| now - v.at < SUBSCRIBE_TTL);
            map.insert(
                subscribe_id.clone(),
                PendingSubscribe { state, port, at: now },
            );
        }
        tracing::info!(target: "license", port = port.unwrap_or(0), "已发起订阅，等待支付");
        Ok(GwSubscribeStart {
            subscribe_id,
            url,
            port,
            expires_in: SUBSCRIBE_TTL as u64,
        })
    }

    /// 等一轮订阅结果。`wait_secs` 是本次最多阻塞多久（回环回调等待），
    /// 超时后顺带问一次订单状态——浏览器关了、回调被拦，轮询兜底照样通。
    /// 界面循环调用直到拿到 done / expired / error。
    pub async fn subscribe_wait(&self, subscribe_id: &str, wait_secs: u64) -> GwSubscribeState {
        let pending = {
            let map = self.st.subscribes.lock().unwrap_or_else(|e| e.into_inner());
            map.get(subscribe_id).map(|p| (p.state.clone(), p.port, p.at))
        };
        let Some((state, port, at)) = pending else {
            return GwSubscribeState::error("这次订阅会话不存在或已完成，请重新发起");
        };
        if now() - at >= SUBSCRIBE_TTL {
            self.subscribe_cancel(subscribe_id);
            return GwSubscribeState::expired();
        }

        // 1) 回环回调。wait_callback 每次调用重新绑同一端口（与登录流一致）
        if let Some(port) = port {
            match freeport_auth::sub::wait_callback(port, "/callback", wait_secs).await {
                Ok(cb) => {
                    if cb.state.as_deref() != Some(state.as_str()) {
                        return GwSubscribeState::error("回调校验不通过，请重新发起订阅");
                    }
                    return self.subscribe_complete(subscribe_id, &state).await;
                }
                // 超时不是错误：接着走轮询
                Err(freeport_auth::AuthError::Expired) => {}
                Err(e) => return GwSubscribeState::error(e.to_string()),
            }
        } else {
            // 无回环端口（headless/端口被占）：纯轮询节奏放慢点
            tokio::time::sleep(std::time::Duration::from_secs(wait_secs.min(5))).await;
        }

        // 2) 轮询兜底
        match license_post("/v1/order/status", None, serde_json::json!({ "state": state })).await {
            Ok(resp) => match resp.get("status").and_then(|v| v.as_str()) {
                Some("issued") => self.subscribe_complete(subscribe_id, &state).await,
                Some("expired") => {
                    self.subscribe_cancel(subscribe_id);
                    GwSubscribeState::expired()
                }
                _ => GwSubscribeState::pending(),
            },
            // 连不上服务器就当还没付好——下一轮再试，不急着报错
            Err(_) => GwSubscribeState::pending(),
        }
    }

    /// 领码 + 激活。claim → activate 复用在线激活全链路（本地验签、
    /// 落盘、状态机一个不少）。
    async fn subscribe_complete(&self, subscribe_id: &str, state: &str) -> GwSubscribeState {
        let key = match license_post("/v1/order/claim", None, serde_json::json!({ "state": state }))
            .await
        {
            Ok(resp) => match resp.get("key").and_then(|v| v.as_str()) {
                Some(k) => k.to_string(),
                None => return GwSubscribeState::error("订单已完成，但服务器没返回激活码"),
            },
            Err(e) => {
                return match e.code.as_deref() {
                    // 支付刚到账、发码还在路上：算 pending，下一轮再来
                    Some("not_ready") => GwSubscribeState::pending(),
                    _ => GwSubscribeState::error(e.humanize()),
                }
            }
        };
        match self.activate_license(&key).await {
            Ok(l) if l.pro => {
                self.subscribe_cancel(subscribe_id);
                tracing::info!(target: "license", "订阅支付完成，已自动激活");
                GwSubscribeState::done(l)
            }
            Ok(l) => GwSubscribeState::error(
                l.message.unwrap_or_else(|| "激活未生效，请手动输入激活码".into()),
            ),
            Err(e) => GwSubscribeState::error(e),
        }
    }

    pub fn subscribe_cancel(&self, subscribe_id: &str) {
        let mut map = self.st.subscribes.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(subscribe_id);
    }

    /* ── 授权管理（设备列表 / 自助解绑） ─────────────────────── */

    /// 席位占用列表。`current` 标出本机，界面好把「解绑」留给别的设备。
    pub async fn license_devices(&self) -> Result<GwLicenseDevices, String> {
        let (machine, token) = {
            let d = self.st.data.read().await;
            (machine_of(&d), d.license.token().map(str::to_string))
        };
        let Some(token) = token else {
            return Err("尚未激活许可证".into());
        };
        let body = serde_json::json!({ "machineId": machine });
        let resp = license_post("/v1/license/devices", Some(&token), body)
            .await
            .map_err(|e| e.humanize())?;
        let items = resp
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        Some(GwDevice {
                            machine_id: v.get("machineId")?.as_str()?.to_string(),
                            activated_at: v.get("activatedAt")?.as_i64()?,
                            last_refresh: v.get("lastRefresh")?.as_i64()?,
                            current: v.get("machineId").and_then(|m| m.as_str()) == Some(machine.as_str()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(GwLicenseDevices {
            items,
            seats: resp.get("seats").and_then(|v| v.as_i64()).unwrap_or(0),
            plan: resp
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("lifetime")
                .to_string(),
        })
    }

    /// 远程解绑一台设备（腾席位）。失败原样上抛——「解绑了却没解掉」
    /// 必须让用户知道，不能安静吞掉。
    pub async fn license_unbind(&self, machine_id: &str) -> Result<bool, String> {
        let token = {
            let d = self.st.data.read().await;
            d.license.token().map(str::to_string)
        };
        let Some(token) = token else {
            return Err("尚未激活许可证".into());
        };
        let body = serde_json::json!({ "machineId": machine_id.trim() });
        let resp = license_post("/v1/license/unbind", Some(&token), body)
            .await
            .map_err(|e| e.humanize())?;
        Ok(resp.get("removed").and_then(|v| v.as_bool()).unwrap_or(false))
    }
}
