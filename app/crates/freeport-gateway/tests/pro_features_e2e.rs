//! Pro 本机功能门禁端到端：免费版拿不到、Pro 版能用的完整回路。
//!
//! 与 license_e2e 同一套签发手法：完即弃密钥对 + `FREEPORT_LICENSE_PUBKEY`
//! env 覆盖（仅 debug 构建生效）。重点锁三件事：
//! 1. **免费版**所有 Pro 入口一律拒绝/为空，且不给假数据（models 空 + pro:false）；
//! 2. **Pro 版**功能可用：策略档写入与持久化、备份打快照→列表→恢复；
//! 3. 策略档校验：非法档整批拒绝，路由核心不受影响。

use freeport_gateway::Gateway;
use freeport_license::issuer::Issuer;
use freeport_license::Claims;
use freeport_core::dto::{ProPatch, RoutingPatch};
use freeport_core::model::StrategyProfile;
use serde_json::json;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-pro-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    std::fs::write(dir.join("freeport.json"), json!({ "version": 1 }).to_string()).unwrap();
    dir
}

/// 同一个测试二进制里的用例共享一对密钥：`FREEPORT_LICENSE_PUBKEY` 是
/// 进程级 env，各测各的 issuer 会互相覆盖（同进程测试并行跑）。
fn issuer() -> &'static Issuer {
    static ISS: std::sync::OnceLock<Issuer> = std::sync::OnceLock::new();
    ISS.get_or_init(|| {
        let iss = Issuer::generate();
        std::env::set_var("FREEPORT_LICENSE_PUBKEY", iss.public_b64());
        iss
    })
}

/// `FREEPORT_DATA_DIR` 也是进程级 env——load/save 都看它。
/// 并行用例必须串行进入「改 env + 建 Gateway」的临界区。
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn coding_profile() -> StrategyProfile {
    StrategyProfile {
        id: "p-coding".into(),
        name: "coding 走最快".into(),
        key_prefix: Some("agent:".into()),
        purpose: Some("coding".into()),
        strategy: "fastest".into(),
        weights: None,
    }
}

/// 免费开放时代（`pro::FREE_FOR_ALL`）门禁已关闭，本用例保留待恢复收费时
/// 重新启用——届时删掉 `#[ignore]` 即可，断言语义不变。
#[tokio::test]
#[ignore = "FREE_FOR_ALL：门禁关闭，恢复收费时移除 ignore"]
async fn free_user_is_locked_out_of_every_pro_entry() {
    let _lock = env_lock();
    let _dir = fresh_dir("free");
    let _iss = issuer();

    let gw = Gateway::new();
    gw.load().await;
    assert!(!gw.license().await.pro);

    // 情报：pro=false 且 models 为空——不给一份「看起来有」的假数据
    let intel = gw.intel().await;
    assert!(!intel.pro);
    assert!(intel.models.is_empty());

    // 报表：pro=false，聚合为空
    let rep = gw.reports(7).await;
    assert!(!rep.pro);
    assert_eq!(rep.requests, 0);

    // 策略档：写入被拒
    let err = gw.set_profiles(vec![coding_profile()]).await.unwrap_err();
    assert!(err.contains("Pro"), "实际：{err}");

    // 走 set_routing 塞进来的档也要被剥掉：免费版配置里不许躺着一份
    // 「等升级就生效」的档——那等于引诱用户以为设置丢了
    let r = gw
        .set_routing(RoutingPatch {
            strategy: None,
            chain: None,
            chains: None,
            purpose_chain: None,
            weights: None,
            profiles: Some(vec![coding_profile()]),
        })
        .await
        .unwrap();
    assert!(r.profiles.is_empty(), "免费版的策略档必须被剥掉");

    // 备份：没配目录前就先挡在 Pro 门外
    let err = gw.backup_now().await.unwrap_err();
    assert!(err.contains("Pro"), "实际：{err}");

    // 导出留存日志：同样挡在门外
    assert!(gw.reports_export().await.is_err());
}

