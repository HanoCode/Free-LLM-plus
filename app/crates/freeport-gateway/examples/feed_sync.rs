//! 手动同步一次目录 feed，并把结果打出来。
//!
//! GUI 里点「检查更新」也能做，但排查问题时需要一个能看见每一步的东西：
//! 第几次、seq 有没有推进、被安全阀拦了还是验签失败、新增了哪些模型。
//!
//! ```text
//! # 用临时数据目录，别动真实配置
//! FREEPORT_DATA_DIR=/tmp/fp-feed-test \
//! cargo run -p freeport-gateway --example feed_sync -- \
//!   --url http://127.0.0.1:8765/latest.json --pubkey <hex> --times 2
//! ```

use std::time::{Duration, Instant};

use freeport_core::dto::FeedPatch;
use freeport_gateway::Gateway;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut url = String::new();
    let mut pubkey = None;
    let mut times = 2usize;
    let mut wait = Duration::from_millis(800);
    let mut force = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--url" => {
                i += 1;
                url = argv.get(i).cloned().unwrap_or_default();
            }
            "--pubkey" => {
                i += 1;
                pubkey = argv.get(i).cloned();
            }
            "--times" => {
                i += 1;
                times = argv.get(i).and_then(|v| v.parse().ok()).unwrap_or(2);
            }
            "--wait" => {
                i += 1;
                let ms = argv.get(i).and_then(|v| v.parse().ok()).unwrap_or(800);
                wait = Duration::from_millis(ms);
            }
            "--force" => force = true,
            other => return Err(format!("未知参数 {other}")),
        }
        i += 1;
    }
    if url.trim().is_empty() {
        return Err("缺少 --url".into());
    }
    println!("数据目录：{}", freeport_gateway::store::data_dir().display());

    let gw = Gateway::new();
    gw.load().await;
    gw.set_feed(FeedPatch {
        enabled: Some(true),
        url: Some(url.clone()),
        channel: None,
        policy: Some("auto".into()),
        pubkey,
        check_minutes: Some(1),
    })
    .await;

    for n in 1..=times {
        if n > 1 {
            tokio::time::sleep(wait).await;
        }
        let t0 = Instant::now();
        let f = if force {
            // 用户看过差量、确认「就要这份」：跳过安全阀
            gw.feed_apply().await
        } else {
            gw.feed_check(true).await
        };
        println!(
            "\n[第 {n} 次] seq={} version={} 用时={} ms",
            f.seq,
            if f.version.is_empty() { "-" } else { &f.version },
            t0.elapsed().as_millis()
        );
        if let Some(e) = &f.last_error {
            println!("  错误：{e}");
        }
        if let Some(h) = &f.halted {
            println!("  安全阀：{h}");
        }
        if let Some(p) = &f.pending {
            println!("  待确认：seq {}", p.seq);
        }
        for e in f.events.iter().take(6) {
            println!("  · {}  {}", e.kind, e.title);
        }

        // 快照缓存是下一轮走增量的前提，顺手看一眼它在不在
        let snap = freeport_catalog::store::load_snapshot();
        match snap {
            Some(s) => println!("  基线快照 seq={}", s.seq),
            None => println!("  基线快照：无（下一轮只能下全量）"),
        }
    }

    Ok(())
}
