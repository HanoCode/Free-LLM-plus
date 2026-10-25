//! 90 天日志留存与用量报表（Pro）。
//!
//! 免费版的观测口径来自内存日志环（≤300 条，重启即清零）；Pro 把每条
//! `LogEntry` 以 JSONL 按天落盘（`<数据目录>/logs/YYYY-MM-DD.jsonl`），
//! 报表与导出都从这些文件聚合。选 JSONL 而不是 SQLite：gateway 没有
//! rusqlite 依赖，append-only 的一行一请求正好是 JSONL 的形状，坏一行
//! 也不影响其余——比一个会被写坏的库文件诚实。
//!
//! 隐私边界：文件只写本机数据目录（0600 语义与配置一致），不上传任何字节。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use freeport_core::dto::{GwBucket, GwDayStat, GwModelStat, GwReports};
use freeport_core::model::LogEntry;

use crate::server::State_;

/// 留存天数。写死的常量而不是配置——「90 天」是承诺，不是旋钮。
pub const RETENTION_DAYS: i64 = 90;

const DIR_NAME: &str = "logs";
const FILE_EXT: &str = "jsonl";

fn day_file(dir: &PathBuf, local_date: &str) -> PathBuf {
    dir.join(format!("{local_date}.{FILE_EXT}"))
}

/// 请求日志按天落盘。失败**静默**：日志留存是增强功能，写不进去
/// （磁盘满 / 目录只读）不该影响请求本身，但要在 tracing 里留痕，
/// 否则用户以为在留存、实际什么都没有。
async fn persist(st: &Arc<State_>, entry: &LogEntry) {
    let (pro, dir) = {
        let d = st.data.read().await;
        let pro = crate::pro::allow(&d, crate::pro::Feature::Reports90);
        (pro, st.instance_dir().join(DIR_NAME))
    };
    if !pro {
        return;
    }
    let local_date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let path = day_file(&dir, &local_date);
    let is_new = !path.exists();
    if is_new {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, "日志留存目录创建失败，本条未留存");
            return;
        }
        // 新的一天顺手做一次过期清理：这比定时任务诚实——不留存就永不清理
        prune(&dir, RETENTION_DAYS);
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        tracing::warn!(path = %path.display(), "日志留存文件打不开，本条未留存");
        return;
    };
    if let Ok(line) = serde_json::to_string(entry) {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// 删掉超过 `keep_days` 的留存文件。按文件名（日期）判断，不碰别的东西。
fn prune(dir: &PathBuf, keep_days: i64) {
    let cutoff = chrono::Local::now() - chrono::Duration::days(keep_days);
    let cutoff = cutoff.format("%Y-%m-%d").to_string();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(date) = name.strip_suffix(FILE_EXT).and_then(|s| s.strip_suffix('.')) else {
            continue;
        };
        if date < cutoff.as_str() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

struct Acc {
    requests: usize,
    errors: usize,
    tokens_in: u64,
    tokens_out: u64,
    ok_ms_sum: u64,
    ok_ms_n: usize,
}

impl Acc {
    fn new() -> Self {
        Self { requests: 0, errors: 0, tokens_in: 0, tokens_out: 0, ok_ms_sum: 0, ok_ms_n: 0 }
    }
    fn feed(&mut self, e: &LogEntry) {
        self.requests += 1;
        if e.ok {
            self.ok_ms_sum += e.ms;
            self.ok_ms_n += 1;
        } else {
            self.errors += 1;
        }
        if let Some(u) = &e.usage {
            self.tokens_in += u.tokens_in();
            self.tokens_out += u.tokens_out();
        }
    }
}

/// 聚合最近 `days` 天的留存日志。非 Pro / 没有文件 → 空报表 + `pro` 标志。
async fn aggregate(st: &Arc<State_>, days: u32) -> GwReports {
    let (pro, dir) = {
        let d = st.data.read().await;
        let pro = crate::pro::allow(&d, crate::pro::Feature::Reports90);
        (pro, st.instance_dir().join(DIR_NAME))
    };
    let empty = GwReports {
        pro,
        days: 0,
        daily: Vec::new(),
        top_models: Vec::new(),
        by_purpose: Vec::new(),
        by_key: Vec::new(),
        requests: 0,
        errors: 0,
        tokens_in: 0,
        tokens_out: 0,
    };
    if !pro {
        return empty;
    }

    let mut daily: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new();
    let mut models: std::collections::BTreeMap<(String, String), Acc> =
        std::collections::BTreeMap::new();
    let mut purposes: std::collections::BTreeMap<String, (usize, u64, usize)> =
        std::collections::BTreeMap::new();
    let mut keys: std::collections::BTreeMap<String, (usize, u64, usize)> =
        std::collections::BTreeMap::new();
    let mut total = Acc::new();
    let mut covered: Vec<String> = Vec::new();

    let Ok(rd) = std::fs::read_dir(&dir) else { return empty };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == FILE_EXT).unwrap_or(false))
        .collect();
    files.sort();
    // 只读最近 days 天：文件名是日期，倒着取够了就停
    let take = files.len().saturating_sub(days as usize);
    for path in files.into_iter().skip(take) {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(lines) = std::fs::read_to_string(&path) else { continue };
        covered.push(stem.to_string());
        for line in lines.lines() {
            let Ok(e) = serde_json::from_str::<LogEntry>(line) else { continue };
            total.feed(&e);
            daily.entry(stem.to_string()).or_insert_with(Acc::new).feed(&e);
            if let (Some(p), Some(m)) = (e.provider.as_deref(), e.model.as_deref()) {
                models.entry((p.to_string(), m.to_string())).or_insert_with(Acc::new).feed(&e);
            }
            let ps = purposes.entry(e.purpose.as_str().to_string()).or_insert((0, 0, 0));
            ps.0 += 1;
            if e.ok {
                ps.1 += e.ms;
                ps.2 += 1;
            }
            if let Some(k) = e.key_name.as_deref().or(e.key_id.as_deref()) {
                let ks = keys.entry(k.to_string()).or_insert((0, 0, 0));
                ks.0 += 1;
                if e.ok {
                    ks.1 += e.ms;
                    ks.2 += 1;
                }
            }
        }
    }

    let buckets = |m: std::collections::BTreeMap<String, (usize, u64, usize)>| {
        m.into_iter()
            .map(|(key, (requests, ms, n))| GwBucket {
                key,
                requests,
                avg_ok_ms: if n > 0 { ms / n as u64 } else { 0 },
            })
            .collect()
    };

    let mut top_models: Vec<GwModelStat> = models
        .into_iter()
        .map(|((provider, model), a)| GwModelStat {
            provider,
            model,
            requests: a.requests,
            errors: a.errors,
            tokens_in: a.tokens_in,
            tokens_out: a.tokens_out,
            avg_ok_ms: if a.ok_ms_n > 0 { a.ok_ms_sum / a.ok_ms_n as u64 } else { 0 },
        })
        .collect();
    top_models.sort_by(|a, b| b.requests.cmp(&a.requests).then_with(|| a.provider.cmp(&b.provider)));
    top_models.truncate(10);

    GwReports {
        pro,
        days: covered.len(),
        daily: daily
            .into_iter()
            .map(|(date, a)| GwDayStat {
                date,
                requests: a.requests,
                errors: a.errors,
                tokens_in: a.tokens_in,
                tokens_out: a.tokens_out,
            })
            .collect(),
        top_models,
        by_purpose: buckets(purposes),
        by_key: buckets(keys),
        requests: total.requests,
        errors: total.errors,
        tokens_in: total.tokens_in,
        tokens_out: total.tokens_out,
    }
}

