//! 维护者用的签名工具。**不进发布包**——`cargo build` 不会编译 examples，
//! 所以私钥相关的代码永远不会出现在用户拿到的二进制里。
//!
//! ```text
//! # 生成一对密钥（私钥自己收好，公钥填进 feed.rs 的 BUILTIN_PUBKEY）
//! cargo run -p freeport-catalog --example sign_feed -- --genkey
//!
//! # 给一份快照签名，产出 latest.json
//! cargo run -p freeport-catalog --example sign_feed -- \
//!   --key $FEED_KEY --snapshot snapshots/2026.10.19.2.json \
//!   --version 2026.10.19.2 --seq 2 --channel stable \
//!   --url snapshots/2026.10.19.2.json --tier pro --out feed/
//!
//! # 顺带生成一条增量补丁（客户端从 seq=1 可以直接拼到 2）
//! cargo run -p freeport-catalog --example sign_feed -- \
//!   --key $FEED_KEY --snapshot snapshots/2026.10.19.2.json \
//!   --version 2026.10.19.2 --seq 2 --patch-from snapshots/2026.10.19.1.json --out feed/
//! ```

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use rand_core::RngCore;
use freeport_catalog::feed::{
    diff, snapshot_digest, snapshot_value, PatchFile, PatchRef, Snapshot,
};
use freeport_catalog::{jcs, FeatRef, Manifest};
use sha2::{Digest, Sha256};

fn main() {
    let args = Args::parse();
    if args.genkey {
        let mut seed = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut seed);
        let sk = SigningKey::from_bytes(&seed);
        println!("secret(hex): {}", hex::encode(sk.to_bytes()));
        println!("pubkey(hex): {}", hex::encode(sk.verifying_key().to_bytes()));
        return;
    }

    let key = args.key.expect("缺少 --key（或先 --genkey 生成）");
    let sk = SigningKey::from_bytes(&decode32(&key));
    let snap_path = args.snapshot.expect("缺少 --snapshot");
    let bytes = std::fs::read(&snap_path).expect("读不到快照文件");

    let url = args
        .url
        .clone()
        .unwrap_or_else(|| snap_path.file_name().unwrap().to_string_lossy().to_string());
    let seq = args.seq.expect("缺少 --seq");

    let out = args.out.clone().unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&out).unwrap();

    // 增量补丁：拿上一版快照做 diff，写成独立文件，摘要进 manifest
    let mut patches: Vec<PatchRef> = Vec::new();
    if let Some(prev_path) = args.patch_from.as_ref() {
        let prev_raw = std::fs::read(prev_path).expect("读不到 --patch-from 的那份快照");
        let prev: Snapshot = serde_json::from_slice(&prev_raw).expect("上一版快照不是合法快照");
        let next: Snapshot = serde_json::from_slice(&bytes).expect("新快照不是合法快照");
        assert_eq!(
            next.seq, seq,
            "新快照里的 seq 是 {}，和 --seq {seq} 不一致",
            next.seq
        );
        assert!(
            prev.seq > 0 && prev.seq < next.seq,
            "上一版 seq 必须大于 0 且小于新版（{} → {}）",
            prev.seq,
            next.seq
        );

        let ops = diff(&snapshot_value(&prev), &snapshot_value(&next));
        let pf = PatchFile {
            from_seq: prev.seq,
            to_seq: next.seq,
            ops,
            sha256_after: snapshot_digest(&next),
        };
        let raw = serde_json::to_vec(&pf).unwrap();
        let dir = args.patch_url.trim_end_matches('/').to_string();
        let rel = format!("{dir}/{}-{}.json", prev.seq, next.seq);
        let path = out.join(&rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &raw).unwrap();

        println!(
            "patch:    {}（{} 个操作，{} 字节）",
            path.display(),
            pf.ops.len(),
            raw.len()
        );
        if pf.ops.len() == 1 && pf.ops[0]["path"] == "" {
            println!("          注意：diff 退化成整体替换，这份补丁不比全量小");
        }

        patches.push(PatchRef {
            from_seq: pf.from_seq,
            to_seq: pf.to_seq,
            url: rel,
            sha256: hex::encode(Sha256::digest(&raw)),
        });
    }

    // 字段包：`--feats <dir>` 下每个 `<name>.json` 作为一份按 feat 下发的情报。
    // 只复制、不加工——摘要进 manifest，服务端照着文件原样吐出去。
    let mut feat: Vec<FeatRef> = Vec::new();
    if let Some(dir) = args.feats.as_ref() {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("读不到 --feats 目录")
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(str::to_string))
            .collect();
        names.sort();
        let dest = out.join("feat");
        std::fs::create_dir_all(&dest).unwrap();
        for name in names {
            let raw = std::fs::read(dir.join(format!("{name}.json"))).expect("读不到字段包");
            // 解析一次：发布一个语法坏掉的包，客户端只会把它记进 featsSkipped
            let _: serde_json::Value =
                serde_json::from_slice(&raw).expect(&format!("字段包 {name} 不是合法 JSON"));
            std::fs::write(dest.join(format!("{name}.json")), &raw).unwrap();
            feat.push(FeatRef {
                name: name.clone(),
                url: format!("feat/{name}.json"),
                sha256: hex::encode(Sha256::digest(&raw)),
            });
            println!("feat:     feat/{name}.json（{} 字节）", raw.len());
        }
    }

    let mut m = Manifest {
        version: args.version.expect("缺少 --version"),
        channel: args.channel,
        seq,
        published_at: chrono::Utc::now().to_rfc3339(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        sig: String::new(),
        url,
        tier: args.tier,
        notes: args.notes,
        feat,
        patches,
    };

    let payload = jcs::canonical(&m.signing_payload()).expect("签名载荷必须是可规范化的");
    m.sig = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        sk.sign(payload.as_bytes()).to_bytes(),
    );

    // 自己先验一遍：签错了当场发现，别等用户端报「签名无效」
    let pk = hex::encode(sk.verifying_key().to_bytes());
    m.verify(&pk).expect("自验签失败");

    let manifest_path = out.join("latest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&m).unwrap() + "\n",
    )
    .unwrap();

    println!("manifest: {}", manifest_path.display());
    println!("sha256:   {}", m.sha256);
    println!("pubkey:   {pk}");
    if let Some(parent) = Path::new(&m.url).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(out.join(parent));
        }
    }
    let copy = out.join(&m.url);
    if copy != snap_path {
        std::fs::write(&copy, &bytes).unwrap();
        println!("snapshot: {}", copy.display());
    }
}

