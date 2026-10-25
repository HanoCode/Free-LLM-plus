//! Pro 功能门禁（v1.12 起；v1.16 起全部免费开放）。
//!
//! 分工原则：**决策全在本地。** 情报、报表、备份、预热、巡检增强、
//! 策略档全部本地实现，`is_pro` 是它们唯一的开关。
//!
//! 红线（修订版）：`pro::allow()` 是**唯一**的 Pro 判断入口。它只允许被
//! Pro 功能的入口调用；路由核心（Gate / Order / Pick / 账本 / 熔断 /
//! affinity）永远不看它——即使有人 patch 掉解锁，路由安全与体验
//! 也不会受任何影响。
//!
//! **免费开放时代（v1.16）**：`FREE_FOR_ALL = true`，所有 Pro 功能对
//! 所有人可用，许可证链路（激活/订阅/吊销）代码原样保留但不再参与
//! 门禁——将来要恢复收费，把这一个常量翻回 `false` 即可，其余零改动。

use freeport_core::model::{AppData, Purpose, StrategyProfile};

/// 产品形态开关：`true` = 全部功能免费开放。恢复收费时改回 `false`，
/// 门禁立即回到「许可证 + 能力清单」语义（见下方 allow/grants）。
pub const FREE_FOR_ALL: bool = true;

/// Pro 解锁的本地功能。每个变体对应一个功能入口，判断集中在这一个 match。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// 本机额度情报（`gw_intel`）
    Intel,
    /// 90 天日志留存与用量报表（`gw_reports` / 落盘 JSONL）
    Reports90,
    /// 自动备份定时快照
    AutoBackup,
    /// 会话预热
    Prewarm,
    /// 巡检增强（免费固定 5 次/天，Pro 自动扩展）
    PatrolPlus,
    /// 策略档（按 key / 用途绑定策略与权重）
    Profiles,
    /// 远程接入（绑定非回环地址；非回环强制鉴权）
    RemoteAccess,
}

impl Feature {
    /// token 能力清单（claims.feat）里的能力名。v1.14 起功能门禁不只看
    /// is_pro，还看清单——能力跟着签名过的 claims 走，patch 掉单个
    /// bool 拿不到任何功能（伪造 claims 需要签发私钥）。
    fn feat(self) -> &'static str {
        match self {
            Feature::Intel => "quota.intel",
            Feature::Reports90 => "log.90d",
            Feature::AutoBackup => "backup",
            Feature::Prewarm => "prewarm",
            Feature::PatrolPlus => "patrol.plus",
            Feature::Profiles => "profiles",
            Feature::RemoteAccess => "remote.access",
        }
    }
}

/// 这个功能现在能不能用。免费开放时代只看用户自己的开关；恢复收费后
/// 重新走「许可证能力清单」校验（每次现算，一次 Ed25519 验签约 50µs）。
pub fn allow(data: &AppData, f: Feature) -> bool {
    if FREE_FOR_ALL {
        return match f {
            // 这两个是「用户还可以关」的开关，默认值都偏向开启
            Feature::AutoBackup => data.pro.backup.enabled,
            Feature::Prewarm => data.pro.prewarm,
            _ => true,
        };
    }
    if !crate::license::grants(data, f.feat()) {
        return false;
    }
    match f {
        Feature::Intel
        | Feature::Reports90
        | Feature::PatrolPlus
        | Feature::Profiles
        | Feature::RemoteAccess => true,
        // 这两个是「Pro 之上用户还可以关」的开关，默认值都偏向开启
        Feature::AutoBackup => data.pro.backup.enabled,
        Feature::Prewarm => data.pro.prewarm,
    }
}

/// 解粘阈值。Pro 可以在 0.02..=0.5 里自定义；其余一律用内置默认。
/// 非法配置（NaN / 越界）当没配处理，不报错——配置文件是用户可编辑的。
pub fn unstick_ratio(data: &AppData) -> f64 {
    const DEFAULT: f64 = freeport_core::affinity::UNSTICK_RATIO;
    if !crate::license::state(data).pro {
        return DEFAULT;
    }
    match data.pro.unstick_ratio {
        Some(r) if r.is_finite() && (0.02..=0.5).contains(&r) => r,
        _ => DEFAULT,
    }
}