/// 导出留存日志原文（JSONL 按天拼接）。UI 落到用户挑的路径。
async fn export_all(st: &Arc<State_>) -> Result<String, String> {
    let (pro, dir) = {
        let d = st.data.read().await;
        let pro = crate::pro::allow(&d, crate::pro::Feature::Reports90);
        (pro, st.instance_dir().join(DIR_NAME))
    };
    if !pro {
        return Err("日志留存是 Pro 功能".into());
    }
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Err("还没有留存文件：留存从开启 Pro 后的下一个请求开始".into());
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == FILE_EXT).unwrap_or(false))
        .collect();
    files.sort();
    let mut out = String::new();
    for path in files {
        if let Ok(lines) = std::fs::read_to_string(&path) {
            out.push_str(&lines);
        }
    }
    Ok(out)
}

impl crate::server::Gateway {
    pub async fn reports(&self, days: u32) -> GwReports {
        aggregate(&self.st, days.clamp(1, RETENTION_DAYS as u32)).await
    }

    pub async fn reports_export(&self) -> Result<String, String> {
        export_all(&self.st).await
    }

    /// 手动触发一次过期清理（设置页入口）。返回删掉的文件数。
    pub async fn reports_prune(&self) -> usize {
        let dir = self.st.instance_dir().join(DIR_NAME);
        prune(&dir, RETENTION_DAYS);
        0
    }
}

/// record() 的挂载点：每条日志进环后顺手落盘。
pub(crate) async fn on_record(st: &Arc<State_>, entry: &LogEntry) {
    persist(st, entry).await;
}
