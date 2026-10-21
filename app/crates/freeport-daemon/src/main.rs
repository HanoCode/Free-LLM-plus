//! Freeport 的无界面宿主（`freeport` 二进制）。
//!
//! 桌面版把网关嵌在 Tauri 里，容器 / NAS / systemd 里没有窗口系统，
//! 也不该为了跑一个网关去装 WebKit。这个 crate 只做三件事：
//!
//! 1. 起网关（`Gateway::start`），把端口和鉴权 token 打到 stdout 方便脚本抓；
//! 2. 从环境变量 / 参数塞初始凭据（`--add id=key`），省得进容器还要点界面；
//! 3. 收 SIGTERM 干净退出（`docker stop` 默认发的是 TERM，不是 INT）。
//!
//! 刻意**不**做的事：不提供任何 HTTP 管理接口。要改配置就挂载数据目录、
//! 用桌面版改完再挂回去，或者重启时用 `--add` 覆盖。给容器开一个「改配置」
//! 的口子等于把额度交给网络，而这条产品线一直是「只绑回环」。

use std::process::ExitCode;

use freeport_core::dto::{ConfigPatch, ProviderInput};
use freeport_gateway::Gateway;

const HELP: &str = "\
freeport — 无界面网关

用法:
  freeport [选项]            起网关（默认）
  freeport --mcp             以 MCP stdio 服务器运行（不起网关）
  freeport --help            显示这段说明

选项:
  --data-dir <路径>          数据目录。相当于设 FREEPORT_DATA_DIR
  --port <端口>              监听端口。默认取配置文件里的值（8787）
  --bind <地址>              监听地址。默认 127.0.0.1
  --allow-non-loopback       允许 --bind 绑到非回环地址。容器里必须显式给
  --add <厂商>=<key>         启动前写入一条凭据，可重复
  --compress                 打开工具输出压缩（默认关：会丢信息）
  --print-token              启动后把本地鉴权 token 打到 stdout
  --quiet                    只输出最终结果，不输出逐条进度

环境变量:
  FREEPORT_DATA_DIR          同 --data-dir
  FREEPORT_PORT              同 --port
  FREEPORT_BIND              同 --bind
  FREEPORT_ALLOW_NON_LOOPBACK=1  同 --allow-non-loopback
  FREEPORT_ADD               `id=key,id=key` 形式，同多次 --add
  FREEPORT_COMPRESS=1        同 --compress
  RUST_LOG                   日志级别，默认 info
";

/// 回环判定。写成函数而不是 inline，是因为「什么算回环」在容器里很容易
/// 被想当然：`0.0.0.0` **不是**回环，它是「所有网卡」。
fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

struct Opts {
    data_dir: Option<String>,
    port: Option<u16>,
    bind: Option<String>,
    allow_non_loopback: bool,
    adds: Vec<(String, String)>,
    compress: bool,
    print_token: bool,
    quiet: bool,
    mcp: bool,
}

fn parse(args: &[String]) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        data_dir: std::env::var("FREEPORT_DATA_DIR").ok().filter(|s| !s.trim().is_empty()),
        port: std::env::var("FREEPORT_PORT")
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok()),
        bind: std::env::var("FREEPORT_BIND").ok().filter(|s| !s.trim().is_empty()),
        allow_non_loopback: matches!(
            std::env::var("FREEPORT_ALLOW_NON_LOOPBACK").as_deref(),
            Ok("1") | Ok("true")
        ),
        adds: Vec::new(),
        compress: matches!(std::env::var("FREEPORT_COMPRESS").as_deref(), Ok("1") | Ok("true")),
        print_token: false,
        quiet: false,
        mcp: false,
    };

    if let Ok(list) = std::env::var("FREEPORT_ADD") {
        for item in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            o.adds.push(split_add(item)?);
        }
    }

    let mut it = args.iter().skip(0).peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--version" | "-V" => {
                println!("freeport {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--mcp" => o.mcp = true,
            "--quiet" => o.quiet = true,
            "--print-token" => o.print_token = true,
            "--compress" => o.compress = true,
            "--allow-non-loopback" => o.allow_non_loopback = true,
            "--data-dir" => o.data_dir = it.next().cloned(),
            "--port" => {
                let v = it.next().ok_or("--port 后面要跟一个端口号")?;
                o.port = Some(v.parse().map_err(|_| format!("端口不是数字：{v}"))?);
            }
            "--bind" => o.bind = it.next().cloned(),
            "--add" => {
                let v = it.next().ok_or("--add 后面要跟 `厂商=key`")?;
                o.adds.push(split_add(&v)?);
            }
            other => return Err(format!("不认识的参数：{other}（用 --help 看用法）")),
        }
    }
    Ok(Some(o))
}