#[tokio::test]
async fn pro_user_gets_local_features() {
    let _lock = env_lock();
    let _dir = fresh_dir("pro");
    let iss = issuer();
    let _ = iss;
    let now = chrono::Utc::now().timestamp();

    let gw = Gateway::new();
    gw.load().await;
    let token = iss.sign(&Claims::pro("pro@example.com", 30, now));
    let l = gw.activate_license(&token).await.unwrap();
    assert!(l.pro);

    // 情报可用（还没有请求：models 空但 pro=true）
    let intel = gw.intel().await;
    assert!(intel.pro);

    // 报表可用（还没有留存文件：days=0 但 pro=true）
    let rep = gw.reports(7).await;
    assert!(rep.pro);

    // 策略档：合法档可写、可读回、重启还在
    let r = gw.set_profiles(vec![coding_profile()]).await.unwrap();
    assert_eq!(r.profiles.len(), 1);
    assert_eq!(r.profiles[0].strategy, "fastest");
    let dir = _dir.clone();
    let gw2 = Gateway::new();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    gw2.load().await;
    let r2 = gw2.routing().await;
    assert_eq!(r2.profiles.len(), 1, "策略档要落盘");

    // 非法档（两个维度都不绑）：整批拒绝，已存的档不受影响
    let loose = StrategyProfile {
        id: "p-loose".into(),
        name: "没有绑定维度".into(),
        key_prefix: None,
        purpose: None,
        strategy: "rotate".into(),
        weights: None,
    };
    let err = gw2.set_profiles(vec![loose]).await.unwrap_err();
    assert!(err.contains("绑定"), "实际：{err}");
    assert_eq!(gw2.routing().await.profiles.len(), 1);

    // Pro 开关：预热关闭 + 解粘阈值自定义，回读一致
    let cfg = gw2
        .set_pro(ProPatch {
            prewarm: Some(false),
            unstick_ratio: Some(Some(0.25)),
            stick_ttl_min: None,
            backup_enabled: None,
            backup_dir: None,
            backup_interval_hours: None,
            backup_keep: None,
        })
        .await;
    assert!(!cfg.prewarm);
    assert_eq!(cfg.unstick_ratio, Some(0.25));
    // 粘性窗口：设置与回读（120 分钟），再传 null 回默认
    let cfg = gw2
        .set_pro(ProPatch {
            prewarm: None,
            unstick_ratio: None,
            stick_ttl_min: Some(Some(120)),
            backup_enabled: None,
            backup_dir: None,
            backup_interval_hours: None,
            backup_keep: None,
        })
        .await;
    assert_eq!(cfg.stick_ttl_min, Some(120));
    let cfg = gw2
        .set_pro(ProPatch {
            prewarm: None,
            unstick_ratio: Some(None),
            stick_ttl_min: Some(None),
            backup_enabled: None,
            backup_dir: None,
            backup_interval_hours: None,
            backup_keep: None,
        })
        .await;
    assert_eq!(cfg.unstick_ratio, None, "传 null 应回到默认");
    assert_eq!(cfg.stick_ttl_min, None, "粘性窗口 null 也回默认");

    gw2.stop().await;
}

#[tokio::test]
async fn backup_snapshot_roundtrip() {
    let _lock = env_lock();
    let _dir = fresh_dir("backup");
    let backup_dir = _dir.join("snapshots");
    let iss = issuer();
    let now = chrono::Utc::now().timestamp();

    let gw = Gateway::new();
    gw.load().await;
    let token = iss.sign(&Claims::pro("pro@example.com", 30, now));
    gw.activate_license(&token).await.unwrap();

    // 没开开关：报「未开启」；没配目录：报「目录」。两种失败必须分得清。
    let err = gw.backup_now().await.unwrap_err();
    assert!(err.contains("未开启"), "实际：{err}");
    gw.set_pro(ProPatch {
        prewarm: None,
        unstick_ratio: None,
        stick_ttl_min: None,
        backup_enabled: Some(true),
        backup_dir: None,
        backup_interval_hours: None,
        backup_keep: None,
    })
    .await;
    let err = gw.backup_now().await.unwrap_err();
    assert!(err.contains("目录"), "实际：{err}");

    // 配目录 → 打快照 → 列表能看到 → 恢复成功
    let cfg = gw
        .set_pro(ProPatch {
            prewarm: None,
            unstick_ratio: None,
        stick_ttl_min: None,
            backup_enabled: Some(true),
            backup_dir: Some(backup_dir.to_string_lossy().to_string()),
            backup_interval_hours: None,
            backup_keep: None,
        })
        .await;
    assert!(cfg.backup.enabled);

    let path = gw.backup_now().await.unwrap();
    assert!(std::path::Path::new(&path).exists(), "快照要真实存在");
    let list = gw.backup_list().await;
    assert_eq!(list.len(), 1);

    // 路径穿越要被拒
    let err = gw.backup_restore("../evil").await.unwrap_err();
    assert!(err.contains("不合法"), "实际：{err}");

    let summary = gw.backup_restore(&list[0].name).await.unwrap();
    assert_eq!(summary.mode, "merge");
    // last_at 被记账：下一次定时备份要等 interval 到点
    let cfg = gw.pro_config().await;
    assert!(cfg.backup.last_at.is_some());
    gw.stop().await;
}
