//! 离线许可证（Pro）。
//!
//! 产品红线只有一条：**license 失效只能降级，不能罢工**。所以这个 crate
//! 只回答「现在是什么身份」，从不参与转发链路的准入判断。网关拿到 `is_pro()`
//! 之后最多改两件事——请求哪个 tier 的目录、同步走不走云端——绝不影响
//! `/v1/chat/completions` 能不能通。
//!
//! 格式是标准 JWT（`alg=EdDSA`），签名覆盖 `header.payload`：
//!
//! ```text
//! eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9 . eyJzdWIiOiJ... . <64B sig>
//! ```
//!
//! 为什么不用「base64(payload)+base64(sig)」省掉 header：用户手里的 token
//! 迟早要被贴进工单、脚本、CI 环境变量。有 header 才符合「看起来像 JWT」的
//! 直觉，也留了 `kid` 做密钥轮换的余地。
//!
//! 公钥编译进二进制，**没有任何联网校验**——断网、官网宕机、用户改了 DNS，
//! 都不影响启动。私钥只存在于签发工具里，用 `--features issuer` 单独构建，
//! 绝不进 App 依赖图。

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// 到期后的宽限期。付费用户不该在到期那一刻被踢下车。
pub const GRACE_SECS: i64 = 7 * 86400;

/// 生产公钥（Ed25519，32 字节，base64）。
///
/// 私钥在签发端，不进仓库也不进二进制。轮换时改这里 + 让新 token 带 `kid`，
/// 旧 token 仍能被旧公钥验到过期为止（见 [`verify_with`]）。
pub const PROD_PUBKEY: &str = "UPVm5flhrILT9a80ExNh9SpzVu5cFx5Dxhj2gkwWpEI";

pub const HEADER: &str = r#"{"alg":"EdDSA","typ":"JWT"}"#;

/// 许可证里的声明。字段名沿用 JWT 习惯，`feat` 是 Pro 能力清单。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    /// 客户标识（邮箱或订单号）
    pub sub: String,
    /// `pro` / `free`。未知值一律按 free 处理，不猜。
    #[serde(default = "tier_free")]
    pub tier: String,
    /// 能力清单：`feed.realtime` / `quota.intel` / `sync.cloud` / `log.90d`
    #[serde(default)]
    pub feat: Vec<String>,
    /// 签发时刻（unix 秒）
    #[serde(default)]
    pub iat: i64,
    /// 到期时刻（unix 秒）
    #[serde(default)]
    pub exp: i64,
    /// 绑定的机器指纹。不填 = 不绑机器（离线激活的默认做法）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<String>,
    /// 席位数。只做展示与人工核验，客户端不强制
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seats: Option<u32>,
    /// 密钥版本，轮换用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

fn tier_free() -> String {
    "free".into()
}

impl Claims {
    pub fn pro(sub: &str, days: i64, now: i64) -> Self {
        Self {
            sub: sub.into(),
            tier: "pro".into(),
            feat: vec![
                "feed.realtime".into(),
                "quota.intel".into(),
                "sync.cloud".into(),
                "log.90d".into(),
            ],
            iat: now,
            exp: now + days * 86400,
            mid: None,
            seats: None,
            kid: None,
        }
    }
}

/// 状态机。`Grace` 仍然算 Pro——只是 UI 该提醒续费。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 没填过
    Missing,
    /// 正常
    Valid,
    /// 过期但还在 7 天宽限期
    Grace,
    /// 过期且超过宽限期
    Expired,
    /// 签名/格式不对
    Invalid,
    /// 签名有效，但绑的不是这台机器
    WrongMachine,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Missing => "missing",
            Status::Valid => "valid",
            Status::Grace => "grace",
            Status::Expired => "expired",
            Status::Invalid => "invalid",
            Status::WrongMachine => "wrong_machine",
        }
    }
}

