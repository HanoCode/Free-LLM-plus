//! 配置持久化。
//!
//! 配置落 `freeport.json`，权限 0600；密钥由 `secrets` 模块单独保管，
//! 这里只存掩码（`keyMask`）用于展示。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use freeport_core::model::AppData;

const FILE_NAME: &str = "freeport.json";

pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FREEPORT_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .map(|d| d.join("dev.freeport.app"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_file() -> PathBuf {
    data_dir().join(FILE_NAME)
}

/// 测试用的进程级环境锁：谁动 `FREEPORT_DATA_DIR` 谁先拿它。
///
/// 为什么必须共享一把锁（而不是每个模块自己一把）：这个变量是**进程级**的，
/// 模块私有的锁彼此看不见。真实踩到的样子是——扩展用例正在往临时目录写
/// builtin manifest，隔壁 secrets 用例把目录改指到别处，于是 `find("asr")`
/// 拿不到，报成「扩展消失了」。看起来是扩展的问题，其实是两个用例在抢目录。
///
/// 用例跑完就删目录的也归这把锁管：删除和写入撞在一起同样会造出假故障。
#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // 某个用例 panic 会把锁毒掉，但那不该连坐下一条用例——它们本来也互不相干
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn load() -> AppData {
    load_from(&data_dir())
}

/// 从指定目录读配置。抽出来是为了让「tmp 兜底」能被单测覆盖——
/// 走 `FREEPORT_DATA_DIR` 的话，同一个进程里的测试会互相踩。
pub fn load_from(dir: &Path) -> AppData {
    let (mut data, recovered) = load_best(dir);
    // 安装码：第一次启动生成一次，之后跟着配置走。
    // 它只被许可证用来回答「这份 license 是不是本机的」，换个数据目录就是新机器。
    let mut dirty = false;
    if data.install_id.is_empty() {
        data.install_id = new_install_id();
        dirty = true;
    }
    // 加法式迁移：只补骨架、不动用户值。配置版本比本程序新时不迁移——
    // 迁移表只会往前走，往回跑等于把新版写下的字段抹掉。
    if data.version > freeport_core::model::SCHEMA_VERSION {
        tracing::warn!(
            "配置版本 {} 高于本程序支持的 {}，本次不迁移",
            data.version,
            freeport_core::model::SCHEMA_VERSION
        );
    } else if data.migrate() {
        tracing::info!("配置已迁移到 v{}", data.version);
        dirty = true;
    }
    if dirty || recovered {
        // 恢复出来的配置立刻固化成正式文件：tmp 只该活到下一次成功保存，
        // 不该成为「每次启动都靠它」的长期状态。
        save_to(dir, &data);
    }
    data
}

fn read_json(path: &Path) -> Option<AppData> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<AppData>(&raw).ok()
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// 取「最新的那份」配置。返回 `(数据, 是否来自 tmp)`。
///
/// 只靠「主文件能不能解析」不够：保存失败的典型形态是主文件完好但内容是旧的，
/// 新配置停在 tmp 里。这时必须比 mtime —— 否则下一次启动会把旧配置当成真相，
/// 还顺手把 tmp 覆盖掉，新数据就真没了（改完 schema 顺带 save 一次最容易踩）。
fn load_best(dir: &Path) -> (AppData, bool) {
    let main = dir.join(FILE_NAME);
    let tmp = main.with_extension("json.tmp");
    let m = read_json(&main);
    let t = read_json(&tmp);

    // 半截 tmp 解析不出来，自然被排除在外
    match (m, t) {
        (Some(a), Some(b)) => {
            let newer = matches!((mtime(&main), mtime(&tmp)), (Some(x), Some(y)) if y > x);
            if newer {
                tracing::warn!("freeport.json.tmp 比主文件新，采用临时文件（上次替换未生效）");
                (b, true)
            } else {
                (a, false)
            }
        }
        (Some(a), None) => (a, false),
        (None, Some(b)) => {
            tracing::warn!("已从 freeport.json.tmp 恢复配置（主文件不可用）");
            (b, true)
        }
        (None, None) => {
            if main.exists() {
                tracing::warn!("freeport.json 解析失败，回到默认配置");
            }
            let mut d = AppData::default();
            d.gateway.auth_token = new_token();
            d.stats.since = chrono::Utc::now().to_rfc3339();
            (d, false)
        }
    }
}

/// 落盘。返回是否真的写成功。
///
/// 失败必须留痕：磁盘满、目录只读、文件被别的进程占住，都会把「保存」
/// 变成静默丢配置——用户下次启动发现厂商全没了，日志里却一个字都没有。
pub fn save(data: &AppData) -> bool {
    save_to(&data_dir(), data)
}

pub fn save_to(dir: &Path, data: &AppData) -> bool {
    let path = dir.join(FILE_NAME);
    if let Err(e) = fs::create_dir_all(dir) {
        tracing::error!("建配置目录失败 {}：{e}", dir.display());
        return false;
    }
    let raw = match serde_json::to_string_pretty(data) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("序列化配置失败：{e}");
            return false;
        }
    };
    // 原子写：tmp + fsync + rename，权限 0600
    let tmp = path.with_extension("json.tmp");
    let mut f = match fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("写临时配置失败 {}：{e}", tmp.display());
            return false;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = f.write_all(raw.as_bytes()).and_then(|_| f.sync_all()) {
        tracing::error!("写配置失败 {}：{e}", tmp.display());
        return false;
    }
    drop(f);

    if let Err(e) = fs::rename(&tmp, &path) {
        // tmp 不删：下次 load 会拿它兜底，这份比磁盘上的旧配置新
        tracing::error!(
            "替换配置失败 {}：{e}（新配置保留在 {}）",
            path.display(),
            tmp.display()
        );
        return false;
    }
    true
}

