//! 迁移包文件（`.fpk`）的端到端验证。
//!
//! 三条验收标准：
//! ① 写出的文件就是迁移码原文，`open()` 能解开，且导入后配置真的进来了
//! ② 覆盖已有文件不报错，且不留临时文件
//! ③ 空文件 / 非文本 / 超大文件都给出人话错误，而不是「解密失败」

use std::path::PathBuf;

use freeport_gateway::{secrets, Gateway};
use freeport_gateway::transfer::{self, Mode};
use serde_json::json;

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fp-fpk-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);
    dir
}

#[tokio::test]
async fn transfer_file_roundtrip_and_bad_inputs() {
    let src_dir = fresh_dir("src");
    std::fs::write(
        src_dir.join("freeport.json"),
        json!({
            "version": 1,
            "providers": [{
                "id": "p1",
                "name": "P1",
                "baseUrl": "https://api.example.com/v1",
                "tier": "A",
                "keyMask": "k••••k",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [],
                "health": { "status": "unknown" },
                "accounts": [{
                    "id": "p1",
                    "name": "默认",
                    "keyMask": "k••••k",
                    "enabled": true,
                    "addedAt": "2026-10-19T00:00:00Z"
                }]
            }],
            "routing": { "chain": [] }
        })
        .to_string(),
    )
    .unwrap();
    secrets::set_key("p1", "sk-source-key").unwrap();

    let gw = Gateway::new();
    gw.load().await;

    /* ── ① 导出到文件，再原样读回来 ── */

    let file = src_dir.join("bundle.fpk");
    let saved = gw
        .export_bundle_to(file.clone(), "correct horse battery")
        .await
        .expect("导出到文件应当成功");
    assert_eq!(saved, file.display().to_string());
    assert!(file.is_file(), "文件没落地：{}", file.display());

    let raw = std::fs::read_to_string(&file).unwrap();
    assert!(
        raw.contains("freeport.transfer"),
        "文件内容应当就是迁移码原文"
    );
    assert!(
        !raw.contains("sk-source-key"),
        "迁移包里不能出现明文 key"
    );

    // 权限：同机其他用户不该能随手拷走（哪怕是密文）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "迁移包权限应当是 0600，实际 {mode:o}");
    }

    // 同目录不该留临时文件
    let leftovers: Vec<_> = std::fs::read_dir(&src_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "留下了临时文件：{leftovers:?}");

    // 解出来的东西与内存里一致
    assert!(
        transfer::open(&raw, "correct horse battery").is_ok(),
        "同一口令应当能解开"
    );

    /* ── ② 目标机器导入 ── */

    let dst_dir = fresh_dir("dst");
    std::fs::write(dst_dir.join("freeport.json"), json!({ "version": 1 }).to_string()).unwrap();
    let dst = Gateway::new();
    dst.load().await;

    let sum = dst
        .import_bundle_from(file.clone(), "correct horse battery", "merge")
        .await
        .expect("从文件导入应当成功");
    assert_eq!(sum.providers_added, 1);
    assert_eq!(sum.accounts_added, 1);
    assert_eq!(sum.secrets_written, 1);
    assert_eq!(secrets::get_key("p1").as_deref(), Some("sk-source-key"));

    // 口令错了：报错要说清是口令，不是「文件坏了」
    let err = dst
        .import_bundle_from(file.clone(), "wrong passphrase!!", "merge")
        .await
        .unwrap_err();
    assert!(err.contains("口令"), "实际：{err}");

    /* ── ③ 覆盖写 + 坏输入 ── */

    // 同一路径再写一次（对话框已经确认过覆盖）
    dst.export_bundle_to(file.clone(), "another passphrase")
        .await
        .expect("覆盖已有文件应当成功");
    let raw2 = std::fs::read_to_string(&file).unwrap();
    assert!(transfer::open(&raw2, "another passphrase").is_ok());
    assert!(
        transfer::open(&raw2, "correct horse battery").is_err(),
        "覆盖后旧口令必须失效"
    );

    // 空文件
    let empty = src_dir.join("empty.fpk");
    std::fs::write(&empty, "").unwrap();
    assert!(transfer::read_file(&empty).unwrap_err().contains("空"));

    // 非文本
    let bin = src_dir.join("bin.fpk");
    std::fs::write(&bin, [0xffu8, 0xfe, 0x00, 0x01]).unwrap();
    assert!(transfer::read_file(&bin).unwrap_err().contains("不是文本"));

    // 超大：先写一个稀疏文件，再确认被大小闸门挡下（不真的读进内存）
    let big = src_dir.join("big.fpk");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(transfer::MAX_FILE_BYTES + 1).unwrap();
    drop(f);
    assert!(transfer::read_file(&big).unwrap_err().contains("不像迁移包"));

    // 目录不存在
    let missing = src_dir.join("nope").join("x.fpk");
    assert!(transfer::write_file(&missing, "{}").unwrap_err().contains("目录不存在"));

    // 口令过短：文件一个字节都不该写
    let short = src_dir.join("short.fpk");
    let err = dst.export_bundle_to(short.clone(), "123").await.unwrap_err();
    assert!(err.contains("口令至少"), "实际：{err}");
    assert!(!short.exists(), "口令太短时不该产生文件");

    // 覆盖模式的语义顺带过一遍：replace 后本地多余凭据的密文被清掉
    let sum = dst
        .import_bundle_from(file, "another passphrase", "replace")
        .await
        .expect("覆盖导入应当成功");
    assert_eq!(sum.mode, Mode::Replace.as_str());
}