fn decode32(hex_str: &str) -> [u8; 32] {
    let bytes = hex::decode(hex_str.trim()).expect("私钥必须是 hex");
    bytes.try_into().expect("私钥必须是 32 字节")
}

struct Args {
    genkey: bool,
    key: Option<String>,
    snapshot: Option<PathBuf>,
    version: Option<String>,
    seq: Option<u64>,
    channel: String,
    url: Option<String>,
    /// `free` / `pro`：标注这份快照按哪个档下发（会进签名）
    tier: Option<String>,
    notes: Option<String>,
    out: Option<PathBuf>,
    /// 上一版快照：给了就顺带生成一条增量补丁
    patch_from: Option<PathBuf>,
    /// 补丁在 manifest 目录下的相对目录
    patch_url: String,
    /// 字段包目录：每个 `<name>.json` 变成一份按 feat 下发的情报
    feats: Option<PathBuf>,
}

impl Args {
    fn parse() -> Self {
        let mut a = Args {
            genkey: false,
            key: None,
            snapshot: None,
            version: None,
            seq: None,
            channel: "stable".into(),
            url: None,
            tier: None,
            notes: None,
            out: None,
            patch_from: None,
            patch_url: "patches".into(),
            feats: None,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let k = argv[i].clone();
            let mut take = || -> String {
                i += 1;
                argv.get(i)
                    .cloned()
                    .unwrap_or_else(|| panic!("{k} 需要一个值"))
            };
            match k.as_str() {
                "--genkey" => a.genkey = true,
                "--key" => a.key = Some(take()),
                "--snapshot" => a.snapshot = Some(PathBuf::from(take())),
                "--version" => a.version = Some(take()),
                "--seq" => a.seq = Some(take().parse().expect("--seq 必须是整数")),
                "--channel" => a.channel = take(),
                "--url" => a.url = Some(take()),
                "--tier" => a.tier = Some(take()),
                "--notes" => a.notes = Some(take()),
                "--out" => a.out = Some(PathBuf::from(take())),
                "--patch-from" => a.patch_from = Some(PathBuf::from(take())),
                "--patch-url" => a.patch_url = take(),
                "--feats" => a.feats = Some(PathBuf::from(take())),
                other => panic!("未知参数 {other}"),
            }
            i += 1;
        }
        a
    }
}