/// 校验结果。**只有 `Valid` / `Grace` 且 `tier == "pro"` 才是 Pro。**
#[derive(Debug, Clone)]
pub struct License {
    pub status: Status,
    pub claims: Option<Claims>,
    /// 到期时刻（unix 秒）
    pub expires_at: Option<i64>,
    /// 宽限期结束时刻（unix 秒）
    pub grace_until: Option<i64>,
    /// 剩余天数（向上取整；过期后为负）
    pub days_left: Option<i64>,
    /// 给用户看的一句话。失败时说清「为什么失败」，不要只说 invalid。
    pub error: Option<String>,
}

impl License {
    pub fn missing() -> Self {
        Self {
            status: Status::Missing,
            claims: None,
            expires_at: None,
            grace_until: None,
            days_left: None,
            error: None,
        }
    }

    fn fail(status: Status, msg: impl Into<String>) -> Self {
        Self {
            status,
            claims: None,
            expires_at: None,
            grace_until: None,
            days_left: None,
            error: Some(msg.into()),
        }
    }

    pub fn status_str(&self) -> &'static str {
        self.status.as_str()
    }

    /// 是不是 Pro。这是唯一该被业务代码调用的判断。
    pub fn is_pro(&self) -> bool {
        matches!(self.status, Status::Valid | Status::Grace)
            && self
                .claims
                .as_ref()
                .map(|c| c.tier.eq_ignore_ascii_case("pro"))
                .unwrap_or(false)
    }

    /// **生效**档位：过期或被改过就是 `free`。业务代码只该看这个。
    pub fn tier(&self) -> &str {
        if self.is_pro() {
            "pro"
        } else {
            "free"
        }
    }

    /// **买过**的档位：用于 UI 说「你买的是 Pro，已于 10-01 到期」。
    /// 与 [`tier`](Self::tier) 分开，是因为「买过」和「现在还能用」是两件事。
    pub fn licensed_tier(&self) -> &str {
        match &self.claims {
            Some(c) if c.tier.eq_ignore_ascii_case("pro") => "pro",
            _ => "free",
        }
    }

    /// 能力判断。`Grace` 期间**保留全部能力**，只提示续费。
    pub fn has(&self, feature: &str) -> bool {
        if !self.is_pro() {
            return false;
        }
        self.claims
            .as_ref()
            .map(|c| c.feat.iter().any(|f| f.eq_ignore_ascii_case(feature)))
            .unwrap_or(false)
    }
}

/// 用内置公钥校验。
pub fn verify(token: &str, now: i64, install_id: &str) -> License {
    let pubkey = pubkey();
    match pubkey {
        Some(k) => verify_with(&k, token, now, install_id),
        None => License::fail(
            Status::Invalid,
            "内置公钥缺失：这个构建不是可发布版本，请用官方安装包",
        ),
    }
}

