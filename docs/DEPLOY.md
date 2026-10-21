# 无界面部署（容器 / NAS / systemd）

Freeport 有两种宿主：

| 宿主 | 二进制 | 用来做什么 |
|---|---|---|
| 桌面 | `Freeport.app`（Tauri） | 日常用：界面、注入、订阅登录 |
| 无界面 | `freeport`（`freeport-daemon`） | 长期跑：容器、NAS、小主机、CI |

无界面宿主**没有 HTTP 管理接口**，这是刻意的：一个能改配置的远程口子等于把
你的额度交给网络。要改配置有两条路——挂载数据目录用桌面版改，或者启动时用
`--add` 覆盖。

---

## 一、Docker

```bash
cp .env.example .env          # 填 FREEPORT_ADD
docker compose up -d
docker compose logs -f        # 看到 FREEPORT_READY port=8787 就是起来了
curl http://127.0.0.1:8787/v1/models
```

镜像只编 `freeport-daemon`，不含 WebKit。构建上下文由 `.dockerignore` 裁过，
不会把 `node_modules` / `target` 传到 daemon。

### 端口为什么是 `127.0.0.1:8787:8787`

冒号左边是**主机侧**绑定地址。写成 `8787:8787` 等于把网关开到局域网——任何
能连到你机器的人都能白用你的额度。容器内必须绑 `0.0.0.0` 才能被 docker 转发，
所以镜像里设了 `FREEPORT_ALLOW_NON_LOOPBACK=1`；这个开关只解除「进程拒绝绑
非回环」，不解除任何网络隔离。

要让**同一台机器上的其他容器**连过来，别改端口发布，改成共享网络：

```yaml
services:
  freeport:
    networks: [ai]
    # 删掉 ports，只留 expose
    expose: ["8787"]
  my-app:
    networks: [ai]
    environment:
      OPENAI_BASE_URL: http://freeport:8787/v1
networks:
  ai:
```

### 数据都在一个卷里

`/data` 里有：`config.json`、`vault.key`、`secrets.json`、`guard.json`、扩展目录。

- **备份这个卷 = 备份全部凭据**。`vault.key` 丢了，`secrets.json` 就是一坨随机字节。
- 别的机器上恢复：把卷内容原样放回去即可，不需要重新登录。

```bash
docker run --rm -v freeport-data:/data -v "$PWD":/backup alpine \
  tar czf /backup/freeport-data.tgz -C /data .
```

---

## 二、systemd（不用容器）

```ini
# /etc/systemd/system/freeport.service
[Unit]
Description=Freeport gateway
After=network-online.target

[Service]
ExecStart=/usr/local/bin/freeport --data-dir /var/lib/freeport --quiet
Environment=RUST_LOG=info,freeport_probe=warn
User=freeport
Restart=on-failure
# 只允许它写自己的数据目录
ProtectSystem=strict
ReadWritePaths=/var/lib/freeport
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

`ExecStart` 里不加 `--bind`，默认就是 `127.0.0.1`。

---

## 三、参数与环境变量

| 参数 | 环境变量 | 说明 |
|---|---|---|
| `--data-dir <路径>` | `FREEPORT_DATA_DIR` | 数据目录 |
| `--port <端口>` | `FREEPORT_PORT` | 监听端口，默认 8787 |
| `--bind <地址>` | `FREEPORT_BIND` | 默认 `127.0.0.1`；非回环要额外给 `--allow-non-loopback` |
| `--allow-non-loopback` | `FREEPORT_ALLOW_NON_LOOPBACK=1` | 解除「拒绝绑非回环」 |
| `--add id=key` | `FREEPORT_ADD=id=key,id=key` | 启动前写入凭据，可重复 |
| `--compress` | `FREEPORT_COMPRESS=1` | 打开工具输出压缩（会丢信息） |
| `--print-token` | — | 把本地鉴权 token 打到 stdout |
| `--mcp` | — | 以 MCP stdio 服务器运行，不起网关 |
| `--quiet` | — | 只输出最终结果 |

### 给脚本抓的两行

```
FREEPORT_READY port=8787
FREEPORT_TOKEN fp-xxxxxxxx
```

第二行只在 `--print-token` 时出现。本地鉴权默认**关**（因为只绑回环），
你开了 `requireAuth` 之后客户端才需要带上它。

### `--add` 的写法

```bash
freeport --add groq=gsk_xxx --add cerebras=cs_yyy
```

只按**第一个** `=` 切分，所以 key 里带 `=`（base64 填充）不会被截断。
上游当时打不通也不影响启动：凭据先落盘，之后第一次真实调用会把它救回来。

---

## 四、能做什么 / 不能做什么

能做：

- `/v1/*` 全部协议面：chat / responses / messages / gemini / ollama / 媒体端点
- MCP（`--mcp`，stdio）
- 巡检、账本、冷却、路由——和桌面版同一套代码路径

不能做：

- 改配置（加凭据、改路由、开关厂商）：挂载卷用桌面版改，或重启时 `--add`
- 订阅登录（Claude / Codex 授权码）：需要浏览器回调到本机回环，容器里没有
  可用的浏览器会话。请在桌面版登录好，再把数据卷复制过去
- 界面：还没有。Web UI 需要一层「命令 → HTTP」的桥，目前没做
