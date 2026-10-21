//! 目录服务端：**按 tier 下发不同的签名快照**。
//!
//! 这是 Pro 唯一真正需要服务端的地方。产品红线是「免费版功能完整，Pro 只卖时效」，
//! 所以服务端只做三件事：
//!
//! 1. 有有效许可证（签名 + 未过期）→ `live` 快照；否则 → `monthly` 快照。
//! 2. 快照内容 + 摘要 + 档位一起签名，客户端验签后照单全收。
//! 3. `?since=` 命中当前 seq 就回 304，让轮询几乎零成本。
//!
//! 在此之上还有一层**字段级**下发：许可证 `feat` 里点到哪几项能力，就把对应的
//! 字段包列进 manifest（见 [`Floor::feats_for`]）。包与快照分开传，所以快照的
//! 摘要、签名、增量补丁链一个字节都不用动。
//!
//! 它**不看** `x-freeport-tier` 头：那是客户端自述，用来让服务端知道该查哪份档；
//! 真正的判定只看 Bearer 能不能验过。头部说 pro 但 token 无效 → 仍然给免费包。
//!
//! 没有用户系统、没有数据库：许可证是无状态 JWT，席位统计留给将来的计费端。
//! 一个目录服务不该成为新的单点故障，所以它只依赖三个文件：`meta.json`、
//! `live.json`、`monthly.json`（可选 `patches/` 与 `feats/`）。

pub mod http;

use std::path::{Path, PathBuf};

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use freeport_catalog::{FeatRef, Manifest, PatchRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 快照目录的元数据。两份快照共享同一个版本与 seq——它们只是同一份内容的
/// 两个时效切片，不是两个版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub seq: u64,
    pub version: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_channel() -> String {
    "stable".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Free,
    Pro,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
        }
    }

    /// 快照文件名。档位与文件名绑定，避免用参数直接拼路径。
    pub fn file(self) -> &'static str {
        match self {
            Self::Free => "monthly.json",
            Self::Pro => "live.json",
        }
    }
}