/// 指定公钥校验（测试与密钥轮换用）。
pub fn verify_with(pubkey: &[u8], token: &str, now: i64, install_id: &str) -> License {
    let token = token.trim();
    if token.is_empty() {
        return License::missing();
    }

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return License::fail(Status::Invalid, "许可证格式不对，应形如 x.y.z 三段");
    }

    // 1) header：alg 必须是我们认的。不做「忽略 header 直接验」——
    //    那样有人把 alg 改成 none 也照样过。
    let header_raw = match B64.decode(parts[0]) {
        Ok(v) => v,
        Err(_) => return License::fail(Status::Invalid, "许可证头部不是合法 base64url"),
    };
    let header: serde_json::Value = match serde_json::from_slice(&header_raw) {
        Ok(v) => v,
        Err(_) => return License::fail(Status::Invalid, "许可证头部不是合法 JSON"),
    };
    let alg = header.get("alg").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(alg, "EdDSA" | "Ed25519") {
        return License::fail(Status::Invalid, format!("不支持的签名算法：{alg}"));
    }

    // 2) 签名
    let sig_raw = match B64.decode(parts[2]) {
        Ok(v) => v,
        Err(_) => return License::fail(Status::Invalid, "签名不是合法 base64url"),
    };
    let sig_arr: [u8; 64] = match sig_raw.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return License::fail(Status::Invalid, "签名长度不对（应为 64 字节）"),
    };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    let vk_arr: [u8; 32] = match pubkey.try_into() {
        Ok(a) => a,
        Err(_) => return License::fail(Status::Invalid, "公钥长度不对（应为 32 字节）"),
    };
    let vk = match ed25519_dalek::VerifyingKey::from_bytes(&vk_arr) {
        Ok(k) => k,
        Err(_) => return License::fail(Status::Invalid, "公钥不是合法的 Ed25519 点"),
    };

    let signed = format!("{}.{}", parts[0], parts[1]);
    if vk.verify_strict(signed.as_bytes(), &sig).is_err() {
        return License::fail(
            Status::Invalid,
            "签名校验失败：许可证被改动过，或不是本产品签发的",
        );
    }

    // 3) payload
    let body = match B64.decode(parts[1]) {
        Ok(v) => v,
        Err(_) => return License::fail(Status::Invalid, "许可证内容不是合法 base64url"),
    };
    let claims: Claims = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => return License::fail(Status::Invalid, format!("许可证内容解析失败：{e}")),
    };
    if claims.exp <= 0 {
        return License::fail(Status::Invalid, "许可证没有有效期（缺少 exp）");
    }

    // 4) 机器绑定。md 为空视为不绑；绑了就必须对上。
    if let Some(mid) = claims.mid.as_deref() {
        if !mid.is_empty() && !install_id.is_empty() && mid != install_id {
            return License::fail(
                Status::WrongMachine,
                "这份许可证绑定的是另一台机器，请使用本机的安装码重新激活",
            );
        }
    }

    // 5) 时间窗。允许 1 天时钟偏差（用户改过时区/主板电池没电很常见），
    //    但偏差大到「比签发还早」，只能判无效——否则把系统时间调回去就能续命。
    if now + 86400 < claims.iat {
        return License::fail(
            Status::Invalid,
            "设备时间早于签发时间，请先校正系统时钟",
        );
    }

    let grace_until = claims.exp + GRACE_SECS;
    let days_left = (claims.exp - now).div_euclid(86400);
    let status = if now <= claims.exp {
        Status::Valid
    } else if now <= grace_until {
        Status::Grace
    } else {
        Status::Expired
    };

    License {
        status,
        expires_at: Some(claims.exp),
        grace_until: Some(grace_until),
        days_left: Some(days_left),
        claims: Some(claims.clone()),
        error: match status {
            Status::Grace => Some(format!(
                "已过期，处于 {} 天宽限期，功能暂不受影响，请尽快续费",
                GRACE_SECS / 86400
            )),
            Status::Expired => Some("许可证已过期，已降级为免费版（转发功能不受影响）".into()),
            _ => None,
        },
    }
}

/// 设备指纹：稳定、不采集硬件序列号。用安装 id + 主机名 + OS 拼一个短哈希。
///
/// 不读 MAC / 磁盘序列号是刻意的——那些既不稳定（换网卡就变），
/// 又容易被当成「设备指纹采集」而不安。
pub fn machine_id(install_id: &str) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default();
    let raw = format!("{install_id}|{host}|{}", std::env::consts::OS);
    let digest = short_hash(raw.as_bytes());
    format!("m_{}", &digest[..16])
}

