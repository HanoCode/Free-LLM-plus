//! 迁移码的端到端验证。
//!
//! 重点不是「能不能加密」，而是四件容易写错的事：
//! 1. 明文密钥不能出现在迁移码里；
//! 2. 口令错 / 被改过都要拒绝，且**不写任何东西**；
//! 3. 合并模式不重复、不覆盖本地已有的 key；
//! 4. 覆盖模式必须把本地多余的密文一起清掉（否则等于「删了还在」）。

use serde_json::{json, Value};

use freeport_core::model::AppData;
use freeport_gateway::{secrets, transfer};

const PASS: &str = "correct horse battery";

fn source_data() -> AppData {
    serde_json::from_value(json!({
        "version": 1,
        "providers": [
            {
                "id": "groq",
                "name": "Groq",
                "baseUrl": "https://api.groq.com/openai/v1",
                "tier": "A",
                "enabled": true,
                "addedAt": "2026-10-19T00:00:00Z",
                "models": [{ "id": "llama-3.3-70b", "ownedBy": "meta" }],
                "accounts": [
                    {
                        "id": "acct_g1",
                        "name": "key-1",
                        "keyMask": "gsk_••••1111",
                        "enabled": true,
                        "addedAt": "2026-10-19T00:00:00Z",
                        "quota": { "rpd": 1000 }
                    },
                    {
                        "id": "acct_g2",
                        "name": "key-2",
                        "keyMask": "gsk_••••2222",
                        "enabled": false,
                        "addedAt": "2026-10-19T00:00:00Z"
                    }
                ],
                "health": { "status": "healthy", "latencyMs": 120 }
            },
            {
                "id": "cerebras",
                "name": "Cerebras",
                "baseUrl": "https://api.cerebras.ai/v1",
                "tier": "A",
                "accounts": [{
                    "id": "acct_c1",
                    "name": "default",
                    "keyMask": "csk-••••3333",
                    "enabled": true,
                    "addedAt": "2026-10-19T00:00:00Z"
                }]
            }
        ],
        "routing": { "strategy": "balanced", "chain": ["groq:llama-3.3-70b"] },
        "patrol": { "enabled": true, "hotHours": 6, "coldHours": 48 },
        "apiKeys": [{
            "id": "key_ci",
            "name": "ci",
            "keyHash": "9f2c0d",
            "prefix": "fp-abcdefg",
            "createdAt": "2026-10-19T00:00:00Z"
        }]
    }))
    .unwrap()
}

fn empty_data() -> AppData {
    serde_json::from_value(json!({ "version": 1 })).unwrap()
}