pub fn new_token() -> String {
    format!("fp-{}", uuid::Uuid::new_v4().simple())
}

/// 安装码。12 位足够区分同一用户的两台机器，又不至于长到需要用户抄。
pub fn new_install_id() -> String {
    let s = uuid::Uuid::new_v4().simple().to_string();
    format!("inst_{}", &s[..12])
}

pub fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return format!("{}••••", chars.iter().take(2).collect::<String>());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fp-store-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn roundtrip_keeps_install_id() {
        let dir = tmpdir("rt");
        let mut d = AppData::default();
        d.install_id = "inst_test123456".into();
        assert!(save_to(&dir, &d));
        assert_eq!(load_from(&dir).install_id, "inst_test123456");
        let _ = fs::remove_dir_all(&dir);
    }

    /// rename 之前进程被杀 / 被沙箱挡下时，新配置停在 tmp 里。
    /// 这份比磁盘上的旧配置新，必须能恢复，否则用户的登录白做一次。
    #[test]
    fn recovers_from_tmp_when_main_is_corrupt() {
        let dir = tmpdir("recover");
        let mut d = AppData::default();
        d.install_id = "inst_fromtmp1234".into();
        fs::write(dir.join(FILE_NAME), b"{ not json").unwrap();
        fs::write(
            dir.join(format!("{FILE_NAME}.tmp")),
            serde_json::to_vec(&d).unwrap(),
        )
        .unwrap();

        assert_eq!(load_from(&dir).install_id, "inst_fromtmp1234");
        // 恢复之后要立刻固化：下一次启动不该还得靠 tmp
        let back = read_json(&dir.join(FILE_NAME)).expect("恢复后应写回正式文件");
        assert_eq!(back.install_id, "inst_fromtmp1234");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 反过来的情形：主文件好好的，tmp 是写了一半的垃圾。
    /// 这时信 tmp 会把配置降级，必须忽略它。
    #[test]
    fn ignores_half_written_tmp_when_main_is_fine() {
        let dir = tmpdir("half");
        let mut d = AppData::default();
        d.install_id = "inst_main123456".into();
        assert!(save_to(&dir, &d));
        fs::write(dir.join(format!("{FILE_NAME}.tmp")), b"{\"installId\":\"inst_ne").unwrap();

        assert_eq!(load_from(&dir).install_id, "inst_main123456");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 主文件合法但内容旧、tmp 更新：以 tmp 为准。
    ///
    /// 这是保存失败最常见的形态——主文件从没被破坏，只是没被替换成新内容。
    /// 只看「能不能解析」会把它当真相，还顺手把 tmp 覆盖掉。
    #[test]
    fn newer_tmp_wins_over_stale_main() {
        let dir = tmpdir("newer");
        let mut old = AppData::default();
        old.install_id = "inst_old0000000".into();
        assert!(save_to(&dir, &old));

        let mut new = AppData::default();
        new.install_id = "inst_new0000000".into();
        // 拉开 mtime：tmp 必须严格比主文件新
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(
            dir.join(format!("{FILE_NAME}.tmp")),
            serde_json::to_vec(&new).unwrap(),
        )
        .unwrap();

        assert_eq!(load_from(&dir).install_id, "inst_new0000000");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 两个都坏：回到默认配置并补一把网关 token，不能带着空 token 起来。
    #[test]
    fn falls_back_to_default_when_both_unreadable() {
        let dir = tmpdir("both");
        fs::write(dir.join(FILE_NAME), b"garbage").unwrap();
        fs::write(dir.join(format!("{FILE_NAME}.tmp")), b"garbage").unwrap();

        let d = load_from(&dir);
        assert!(d.gateway.auth_token.starts_with("fp-"));
        assert!(!d.install_id.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}

pub fn ensure_dir(path: &Path) {
    let _ = fs::create_dir_all(path);
}