/// `groq=gsk_xxx` → ("groq", "gsk_xxx")。
/// 只按**第一个**等号切：key 里可能出现 `=`（base64 填充），按最后一个切会截错。
fn split_add(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((id, key)) if !id.trim().is_empty() && !key.trim().is_empty() => {
            Ok((id.trim().to_string(), key.trim().to_string()))
        }
        _ => Err(format!("--add 要写成 `厂商=key`，收到：{s}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse(&args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("freeport: {e}");
            return ExitCode::from(2);
        }
    };

    // 数据目录必须在任何 store/secrets 调用之前定下来：它们是按进程读这个变量的
    if let Some(dir) = &opts.data_dir {
        std::env::set_var("FREEPORT_DATA_DIR", dir);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,freeport_gateway=info,freeport_probe=warn".into()),
        )
        // stdout 留给 `FREEPORT_READY` 这类给脚本抓的行，日志一律走 stderr
        .with_writer(std::io::stderr)
        .init();

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("freeport: 起 tokio runtime 失败：{e}");
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async move {
        let gw = Gateway::new();
        gw.load().await;

        if let Some(host) = &opts.bind {
            if !is_loopback(host) && !opts.allow_non_loopback {
                eprintln!(
                    "freeport: 拒绝绑到 {host}——那会把网关暴露到本机之外。\n\
                     确实需要（例如容器里）请加 --allow-non-loopback，\
                     并在容器侧把端口发布成 127.0.0.1:8787:8787。"
                );
                return ExitCode::from(3);
            }
            gw.set_bind(host, opts.port.unwrap_or(0)).await;
        } else if let Some(p) = opts.port {
            let (host, _) = gw.bind_of().await;
            gw.set_bind(&host, p).await;
        }

        if opts.compress {
            gw.set_config(ConfigPatch {
                compress: Some(true),
                ..Default::default()
            })
            .await;
        }

        if opts.mcp {
            if let Err(e) = freeport_mcp::serve(&gw).await {
                eprintln!("freeport: MCP 退出：{e}");
                return ExitCode::FAILURE;
            }
            return ExitCode::SUCCESS;
        }

        // 初始凭据要在 start 之前写：start 之后配置已经进了运行期快照，
        // 再写只会改盘上的文件，本次进程里看不到
        for (id, key) in &opts.adds {
            match gw
                .add_provider(ProviderInput {
                    id: id.clone(),
                    name: None,
                    base_url: String::new(),
                    api_key: key.clone(),
                    tier: None,
                })
                .await
            {
                Ok(r) => {
                    if !opts.quiet {
                        eprintln!(
                            "freeport: 已接入 {id}（{} 个模型）",
                            r.model_count
                        );
                    }
                }
                // 上游暂时打不通不该让容器起不来：凭据已经落盘，
                // 下一轮巡检或第一次真实调用会把它救回来
                Err(e) => eprintln!("freeport: 接入 {id} 失败，已跳过：{e}"),
            }
        }

        match gw.start().await {
            Ok(port) => {
                let (host, _) = gw.bind_of().await;
                // 这一行是给 docker healthcheck / systemd 抓的，格式别随便改
                println!("FREEPORT_READY port={port}");
                if !opts.quiet {
                    if is_loopback(&host) {
                        eprintln!("freeport: 监听 http://{host}:{port}");
                    } else {
                        eprintln!(
                            "freeport: 监听 http://{host}:{port}（非回环，请确认外层已经限住来源）"
                        );
                    }
                }
                if opts.print_token {
                    let st = gw.status().await;
                    println!("FREEPORT_TOKEN {}", st.auth_token);
                }
            }
            Err(e) => {
                eprintln!("freeport: {e}");
                return ExitCode::FAILURE;
            }
        }

        // docker stop 发的是 SIGTERM，只等 ctrl_c 会让容器每次都要等 10 秒超时
        wait_for_stop().await;
        gw.stop().await;
        eprintln!("freeport: 已停止");
        ExitCode::SUCCESS
    })
}

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