/// 粘性窗口。免费固定 30 分钟（`DEFAULT_TTL_MS`）；Pro 可在 5..=1440
/// 分钟里自定义。非法配置当没配处理——配置文件是用户可编辑的。
pub fn stick_ttl_ms(data: &AppData) -> i64 {
    const DEFAULT: i64 = freeport_core::affinity::DEFAULT_TTL_MS;
    if !crate::license::state(data).pro {
        return DEFAULT;
    }
    match data.pro.stick_ttl_min {
        Some(min) => (min as i64).clamp(5, 1_440) * 60_000,
        None => DEFAULT,
    }
}

/// 内存日志环容量。免费 300 条（够排障）；Pro 2000 条——
/// 环越大，`intel` / digest 的聚合样本越可信。
pub fn log_ring_cap(data: &AppData) -> usize {
    if crate::license::state(data).pro {
        2_000
    } else {
        300
    }
}

/// 免费版巡检预算：固定 5 次/天。够「今天哪个模型挂了」的最低感知，
/// 撑不起按天体检——把目录保持新鲜是 Pro 巡检增强的卖点。
pub const PATROL_FREE: u32 = 5;

/// Pro 巡检预算天花板：按日配额 1% 自动起算（下限 60），Pro 放宽到 2400。
pub const PATROL_MAX_PRO: u32 = 2_400;

/// 策略档解析：第一条命中的生效。未开 Pro / 没有命中 → `None`，
/// 调用方沿用全局 strategy 与 weights。
pub fn resolve_profile<'a>(
    data: &'a AppData,
    key_id: &str,
    purpose: Purpose,
) -> Option<&'a StrategyProfile> {
    if !allow(data, Feature::Profiles) {
        return None;
    }
    data.routing.profiles.iter().find(|p| p.matches(key_id, purpose))
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_core::model::{AppData, BackupCfg, StrategyProfile};

    #[test]
    fn free_era_opens_everything_respecting_user_switches() {
        let d = AppData::default();
        // 功能全开
        assert!(allow(&d, Feature::Intel));
        assert!(allow(&d, Feature::Reports90));
        assert!(allow(&d, Feature::PatrolPlus));
        assert!(allow(&d, Feature::Profiles));
        assert!(allow(&d, Feature::RemoteAccess));
        // 用户开关仍作数：备份默认关、预热默认开（见 allow 的注释）
        assert!(!allow(&d, Feature::AutoBackup));
        assert!(allow(&d, Feature::Prewarm));
        let mut d2 = d.clone();
        d2.pro.backup.enabled = true;
        d2.pro.prewarm = true;
        assert!(allow(&d2, Feature::AutoBackup));
        assert!(allow(&d2, Feature::Prewarm));
    }

    #[test]
    fn free_era_respects_user_tuning() {
        // 免费开放时代用户配了阈值就生效（原来只有 Pro 能调）
        let mut d = AppData::default();
        d.pro.unstick_ratio = Some(0.3);
        assert!((unstick_ratio(&d) - 0.3).abs() < 1e-9);
        // 非法配置（越界/NaN）当没配处理
        d.pro.unstick_ratio = Some(0.9);
        assert!((unstick_ratio(&d) - freeport_core::affinity::UNSTICK_RATIO).abs() < 1e-9);
        d.pro.unstick_ratio = Some(f64::NAN);
        assert!((unstick_ratio(&d) - freeport_core::affinity::UNSTICK_RATIO).abs() < 1e-9);
        // 粘性窗口与日志环也按 Pro 档
        d.pro.stick_ttl_min = Some(120);
        assert_eq!(stick_ttl_ms(&d), 120 * 60_000);
        assert_eq!(log_ring_cap(&d), 2_000);
    }

    #[test]
    fn profile_validation_requires_a_binding() {
        let p = StrategyProfile {
            id: "p1".into(),
            name: "coding 强模型".into(),
            key_prefix: None,
            purpose: Some("coding".into()),
            strategy: "fastest".into(),
            weights: None,
        };
        assert!(p.validate().is_ok());
        assert!(p.matches("agent:any", Purpose::Coding));
        assert!(!p.matches("agent:any", Purpose::Chat));

        let loose = StrategyProfile { key_prefix: None, purpose: None, ..p.clone() };
        assert!(loose.validate().is_err(), "两个维度都不绑 = 永远命中，必须拒");

        let bogus = StrategyProfile { strategy: "not-a-strategy".into(), ..p };
        assert!(bogus.validate().is_err());
    }

    #[test]
    fn backup_default_is_off() {
        let b = BackupCfg::default();
        assert!(!b.enabled);
        assert_eq!(b.interval_hours, 24);
        assert_eq!(b.keep, 7);
    }
}
