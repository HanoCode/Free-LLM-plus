//! 落盘目标必须是**本实例的目录**，不是进程级的 `data_dir()`。
//!
//! 这条曾经真出过事：网关所有改动都走 `store::save()`（写 `FREEPORT_DATA_DIR`
//! 或系统默认目录），而 `load_from(dir)` 只是把 `dir` 记在 State 里给 `control.key`
//! 用。于是用 `load_from` 载入的实例一旦落盘——配额记账、统计、健康度回写都会——
//! 就把那份数据写进了**真实用户目录**。
//!
//! 现场表现很安静：保存成功、日志无错，只是用户的 `freeport.json` 被测试配置
//! 顶掉了。所以这里把「诱饵目录」摆出来：任何写到进程级目录的行为都会留在那儿。

use serde_json::json;

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "fp-store-target-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn writes_land_in_the_loaded_dir_not_the_process_dir() {
    let decoy = temp("decoy");
    let instance = temp("instance");

    // 进程级目录指向诱饵：谁要是用 store::save()，文件就落在这儿
    std::env::set_var("FREEPORT_DATA_DIR", &decoy);

    let gw = freeport_gateway::Gateway::new();
    gw.load_from(&instance).await;

    // 走控制面的写路径（create_key 直接落盘）
    gw.create_key(freeport_core::dto::KeyInput {
        name: "agent:claude-code".into(),
        rpm_limit: None,
        daily_limit: None,
        daily_token_limit: None,
        allowed_models: Vec::new(),
        expires_at: None,
    })
    .await
    .expect("建 key 要成功");

    // 再走一条会写状态的配置路径
    gw.set_config(freeport_core::dto::ConfigPatch {
        compress: Some(true),
        ..Default::default()
    })
    .await;

    let written = instance.join("freeport.json");
    assert!(written.exists(), "配置必须写在 load_from 指定的目录里");
    let raw = std::fs::read_to_string(&written).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["gateway"]["compress"]["enabled"], json!(true));

    assert!(
        !decoy.join("freeport.json").exists(),
        "绝不能写到进程级 data_dir——那正是「跑个测试用户配置没了」的成因"
    );

    std::env::remove_var("FREEPORT_DATA_DIR");
    let _ = std::fs::remove_dir_all(&decoy);
    let _ = std::fs::remove_dir_all(&instance);
}
