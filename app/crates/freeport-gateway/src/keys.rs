//! 虚拟 key：把「一台机器一个 token」变成「一个中心多个租户」。
//!
//! 三个刻意的取舍：
//! 1. **不存明文**。只存 sha256，UI 只显示前 8 位。泄露 `freeport.json` 不等于泄露 key。
//! 2. **新建时返回一次明文**。之后无法再取回——这是唯一安全的做法。
//! 3. **限额靠惰性窗口**。不建定时任务，重启不丢计数。

use sha2::{Digest, Sha256};

use freeport_core::model::{ApiKey, AppData, KeyUsage};

/// 校验 + 计数一次。返回 Err 时带上给用户看的原因。
pub fn check_and_spend(data: &mut AppData, key_id: &str, model: &str) -> Result<(), String> {
    let Some(idx) = data.api_keys.iter().position(|k| k.id == key_id) else {
        return Ok(()); // legacy token 不在列表里，不限额
    };

    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let minute = now.timestamp() / 60;

    let key = &mut data.api_keys[idx];
    // 惰性归零：跨天 / 跨分钟时读数先重置
    if key.usage.day != today {
        key.usage.day = today;
        key.usage.day_count = 0;
    }
    if key.usage.minute != minute {
        key.usage.minute = minute;
        key.usage.minute_count = 0;
    }

    if let Some(limit) = key.rpm_limit {
        if key.usage.minute_count >= limit {
            return Err(format!(
                "该 key 已达每分钟 {limit} 次上限，请 {} 秒后重试",
                60 - now.timestamp() % 60
            ));
        }
    }
    if let Some(limit) = key.daily_req_limit {
        if key.usage.day_count >= limit {
            return Err(format!("该 key 今日 {limit} 次配额已用完"));
        }
    }

    key.usage.minute_count += 1;
    key.usage.day_count += 1;
    key.last_used_at = Some(now.to_rfc3339());
    let _ = model;
    Ok(())
}

/// 供 UI 展示：只给前缀与用量。
pub fn public_view(k: &ApiKey, today: &str) -> (u32, u32) {
    let used = if k.usage.day == today {
        k.usage.day_count
    } else {
        0
    };
    (used, k.daily_req_limit.unwrap_or(0))
}

/// 生成新 key：`fp-` + 32 位 hex，约 128 bit 熵。
pub fn generate() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    format!("fp-{}", hex::encode(buf))
}

pub fn hash(plain: &str) -> String {
    let mut h = Sha256::new();
    h.update(plain.as_bytes());
    hex::encode(h.finalize())
}

/// 明文开头几位，UI 用来分辨是哪一个。
pub fn prefix(plain: &str) -> String {
    plain.chars().take(11).collect()
}

pub fn new_key(name: &str, plain: &str, rpm: Option<u32>, daily: Option<u32>) -> ApiKey {
    ApiKey {
        id: format!("key_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
        name: name.to_string(),
        key_hash: hash(plain),
        prefix: prefix(plain),
        rpm_limit: rpm,
        daily_req_limit: daily,
        enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_used_at: None,
        usage: KeyUsage::default(),
    }
}

/// 旧版本只有一个 `authToken`。首次加载时把它迁移成 `name = "default"` 的一条，
/// 这样老客户端不用改配置就能继续用，也能在 UI 里看到它。
pub fn migrate_legacy(data: &mut AppData) -> bool {
    if !data.api_keys.is_empty() {
        return false;
    }
    let token = data.gateway.auth_token.trim().to_string();
    if token.is_empty() {
        return false;
    }
    let mut k = new_key("default", &token, None, None);
    k.id = "key_default".into();
    data.api_keys.push(k);
    true
}

/// 从请求头里的 token 解析身份。
///
/// 顺序：先比 `authToken`（兼容），再查虚拟 key。两者都不中返回 None。
pub enum Identity {
    /// 老的全局 token，不限额
    Legacy,
    Key(String),
}

impl Identity {
    /// 日志归因用的 id
    pub fn label(&self) -> String {
        match self {
            Self::Legacy => "default".into(),
            Self::Key(id) => id.clone(),
        }
    }
}

pub fn resolve(data: &AppData, token: &str) -> Option<Identity> {
    if token.is_empty() {
        return None;
    }
    if !data.gateway.auth_token.is_empty() && ct_eq(token, &data.gateway.auth_token) {
        return Some(Identity::Legacy);
    }
    let h = hash(token);
    data.api_keys
        .iter()
        .find(|k| k.enabled && ct_eq(&h, &k.key_hash))
        .map(|k| Identity::Key(k.id.clone()))
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_legacy_then_virtual_key() {
        let mut d = AppData::default();
        d.gateway.auth_token = "legacy-token".into();
        let plain = generate();
        d.api_keys.push(new_key("ci", &plain, None, None));
        d.api_keys.push(new_key("off", &generate(), None, None));
        d.api_keys[1].enabled = false;

        assert!(matches!(resolve(&d, "legacy-token"), Some(Identity::Legacy)));
        assert!(matches!(resolve(&d, &plain), Some(Identity::Key(_))));
        assert!(resolve(&d, "nope").is_none());
        assert!(resolve(&d, "").is_none(), "空 token 必须拒绝");
    }

    #[test]
    fn daily_limit_blocks_after_quota() {
        let mut d = AppData::default();
        let plain = generate();
        let k = new_key("ci", &plain, None, Some(2));
        let id = k.id.clone();
        d.api_keys.push(k);

        assert!(check_and_spend(&mut d, &id, "auto").is_ok());
        assert!(check_and_spend(&mut d, &id, "auto").is_ok());
        let err = check_and_spend(&mut d, &id, "auto").unwrap_err();
        assert!(err.contains("今日 2 次"), "{err}");
    }

    #[test]
    fn disabled_key_is_rejected() {
        let mut d = AppData::default();
        let plain = generate();
        let mut k = new_key("ci", &plain, None, None);
        k.enabled = false;
        d.api_keys.push(k);
        assert!(resolve(&d, &plain).is_none());
    }

    #[test]
    fn migrate_creates_default_key_from_auth_token() {
        let mut d = AppData::default();
        d.gateway.auth_token = "abc".into();
        assert!(migrate_legacy(&mut d));
        assert_eq!(d.api_keys.len(), 1);
        assert_eq!(d.api_keys[0].id, "key_default");
        assert_eq!(d.api_keys[0].prefix, "abc");
        // 幂等
        assert!(!migrate_legacy(&mut d));
    }
}