/// 内置公钥。debug 构建允许用环境变量覆盖，方便本地用测试密钥跑端到端；
/// release 构建忽略环境变量——否则改个 env 就能自签。
pub fn pubkey() -> Option<[u8; 32]> {
    #[cfg(debug_assertions)]
    if let Ok(s) = std::env::var("FREEPORT_LICENSE_PUBKEY") {
        if let Ok(raw) = B64.decode(s.trim()) {
            if let Ok(arr) = <[u8; 32]>::try_from(raw.as_slice()) {
                return Some(arr);
            }
        }
    }
    let raw = B64.decode(PROD_PUBKEY).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

fn short_hash(bytes: &[u8]) -> String {
    // 只为一个 16 位指纹引入 sha2 会让 license crate 多一个依赖；
    // 这里用 FNV-1a + 二次混合，够做「不碰撞的短标识」，且不承担安全职责。
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    format!("{h:016x}{:016x}", h.rotate_left(17))
}

// ── 签发端 ────────────────────────────────────────────
// 只有 `--features issuer` 才编译。App 依赖图里没有这个 feature，
// 所以发布出去的二进制里不存在任何签发能力。
#[cfg(feature = "issuer")]
pub mod issuer {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub struct Issuer {
        key: SigningKey,
    }

    impl Issuer {
        pub fn generate() -> Self {
            Self {
                key: SigningKey::generate(&mut rand::rngs::OsRng),
            }
        }

        /// 从 32 字节私钥（base64）恢复
        pub fn from_secret_b64(secret: &str) -> Result<Self, String> {
            let raw = B64
                .decode(secret.trim())
                .map_err(|e| format!("私钥不是合法 base64：{e}"))?;
            let arr: [u8; 32] = raw
                .as_slice()
                .try_into()
                .map_err(|_| "私钥长度不对（应为 32 字节）".to_string())?;
            Ok(Self {
                key: SigningKey::from_bytes(&arr),
            })
        }

        pub fn secret_b64(&self) -> String {
            B64.encode(self.key.to_bytes())
        }

        pub fn public_b64(&self) -> String {
            B64.encode(self.key.verifying_key().to_bytes())
        }

        pub fn sign(&self, claims: &Claims) -> String {
            let head = B64.encode(HEADER.as_bytes());
            let body = B64.encode(serde_json::to_vec(claims).expect("claims 可序列化"));
            let signed = format!("{head}.{body}");
            let sig = self.key.sign(signed.as_bytes());
            format!("{signed}.{}", B64.encode(sig.to_bytes()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用密钥对。**只用于单测**，与生产公钥无关。
    const SK: &str = "nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A";
    const PK: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";

    fn key() -> [u8; 32] {
        B64.decode(PK).unwrap().as_slice().try_into().unwrap()
    }

    fn token(claims: &Claims) -> String {
        // 手写签发，避免测试依赖 issuer feature
        let head = B64.encode(HEADER.as_bytes());
        let body = B64.encode(serde_json::to_vec(claims).unwrap());
        let signed = format!("{head}.{body}");
        let sk_arr: [u8; 32] = B64.decode(SK).unwrap().as_slice().try_into().unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&sk_arr);
        use ed25519_dalek::Signer;
        let sig = sk.sign(signed.as_bytes());
        format!("{signed}.{}", B64.encode(sig.to_bytes()))
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn fresh_token_is_pro() {
        let l = verify_with(&key(), &token(&Claims::pro("a@b.c", 30, NOW)), NOW, "inst");
        assert_eq!(l.status, Status::Valid);
        assert!(l.is_pro());
        assert!(l.has("feed.realtime"));
        assert_eq!(l.days_left, Some(30));
    }

    #[test]
    fn empty_token_is_missing_not_invalid() {
        let l = verify_with(&key(), "  ", NOW, "inst");
        assert_eq!(l.status, Status::Missing);
        // 没填过不是错误，UI 不该弹红字
        assert!(l.error.is_none());
    }

    #[test]
    fn expired_but_inside_grace_stays_pro() {
        let c = Claims::pro("a@b.c", 1, NOW - 3 * 86400);
        let l = verify_with(&key(), &token(&c), NOW, "inst");
        assert_eq!(l.status, Status::Grace);
        assert!(l.is_pro(), "宽限期内不该掉能力");
        assert!(l.error.as_deref().unwrap_or("").contains("宽限期"));
    }

    #[test]
    fn past_grace_degrades_to_free() {
        let c = Claims::pro("a@b.c", 1, NOW - 30 * 86400);
        let l = verify_with(&key(), &token(&c), NOW, "inst");
        assert_eq!(l.status, Status::Expired);
        assert!(!l.is_pro());
        assert_eq!(l.tier(), "free");
        assert!(l.error.as_deref().unwrap_or("").contains("降级"));
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let good = token(&Claims::pro("a@b.c", 1, NOW));
        let parts: Vec<&str> = good.split('.').collect();
        let mut claims = Claims::pro("a@b.c", 1, NOW);
        claims.exp = NOW + 3650 * 86400; // 手改成十年
        let forged_body = B64.encode(serde_json::to_vec(&claims).unwrap());
        let forged = format!("{}.{}.{}", parts[0], forged_body, parts[2]);
        let l = verify_with(&key(), &forged, NOW, "inst");
        assert_eq!(l.status, Status::Invalid);
        assert!(l.error.as_deref().unwrap_or("").contains("签名"));
    }

    #[test]
    fn signature_from_another_key_is_rejected() {
        let head = B64.encode(HEADER.as_bytes());
        let body = B64.encode(serde_json::to_vec(&Claims::pro("a@b.c", 30, NOW)).unwrap());
        let signed = format!("{head}.{body}");
        let other = [7u8; 32];
        let sk = ed25519_dalek::SigningKey::from_bytes(&other);
        use ed25519_dalek::Signer;
        let sig = sk.sign(signed.as_bytes());
        let t = format!("{signed}.{}", B64.encode(sig.to_bytes()));
        let l = verify_with(&key(), &t, NOW, "inst");
        assert_eq!(l.status, Status::Invalid);
    }

    #[test]
    fn alg_none_is_rejected() {
        let head = B64.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let body = B64.encode(serde_json::to_vec(&Claims::pro("a@b.c", 30, NOW)).unwrap());
        let t = format!("{head}.{body}.{}", B64.encode([0u8; 64]));
        let l = verify_with(&key(), &t, NOW, "inst");
        assert_eq!(l.status, Status::Invalid);
        assert!(l.error.as_deref().unwrap_or("").contains("算法"));
    }

    #[test]
    fn machine_mismatch_is_called_out() {
        let mut c = Claims::pro("a@b.c", 30, NOW);
        c.mid = Some("m_other".into());
        let l = verify_with(&key(), &token(&c), NOW, "m_mine");
        assert_eq!(l.status, Status::WrongMachine);
        assert!(!l.is_pro());
    }

    #[test]
    fn machine_match_passes() {
        let mut c = Claims::pro("a@b.c", 30, NOW);
        c.mid = Some("m_mine".into());
        let l = verify_with(&key(), &token(&c), NOW, "m_mine");
        assert_eq!(l.status, Status::Valid);
    }

    #[test]
    fn clock_rollback_beyond_a_day_is_rejected() {
        let c = Claims::pro("a@b.c", 30, NOW);
        let l = verify_with(&key(), &token(&c), NOW - 5 * 86400, "inst");
        assert_eq!(l.status, Status::Invalid);
        assert!(l.error.as_deref().unwrap_or("").contains("时钟"));
    }

    #[test]
    fn small_clock_skew_is_tolerated() {
        let c = Claims::pro("a@b.c", 30, NOW);
        let l = verify_with(&key(), &token(&c), NOW - 3600, "inst");
        assert_eq!(l.status, Status::Valid);
    }

    #[test]
    fn free_tier_has_no_pro_features() {
        let mut c = Claims::pro("a@b.c", 30, NOW);
        c.tier = "free".into();
        let l = verify_with(&key(), &token(&c), NOW, "inst");
        assert_eq!(l.status, Status::Valid);
        assert!(!l.is_pro());
        assert!(!l.has("feed.realtime"));
    }

    #[test]
    fn garbage_is_invalid_with_a_reason() {
        for bad in ["abc", "a.b", "a.b.c", "!!!.???.###"] {
            let l = verify_with(&key(), bad, NOW, "inst");
            assert!(
                matches!(l.status, Status::Invalid | Status::Missing),
                "{bad} 应被拒"
            );
            assert!(l.error.is_some(), "{bad} 应带原因");
        }
    }

    #[test]
    fn machine_id_is_stable_and_short() {
        let a = machine_id("inst-1");
        assert_eq!(a, machine_id("inst-1"));
        assert_ne!(a, machine_id("inst-2"));
        assert!(a.starts_with("m_") && a.len() == 18);
    }
}