#[test]
fn transfer_roundtrip_is_encrypted_verifiable_and_merge_safe() {
    // 密钥库是进程级单例，所以必须在第一次碰它之前定好数据目录
    let dir = std::env::temp_dir().join(format!("fp-transfer-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("FREEPORT_DATA_DIR", &dir);

    let src = source_data();
    secrets::set_key("acct_g1", "gsk-secret-one").unwrap();
    secrets::set_key("acct_g2", "gsk-secret-two").unwrap();
    secrets::set_key("acct_c1", "csk-secret-three").unwrap();

    let blob = transfer::export(&src, PASS, "Freeport test · macos").expect("导出应当成功");

    /* ── 1. 密文里不能出现明文 ─────────────────── */
    assert!(!blob.contains("gsk-secret-one"), "明文密钥泄漏进了迁移码");
    assert!(!blob.contains("acct_g1"), "连凭据 id 都不该以明文出现");
    let env: Value = serde_json::from_str(&blob).unwrap();
    assert_eq!(env["format"], "freeport.transfer");
    assert_eq!(env["v"], 1);
    assert_eq!(env["kdf"], "argon2id");

    /* ── 2. 口令错误 ──────────────────────────── */
    let err = transfer::open(&blob, "wrong passphrase").unwrap_err();
    assert!(err.contains("口令不对"), "错误文案要能区分「打错了」：{err}");

    /* ── 3. 密文被改一个字节 ───────────────────── */
    let mut tampered = env.clone();
    let ct = tampered["ct"].as_str().unwrap().to_string();
    let mut bytes = ct.into_bytes();
    // 翻转 base64 里的一个字符（保持在字母表内，避免变成「格式损坏」）
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };
    tampered["ct"] = Value::String(String::from_utf8(bytes).unwrap());
    let err = transfer::open(&tampered.to_string(), PASS).unwrap_err();
    assert!(err.contains("被改过") || err.contains("损坏"), "{err}");

    /* ── 4. KDF 参数被恶意放大 ─────────────────── */
    let mut bomb = env.clone();
    bomb["m"] = json!(4_000_000);
    let err = transfer::open(&bomb.to_string(), PASS).unwrap_err();
    assert!(err.contains("KDF 参数"), "不能照着信封里的数字分配内存：{err}");

    /* ── 5. 正常导入到空配置 ───────────────────── */
    let body = transfer::open(&blob, PASS).expect("口令正确应当能解开");
    let mut dst = empty_data();
    let sum = transfer::apply(body, &mut dst, transfer::Mode::Merge);

    assert_eq!(sum.providers_added, 2);
    assert_eq!(sum.accounts_added, 3);
    assert_eq!(sum.keys_added, 1);
    assert_eq!(sum.secrets_written, 3);
    assert_eq!(sum.secrets_orphaned, 0);
    assert_eq!(dst.providers.len(), 2);
    assert_eq!(dst.routing.chain, vec!["groq:llama-3.3-70b"]);
    assert_eq!(dst.api_keys.len(), 1, "虚拟 key 的哈希要一起搬，否则原客户端全失效");

    // 搬过来的额度限额、启停状态都不能丢
    let g1 = dst.providers[0].account("acct_g1").unwrap();
    assert_eq!(g1.quota.and_then(|q| q.rpd), Some(1000));
    assert!(!dst.providers[0].account("acct_g2").unwrap().enabled);

    // 健康度与目录是本机历史，不该跟着走
    assert!(dst.providers[0].models.is_empty(), "模型目录要重拉，不跟着搬");
    assert_eq!(secrets::get_key("acct_g1").as_deref(), Some("gsk-secret-one"));

    /* ── 6. 再导入一次：幂等，不该长出重复条目 ────── */
    let body = transfer::open(&blob, PASS).unwrap();
    let sum = transfer::apply(body, &mut dst, transfer::Mode::Merge);
    assert_eq!(sum.providers_added, 0);
    assert_eq!(sum.accounts_added, 0);
    assert_eq!(sum.providers_kept, 2);
    assert_eq!(sum.accounts_kept, 3);
    assert_eq!(sum.keys_kept, 1);
    assert_eq!(dst.providers.len(), 2);
    assert_eq!(dst.api_keys.len(), 1);

    /* ── 7. 覆盖模式：本地独有条目与密文都要清掉 ───── */
    dst.providers.push(
        serde_json::from_value(json!({
            "id": "local-only",
            "name": "Local",
            "baseUrl": "https://local",
            "tier": "B",
            "accounts": [{
                "id": "acct_local",
                "name": "default",
                "keyMask": "sk-••••9999",
                "enabled": true
            }]
        }))
        .unwrap(),
    );
    secrets::set_key("acct_local", "sk-local-should-be-gone").unwrap();

    let body = transfer::open(&blob, PASS).unwrap();
    let sum = transfer::apply(body, &mut dst, transfer::Mode::Replace);
    assert_eq!(sum.mode, "replace");
    assert_eq!(dst.providers.len(), 2, "覆盖后只剩迁移码里的厂商");
    assert!(
        dst.providers.iter().all(|p| p.id != "local-only"),
        "本地独有厂商应当被替换掉"
    );
    assert!(
        secrets::get_key("acct_local").is_none(),
        "被替换掉的凭据不能把密文留在磁盘上"
    );
    assert_eq!(
        secrets::get_key("acct_c1").as_deref(),
        Some("csk-secret-three"),
        "迁移码里的 key 必须重新写入本机密钥库"
    );

    /* ── 8. 口令太短直接拒绝 ───────────────────── */
    let err = transfer::export(&src, "1234", "t").unwrap_err();
    assert!(err.contains("至少"), "{err}");

    /* ── 9. 墓碑：本机删过的厂商不该被合并回来 ───── */
    // 这是跨机同步最阴的一个 bug：昨天在 A 机删了 groq，今天在 B 机同步，
    // 它又回来了。墓碑就是让「删除」这个决定跟着数据走。
    let mut dst2 = empty_data();
    dst2.tombstones.bury_provider("groq");
    dst2.tombstones.bury_account("acct_c1");
    let body = transfer::open(&blob, PASS).unwrap();
    let sum = transfer::apply(body, &mut dst2, transfer::Mode::Merge);

    assert_eq!(sum.providers_skipped, 1, "groq 应当被墓碑挡掉");
    assert_eq!(sum.providers_added, 1, "没删过的 cerebras 仍然要进");
    assert!(dst2.providers.iter().all(|p| p.id != "groq"), "删掉的不许复活");
    assert!(
        dst2.providers
            .iter()
            .all(|p| p.accounts.iter().all(|a| a.id != "acct_c1")),
        "凭据级的墓碑同样要生效"
    );
    assert_eq!(sum.accounts_skipped, 3, "groq 的两条 + acct_c1");

    let _ = std::fs::remove_dir_all(&dir);
}