/// 判定这份请求该拿哪一档。
///
/// `token` 为空 / 签名不对 / 已过宽限期 → 免费档。**绝不因为解析失败而报错**：
/// 目录服务挂了，客户端手里还有上一份快照，那比一个 500 好得多。
pub fn tier_of(token: Option<&str>, pubkey: &[u8], now: i64) -> (Tier, Option<freeport_license::Claims>) {
    let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return (Tier::Free, None);
    };
    match freeport_license::verify_claims(pubkey, t, now) {
        Some(c) if c.tier.eq_ignore_ascii_case("pro") => (Tier::Pro, Some(c)),
        _ => (Tier::Free, None),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 组装 manifest。`tier` 会进签名载荷——否则中间人能把免费包标成 Pro 包。
pub fn build_manifest(
    meta: &Meta,
    snapshot: &[u8],
    tier: Tier,
    url: &str,
    patches: Vec<PatchRef>,
    feat: Vec<FeatRef>,
) -> Manifest {
    Manifest {
        version: meta.version.clone(),
        channel: meta.channel.clone(),
        seq: meta.seq,
        published_at: meta.published_at.clone(),
        sha256: sha256_hex(snapshot),
        sig: String::new(),
        url: url.into(),
        tier: Some(tier.as_str().into()),
        // notes 不进签名：改个错别字不该要求重签
        notes: None,
        feat,
        patches,
    }
}

pub fn sign_manifest(m: &mut Manifest, key: &SigningKey) -> Result<(), String> {
    let payload = freeport_catalog::jcs::canonical(&m.signing_payload())
        .map_err(|e| format!("签名载荷无法规范化：{e}"))?;
    let sig = key.sign(payload.as_bytes());
    m.sig = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
    Ok(())
}

/// 一份已加载的目录发布。
pub struct Floor {
    pub meta: Meta,
    pub live: Vec<u8>,
    pub monthly: Vec<u8>,
    pub patches: Vec<(PatchRef, Vec<u8>)>,
    /// 字段包：`feats/<name>.json` → 原文。
    ///
    /// 这里**不预判谁会拿到它**：授权判断在 [`Floor::feats_for`] 里按
    /// 令牌的 `feat` 现算。发布端只管把包放进来，不对齐任何名单。
    pub feats: Vec<(String, Vec<u8>)>,
    pub dir: PathBuf,
}

impl Floor {
    /// 从目录读取。缺任何一份快照都算配置错误——**不做静默降级**：
    /// 一个只配了 live 的服务上线后，所有免费用户会拿到 404，
    /// 那种故障在日志里只是一串 404，很难联想到这里。
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, String> {
        let dir = dir.as_ref().to_path_buf();
        let meta_raw = std::fs::read(dir.join("meta.json"))
            .map_err(|e| format!("读不到 {}：{e}", dir.join("meta.json").display()))?;
        let meta: Meta = serde_json::from_slice(&meta_raw).map_err(|e| format!("meta.json 解析失败：{e}"))?;
        if meta.seq == 0 {
            return Err("meta.json 的 seq 不能为 0：0 表示「从没发布过」".into());
        }
        let live = std::fs::read(dir.join("live.json"))
            .map_err(|e| format!("读不到 live.json：{e}"))?;
        let monthly = std::fs::read(dir.join("monthly.json"))
            .map_err(|e| format!("读不到 monthly.json：{e}"))?;
        let mut patches = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("patches")) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                let Some((from, to)) = name.trim_end_matches(".json").split_once('-') else {
                    continue;
                };
                let (Ok(from), Ok(to)) = (from.parse::<u64>(), to.parse::<u64>()) else {
                    continue;
                };
                if let Ok(bytes) = std::fs::read(ent.path()) {
                    patches.push((
                        PatchRef {
                            from_seq: from,
                            to_seq: to,
                            url: format!("patch/{name}"),
                            sha256: sha256_hex(&bytes),
                        },
                        bytes,
                    ));
                }
            }
        }
        patches.sort_by_key(|(p, _)| p.from_seq);

        let mut feats = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("feats")) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                let Some(feat) = name.strip_suffix(".json") else {
                    continue;
                };
                // 名字要拼进 URL 再拼回路径：只放行一个保守的字符集，
                // 免得 `..%2f` 这种东西从名字里进来
                if feat.is_empty()
                    || feat.len() > 64
                    || !feat
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    tracing::warn!(%name, "字段包名字不合法，跳过");
                    continue;
                }
                if let Ok(bytes) = std::fs::read(ent.path()) {
                    feats.push((feat.to_string(), bytes));
                }
            }
        }
        feats.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(Self {
            meta,
            live,
            monthly,
            patches,
            feats,
            dir,
        })
    }

    /// 按令牌里的 `feat` 挑出这次该下发的包。
    ///
    /// **授权只看令牌**，不看 `x-freeport-tier`、也不看有没有 live 包：
    /// 一个 feat 里没写 `quota.intel` 的 Pro 令牌拿不到它，一个写了它的
    /// 令牌（哪怕档位字段被人改过）也照样拿得到——清单是服务端按签名内容算的。
    pub fn feats_for(&self, allowed: &[String]) -> Vec<(FeatRef, Vec<u8>)> {
        self.feats
            .iter()
            .filter(|(name, _)| {
                allowed.iter().any(|a| a.trim().eq_ignore_ascii_case(name))
            })
            .map(|(name, bytes)| {
                (
                    FeatRef {
                        name: name.clone(),
                        url: format!("feat/{name}.json"),
                        sha256: sha256_hex(bytes),
                    },
                    bytes.clone(),
                )
            })
            .collect()
    }

    pub fn feat_bytes(&self, name: &str) -> Option<&[u8]> {
        self.feats
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    pub fn snapshot(&self, tier: Tier) -> &[u8] {
        match tier {
            Tier::Free => &self.monthly,
            Tier::Pro => &self.live,
        }
    }

    /// 只保留「从客户端当前 seq 能接上」的那一环补丁。
    ///
    /// 不把整条链都发出去：客户端最多走 24 环，但它只需要下一环；
    /// 发整链会让 manifest 体积随历史线性增长。
    pub fn patches_for(&self, since: u64) -> Vec<PatchRef> {
        self.patches
            .iter()
            .filter(|(p, _)| p.from_seq == since && p.to_seq == self.meta.seq)
            .map(|(p, _)| p.clone())
            .collect()
    }

    pub fn patch_bytes(&self, name: &str) -> Option<&[u8]> {
        self.patches
            .iter()
            .find(|(p, _)| p.url == format!("patch/{name}"))
            .map(|(_, b)| b.as_slice())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freeport_license::Claims;

    const SEED: [u8; 32] = [7u8; 32];
    const LICENSE_SEED: [u8; 32] = [9u8; 32];

    fn key() -> SigningKey {
        SigningKey::from_bytes(&SEED)
    }

    fn pubkey_hex() -> String {
        hex::encode(key().verifying_key().to_bytes())
    }

    fn license(tier: &str, exp: i64) -> String {
        let k = SigningKey::from_bytes(&LICENSE_SEED);
        let mut c = Claims::pro("u@example.com", 30, 1_700_000_000);
        c.tier = tier.into();
        c.exp = exp;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let head = b64.encode(freeport_license::HEADER);
        let payload = b64.encode(serde_json::to_vec(&c).unwrap());
        let signed = format!("{head}.{payload}");
        let sig = k.sign(signed.as_bytes());
        format!("{signed}.{}", b64.encode(sig.to_bytes()))
    }

    fn license_pubkey() -> [u8; 32] {
        SigningKey::from_bytes(&LICENSE_SEED).verifying_key().to_bytes()
    }

    #[test]
    fn absent_or_bad_token_falls_back_to_free() {
        let pk = license_pubkey();
        assert_eq!(tier_of(None, &pk, 1_800_000_000).0, Tier::Free);
        assert_eq!(tier_of(Some("garbage"), &pk, 1_800_000_000).0, Tier::Free);

        // 用别的密钥签的 token：签名验不过
        let foreign = license("pro", 1_900_000_000);
        assert_eq!(tier_of(Some(&foreign), &[3u8; 32], 1_800_000_000).0, Tier::Free);
    }

    #[test]
    fn valid_pro_token_gets_the_live_floor() {
        let pk = license_pubkey();
        let t = license("pro", 1_900_000_000);
        let (tier, claims) = tier_of(Some(&t), &pk, 1_800_000_000);
        assert_eq!(tier, Tier::Pro);
        assert_eq!(claims.unwrap().sub, "u@example.com");
        assert_eq!(tier.file(), "live.json");
    }

    #[test]
    fn expired_beyond_grace_falls_back() {
        let pk = license_pubkey();
        // 到期后 8 天：超过 7 天宽限期
        let t = license("pro", 1_800_000_000);
        assert_eq!(
            tier_of(Some(&t), &pk, 1_800_000_000 + 8 * 86400).0,
            Tier::Free
        );
        // 到期后 3 天：仍在宽限期
        assert_eq!(
            tier_of(Some(&t), &pk, 1_800_000_000 + 3 * 86400).0,
            Tier::Pro
        );
    }

    /// tier 必须进签名：否则中间人能把免费包标成 Pro 包。
    #[test]
    fn manifest_signature_covers_the_tier() {
        let meta = Meta {
            seq: 12,
            version: "2026.10.21".into(),
            published_at: "2026-10-21T03:00:00Z".into(),
            channel: "stable".into(),
        };
        let snap = b"{\"seq\":12}";
        let mut m = build_manifest(&meta, snap, Tier::Pro, "snapshot/live.json", Vec::new(), Vec::new());
        sign_manifest(&mut m, &key()).unwrap();
        m.verify(&pubkey_hex()).expect("签名应当有效");

        let mut relabeled = m.clone();
        relabeled.tier = Some("free".into());
        assert!(
            relabeled.verify(&pubkey_hex()).is_err(),
            "把 pro 改成 free 也必须验不过"
        );

        // 快照被改一个字节也要挂
        let mut tampered = m.clone();
        tampered.sha256 = sha256_hex(b"other");
        assert!(tampered.verify(&pubkey_hex()).is_err());
    }

    /// 字段包清单也必须进签名：否则中间人能在免费包的 manifest 里
    /// 塞一个 `quota.intel`，而客户端只会去核对摘要——摘要正是他给的。
    #[test]
    fn manifest_signature_covers_the_feat_list() {
        let meta = Meta {
            seq: 12,
            version: "2026.10.21".into(),
            published_at: "2026-10-21T03:00:00Z".into(),
            channel: "stable".into(),
        };
        let refs = vec![FeatRef {
            name: "quota.intel".into(),
            url: "feat/quota.intel.json".into(),
            sha256: sha256_hex(b"{}"),
        }];
        let mut m = build_manifest(&meta, b"{}", Tier::Pro, "snapshot/live.json", Vec::new(), refs);
        sign_manifest(&mut m, &key()).unwrap();
        m.verify(&pubkey_hex()).expect("签名应当有效");

        let mut injected = m.clone();
        injected.feat.push(FeatRef {
            name: "model.scores".into(),
            url: "feat/model.scores.json".into(),
            sha256: sha256_hex(b"[]"),
        });
        assert!(
            injected.verify(&pubkey_hex()).is_err(),
            "额外塞一个字段包必须验不过"
        );

        // 把地址换到别的服务器同样要挂
        let mut moved = m.clone();
        moved.feat[0].url = "https://evil.example/x.json".into();
        assert!(moved.verify(&pubkey_hex()).is_err());
    }

    #[test]
    fn only_the_next_patch_hop_is_advertised() {
        let dir = std::env::temp_dir().join(format!("fp-catalogd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("patches")).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"seq":3,"version":"v3","channel":"stable"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("live.json"), b"{}").unwrap();
        std::fs::write(dir.join("monthly.json"), b"{}").unwrap();
        std::fs::write(dir.join("patches/2-3.json"), b"[]").unwrap();
        std::fs::write(dir.join("patches/1-2.json"), b"[]").unwrap();

        let f = Floor::load(&dir).unwrap();
        let hops = f.patches_for(2);
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].to_seq, 3);
        assert!(f.patches_for(1).is_empty(), "跳环不合法，必须回退全量");
        assert!(f.patch_bytes("2-3.json").is_some());
        assert!(f.patch_bytes("../meta.json").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 字段包的授权只看**令牌里的 feat 名单**：
    /// 免费档（没有名单）拿不到任何一份；Pro 令牌也只拿到自己写了的那些。
    #[test]
    fn feats_follow_the_token_not_the_published_folder() {
        let dir = std::env::temp_dir().join(format!("fp-catalogd-feat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("feats")).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"seq":5,"version":"v5","channel":"stable"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("live.json"), b"{}").unwrap();
        std::fs::write(dir.join("monthly.json"), b"{}").unwrap();
        std::fs::write(dir.join("feats/quota.intel.json"), br#"{"providers":{}}"#).unwrap();
        std::fs::write(dir.join("feats/model.scores.json"), b"[]").unwrap();
        // 名字里带路径分隔符的不能进来
        std::fs::write(dir.join("feats/bad name.json"), b"{}").unwrap();

        let f = Floor::load(&dir).unwrap();
        assert_eq!(f.feats.len(), 2, "不合法的名字要被拒之门外：{:?}", f.feats.iter().map(|(n, _)| n).collect::<Vec<_>>());

        assert!(f.feats_for(&[]).is_empty(), "没有名单就没有数据");

        let only_intel = f.feats_for(&["quota.intel".to_string()]);
        assert_eq!(only_intel.len(), 1);
        assert_eq!(only_intel[0].0.name, "quota.intel");
        assert_eq!(only_intel[0].0.url, "feat/quota.intel.json");
        assert_eq!(only_intel[0].0.sha256, sha256_hex(br#"{"providers":{}}"#));

        // 大小写不敏感：JWT 里的写法不受控
        let both = f.feats_for(&["QUOTA.INTEL".to_string(), "model.scores".into()]);
        assert_eq!(both.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
