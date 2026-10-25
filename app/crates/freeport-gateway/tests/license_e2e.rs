//! 许可证的端到端行为：激活、持久化、拒绝、降级。
//!
//! 这里用完即弃地生成一对测试密钥，用 `FREEPORT_LICENSE_PUBKEY` 覆盖内置公钥
//! （只有 debug 构建会读这个变量，release 忽略——否则改个 env 就能自签）。

use freeport_gateway::Gateway;
use freeport_license::issuer::Issuer;
use freeport_license::Claims;
use serde_json::json;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-lic-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    // 最小配置：不配厂商也能跑，许可证与转发互不依赖
    std::fs::write(dir.join("freeport.json"), json!({ "version": 1 }).to_string()).unwrap();
    dir
}

#[tokio::test]
async fn license_lifecycle() {
    let _dir = fresh_dir("lifecycle");
    let iss = Issuer::generate();
    std::env::set_var("FREEPORT_LICENSE_PUBKEY", iss.public_b64());
    let now = chrono::Utc::now().timestamp();

    let gw = Gateway::new();
    gw.load().await;

    // 1) 从没激活过：免费开放时代（pro::FREE_FOR_ALL）恒报 free_era/pro，
    //    UI 不弹红字、功能全开；恢复收费后这里回到 missing
    let l = gw.license().await;
    assert_eq!(l.status, "free_era");
    assert!(l.pro);
    assert_eq!(l.tier, "pro");
    assert!(l.machine_id.starts_with("m_"));

    // 2) 垃圾 token：拒绝（激活机制在免费时代仍然工作），且不落盘
    let err = gw.activate_license("not-a-token").await.unwrap_err();
    assert!(err.contains("格式"), "错误要说清原因，实际：{err}");
    assert_eq!(gw.license().await.status, "free_era");

    // 3) 真 token：激活成功，能力清单下发
    let token = iss.sign(&Claims::pro("user@example.com", 30, now));
    let l = gw.activate_license(&token).await.unwrap();
    assert_eq!(l.status, "valid");
    assert!(l.pro);
    assert_eq!(l.subject.as_deref(), Some("user@example.com"));
    assert!(l.features.iter().any(|f| f == "sync.cloud"));
    assert!(matches!(l.days_left, Some(d) if d >= 29));

    // 4) 重启后还在，且安装码稳定（否则绑机器的 license 每次启动都失效）
    let gw2 = Gateway::new();
    gw2.load().await;
    let l2 = gw2.license().await;
    assert!(l2.pro);
    assert_eq!(l2.machine_id, l.machine_id);

    // 5) 已过期（超过宽限期）的 token 被拒，已有的 license 不受影响
    let stale = iss.sign(&Claims::pro("u@x", 1, now - 40 * 86400));
    let err = gw2.activate_license(&stale).await.unwrap_err();
    assert!(err.contains("过期"), "实际：{err}");
    assert!(gw2.license().await.pro);

    // 6) 绑在别的机器上：拒绝，并说清是机器不对
    let mut other = Claims::pro("u@x", 30, now);
    other.mid = Some("m_somebody_else".into());
    let err = gw2.activate_license(&iss.sign(&other)).await.unwrap_err();
    assert!(err.contains("另一台机器"), "实际：{err}");

    // 7) 绑本机：通过
    let mut mine = Claims::pro("u@x", 30, now);
    mine.mid = Some(l2.machine_id.clone());
    assert!(gw2.activate_license(&iss.sign(&mine)).await.unwrap().pro);

    // 8) 解除后回到 free_era（免费时代没有「免费版」这个更低的档），
    //    转发配置一个字节没动
    let l = gw2.clear_license().await;
    assert_eq!(l.status, "free_era");
    assert!(l.pro);

    // 9) 宽限期：签名有效、刚过期几天，仍算 Pro，但带提醒。
    //    掉能力会误伤「刚过期还没来得及续费」的付费用户。
    let token = iss.sign(&Claims::pro("late@payer.com", 1, now - 3 * 86400));
    let l = gw2.activate_license(&token).await.unwrap();
    assert_eq!(l.status, "grace");
    assert!(l.pro);
    assert!(l.message.as_deref().unwrap_or("").contains("宽限期"));
    assert!(l.grace_until.unwrap() > now);
}
