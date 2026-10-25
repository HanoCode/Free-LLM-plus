//! 时间高水位（HWM）。
//!
//! 攻击面：验签的 `iat` 检查只防「回拨到签发之前」——把系统时钟拨回
//! `iat..exp` 窗口内，一份 72h 短 token 就能反复复活，续签循环还会因为
//! 「离到期还早」而不去碰服务器，吊销永远到不了本机。
//!
//! 对策：本机持久记录「见过的最大 unix 秒」，每次验签前先比对——
//! 系统时钟比高水位还早超过容差，说明被回拨，许可证直接判无效。
//! **判无效而不是钳制**：钳到高水位等于把时间冻住，token 同样永不过期。
//!
//! 高水位存两份：明文 `time.hwm`（数据目录）+ 密钥库 `license#hwm` 槽
//! （AES-256-GCM）。验签取两份的较大值——抹掉明文那份还得动密钥库。
//! 两份都能被本机专家攻击者抹除，这一层是减速带不是墙；真正的时间锚点
//! 是服务器：每次 license API 往返都把响应 Date 头喂进来，订阅吊销后
//! 任何一次在线往返都会把高水位推过 token 的 exp。
//!
//! 红线不受影响：这里只影响许可证验签，路由与转发从不看它。时钟异常的
//! 机器上 Pro 降级（不罢工），校正时钟即恢复；主板电池没电这类真实故障
//! 靠 1 小时容差 + NTP 自动对时兜住。

use std::fs;
use std::io::Write;

use crate::store;

const FILE: &str = "time.hwm";

/// 容差。高水位本身来自本机时钟，NTP 把快了几分钟的表往回校是正常的；
/// 超过 1 小时的「倒流」才当成回拨。
pub const SKEW_SECS: i64 = 3600;

/// 当前记录的高水位：两份存储的较大值。没有记录（新装机）返回 0。
pub fn observed() -> i64 {
    let from_file = fs::read_to_string(store::data_dir().join(FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("hwm").and_then(|h| h.as_i64()))
        .unwrap_or(0);
    let from_vault = crate::secrets::get_license_hwm().unwrap_or(0);
    from_file.max(from_vault)
}

/// 系统时钟是否比高水位早得离谱（被回拨）。
pub fn rolled_back() -> bool {
    let now = chrono::Utc::now().timestamp();
    now + SKEW_SECS < observed()
}

/// 抬升高水位并落盘（两份都写）。只在真实前进时写——boot、网关 start、
/// 续签循环醒来、license API 往返，都不在请求热路径上。
pub fn note(now: i64) {
    if now <= observed() {
        return;
    }
    let path = store::data_dir().join(FILE);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::File::create(&path) {
        let _ = f.write_all(serde_json::json!({ "v": 1, "hwm": now }).to_string().as_bytes());
    }
    // 密钥库那份尽力而为：写失败（目录被删等）还有明文那份兜底
    let _ = crate::secrets::set_license_hwm(now);
}

pub fn note_now() {
    note(chrono::Utc::now().timestamp());
}

/// 把 license 服务器的 Date 头喂进高水位。服务器时钟是唯一不受本机
/// 控制的锚——吊销/到期之后哪怕一次在线往返，都会把高水位推过 exp，
/// 此后回拨再也救不回那份 token。解析失败静默忽略（Date 头是可选的）。
pub fn note_http_date(value: &str) {
    if let Ok(t) = chrono::DateTime::parse_from_rfc2822(value.trim()) {
        note(t.timestamp());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn with_temp_dir<T>(tag: &str, f: impl FnOnce(std::path::PathBuf) -> T) -> T {
        let _env = crate::store::env_lock();
        let dir = std::env::temp_dir().join(format!("freeport-hwm-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FREEPORT_DATA_DIR", &dir);
        let out = f(dir.clone());
        // 密钥库是进程级单例：把本用例写进槽位的高水位清零，
        // 别漏进并行跑的其他用例（pro.rs 的单测会读它）
        let _ = crate::secrets::set_license_hwm(0);
        let _ = fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn note_advances_and_rolled_back_detects() {
        with_temp_dir("basic", |dir| {
            // 基线可能不为 0：同进程早前用例可能已往密钥库写过值
            let base = observed();
            assert!(!rolled_back(), "当前真实时钟不该被判回拨");

            note(base + 1_000_000);
            assert_eq!(observed(), base + 1_000_000);
            // 明文那份落了盘
            assert!(dir.join(FILE).exists());

            // 判定式边界：容差内不算回拨，超出容差算
            let now = base + 1_000_000 - SKEW_SECS;
            assert!(!(now + SKEW_SECS < observed()), "容差边界上不该判回拨");
            let now = base + 1_000_000 - SKEW_SECS - 1;
            assert!(now + SKEW_SECS < observed(), "超出容差该判回拨");
        });
    }

    #[test]
    fn note_never_goes_backwards() {
        with_temp_dir("monotonic", |_| {
            let base = observed();
            note(base + 5_000_000);
            note(base + 4_999_999);
            note(base + 4_000_000);
            assert_eq!(observed(), base + 5_000_000, "回拨的 note 不该拉低高水位");
        });
    }

    #[test]
    fn http_date_feeds_the_watermark() {
        with_temp_dir("httpdate", |_| {
            // 垫一个固定锚（2027 年）：高于任何真实历史日期，也高于
            // 同进程其他用例可能漏进密钥库的值
            let anchor = observed().max(1_800_000_000);
            note(anchor);
            assert_eq!(observed(), anchor);
            // 过去的服务器时间：只升不降
            note_http_date("Wed, 21 Oct 2015 07:28:00 GMT");
            assert_eq!(observed(), anchor);
            // 解析失败：静默忽略
            note_http_date("不是日期");
            assert_eq!(observed(), anchor);
            // 未来的服务器时间：推进高水位（吊销后一次在线往返就能
            // 把锚推过旧 token 的 exp，靠的就是这条路）
            note_http_date(
                &chrono::Utc
                    .timestamp_opt(anchor + 86_400, 0)
                    .unwrap()
                    .to_rfc2822(),
            );
            assert_eq!(observed(), anchor + 86_400);
        });
    }
}
