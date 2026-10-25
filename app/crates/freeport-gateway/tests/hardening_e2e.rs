//! Pro 防护加固端到端：时间高水位、能力清单门禁、离线通道上限。
//!
//! 这三个用例只测**客户端本地**的加固逻辑，用 [`Issuer`] 自己签 token，
//! 不依赖任何服务器。设备挑战-应答的用例随 server-api 一起移除
//! （v0.1 清理，git 历史可查）；恢复收费时按 docs/PRO-DESIGN.md
//! 重建服务器后从历史恢复。
//!
//! 进程级 env（本文件独占一个测试进程，用例间用 env_lock 串行）。

use freeport_gateway::Gateway;
use freeport_license as fl;
use freeport_license::issuer::Issuer;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-hard-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    std::fs::write(
        dir.join("freeport.json"),
        serde_json::json!({ "version": 1 }).to_string(),
    )
    .unwrap();
    dir
}

fn issuer() -> &'static Issuer {
    static ISS: std::sync::OnceLock<Issuer> = std::sync::OnceLock::new();
    ISS.get_or_init(|| {
        let iss = Issuer::generate();
        std::env::set_var("FREEPORT_LICENSE_PUBKEY", iss.public_b64());
        iss
    })
}

// ── 1. 离线通道上限 ─────────────────────────────────────

#[tokio::test]
async fn offline_activation_rejects_overlong_tokens() {
    let _lock = env_lock();
    let iss = issuer();
    let _dir = fresh_dir("offline-cap");
    let gw = Gateway::new();
    gw.load().await;

    // 500 天有效期的离线 JWT：没有服务器兜底，拒收
    let long = iss.sign(&fl::Claims::pro("a@b.c", 500, now()));
    let err = gw.activate_license(&long).await.unwrap_err();
    assert!(err.contains("离线激活"), "实际：{err}");

    // 30 天的正常离线 token：照旧接受
    let ok = iss.sign(&fl::Claims::pro("a@b.c", 30, now()));
    let l = gw.activate_license(&ok).await.unwrap();
    assert!(l.pro);
}

// ── 2. 能力清单门禁 ─────────────────────────────────────

#[test]
#[ignore = "FREE_FOR_ALL：门禁关闭，恢复收费时移除 ignore"]
fn feature_gate_follows_the_claims() {
    use freeport_core::model::{AppData, LicenseCfg};
    use freeport_gateway::pro::{allow, Feature};

    let _lock = env_lock();
    let iss = issuer();
    let _dir = fresh_dir("feat-gate");

    let mk = |feat: Vec<&str>| {
        let mut c = fl::Claims::pro("a@b.c", 30, now());
        c.feat = feat.into_iter().map(str::to_string).collect();
        let mut d = AppData::default();
        d.license = LicenseCfg {
            token: Some(iss.sign(&c)),
        };
        d
    };

    // 新式细粒度清单：只解锁列出的能力
    let mut d = mk(vec!["quota.intel", "backup"]);
    assert!(allow(&d, Feature::Intel));
    // AutoBackup 是「Pro 之上用户还可以关」的开关：默认关，打开后放行
    assert!(!allow(&d, Feature::AutoBackup), "默认关的开关不该自己亮");
    d.pro.backup.enabled = true;
    assert!(allow(&d, Feature::AutoBackup), "清单里有 backup 且开关打开，应放行");
    assert!(!allow(&d, Feature::Profiles), "不在清单里的能力不能通配");
    assert!(!allow(&d, Feature::Prewarm));

    // 旧式清单（全部由四条旧能力组成）：通配，存量用户不掉功能
    let d = mk(vec!["feed.realtime", "quota.intel", "sync.cloud", "log.90d"]);
    for f in [Feature::Intel, Feature::Profiles, Feature::RemoteAccess] {
        assert!(allow(&d, f), "{f:?} 应被旧清单通配放行");
    }

    // 无 token：一律拒绝
    let d = AppData::default();
    assert!(!allow(&d, Feature::Intel));
}

// ── 3. 时间高水位 ───────────────────────────────────────

#[tokio::test]
#[ignore = "FREE_FOR_ALL：license() 恒报 free_era，恢复收费时移除 ignore"]
async fn clock_rollback_invalidates_the_license() {
    let _lock = env_lock();
    let _iss = issuer();
    let dir = fresh_dir("hwm");
    let gw = Gateway::new();
    gw.load().await;
    assert_eq!(gw.license().await.status, "missing", "正常时钟不该误伤");

    // 高水位被推到未来（等价于：这台机器见过更晚的真实时间，
    // 现在系统时钟被拨回去了）
    let future = now() + 10 * 86400;
    std::fs::write(
        dir.join("time.hwm"),
        serde_json::json!({ "v": 1, "hwm": future }).to_string(),
    )
    .unwrap();

    // 回拨状态下一切许可证验签判无效——有没有 token 都一样
    let l = gw.license().await;
    assert_eq!(l.status, "invalid");
    assert!(!l.pro);
    assert!(l.message.as_deref().unwrap_or("").contains("时钟"));

    // 填一份真 token：验签时判无效，且明说时钟问题
    let token = issuer().sign(&fl::Claims::pro("a@b.c", 30, now()));
    let l = gw.activate_license(&token).await;
    // 激活路径也会被高水位拦住（Status::Invalid 不落盘）
    let err = l.unwrap_err();
    assert!(err.contains("时钟"), "实际：{err}");

    // 数据面身份降为 free，但转发不受影响（本测试不转发，只验证 tier）
    let mut d = freeport_core::model::AppData::default();
    d.license = freeport_core::model::LicenseCfg { token: Some(token.clone()) };
    let id = freeport_gateway::license::identity(&d);
    assert_eq!(id.tier, "free");

    // 清掉高水位文件，世界恢复正常
    std::fs::remove_file(dir.join("time.hwm")).unwrap();
    let l = gw.activate_license(&token).await.unwrap();
    assert!(l.pro);
}
