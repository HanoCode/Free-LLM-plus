# 发布手册

> 这份文档只写**能照着做**的步骤。任何「等 CI 好了自然就有」的说法都不算步骤。

## 0. 一次性准备

### 0.1 macOS 签名与公证

需要三样东西，缺一样就只能发未签名包：

| 项 | 怎么拿 | 落到哪 |
|---|---|---|
| Developer ID Application 证书 | Apple Developer 后台 → Certificates → + → Developer ID Application | 导出 `.p12`，base64 后存 `APPLE_CERTIFICATE` |
| 证书密码 | 导出时设的 | `APPLE_CERTIFICATE_PASSWORD` |
| 签名身份 | `security find-identity -v -p codesigning` 里的完整名称，如 `Developer ID Application: Xxx (TEAMID)` | `APPLE_SIGNING_IDENTITY` |

公证二选一（推荐 App Store Connect API Key，因为不会因为改密码失效）：

- **API Key**：App Store Connect → 用户与访问 → 集成 → App Store Connect API → 生成密钥。
  下载的 `.p8` 文件内容放 `APPLE_API_KEY`，Issuer ID 放 `APPLE_API_ISSUER`。
- **Apple ID**：`APPLE_ID` + **App 专用密码**（不是登录密码）+ `APPLE_TEAM_ID`。

### 0.2 自动更新密钥（已生成，待配置 secrets）

密钥对**已经生成**，位置与用途如下：

- 私钥：`app/.secrets/tauri-updater.key`（已在 `.gitignore` 里，**永远不会进仓库**；
  把文件内容存进密码管理器后这份本地文件就只是备份）
- 公钥：已写进 `app/src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`，
  端点指向 `https://github.com/HanoCode/Free-LLM-plus/releases/latest/download/latest.json`
- 私钥密码：空（生成时 `-p ""`）

发版前把私钥配进 GitHub secrets：

| secret | 值 |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | `app/.secrets/tauri-updater.key` 的**完整文件内容**（含注释行） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 留空字符串即可（私钥无密码） |

没配私钥时，workflow 会自动跳过升级包，release 里不会出现验不过的 `latest.json`；
客户端的更新通道会报「不可达」，设置页自动回落到「打开 Releases 页」。

**私钥丢了就只能换一把新的**：老客户端会因为验签失败而拒绝更新，只能手动重装。
换钥匙 = 改 `tauri.conf.json` 的 pubkey + 重发一版。

### 0.3 Windows 签名（当前关闭）

Azure Trusted Signing 需要 Azure 订阅 + 身份验证 + 证书配置文件。
`.github/workflows/release.yml` 末尾有一段注释掉的签名步骤，配好
`AZURE_TENANT_ID` / `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET` /
`AZURE_TS_ENDPOINT` / `AZURE_TS_ACCOUNT` / `AZURE_TS_PROFILE` 后取消注释即可。

在此之前，Windows 包是**未签名**的：首次运行会弹 SmartScreen「Windows 已保护你的电脑」，
需要「更多信息 → 仍要运行」。这一点必须写进 release 说明，不能让用户以为包坏了。

### 0.4 secrets 清单

| secret | 必需 | 作用 |
|---|---|---|
| `GITHUB_TOKEN` | 自动 | 上传 release 资产（workflow 里的 `permissions: contents: write` 已开） |
| `APPLE_CERTIFICATE` | 签名时必需 | base64 的 p12 |
| `APPLE_CERTIFICATE_PASSWORD` | 签名时必需 | p12 密码 |
| `APPLE_SIGNING_IDENTITY` | 签名时必需 | 证书全名 |
| `APPLE_API_KEY` / `APPLE_API_ISSUER` | 公证时二选一 | API Key 方式 |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | 公证时二选一 | Apple ID 方式 |
| `TAURI_SIGNING_PRIVATE_KEY` / `..._PASSWORD` | 可选 | 给更新包签名（公钥已提交在 tauri.conf.json） |

> `TAURI_UPDATER_PUBKEY` 这个 secret 已废弃：公钥现在提交在 `tauri.conf.json`，
> workflow 只检查 `TAURI_SIGNING_PRIVATE_KEY` 是否存在。

## 1. 发一版

```bash
# 1) 三个版本号必须一致，verify job 会卡这一条
#    app/Cargo.toml [workspace.package] version
#    app/package.json version
#    app/src-tauri/tauri.conf.json version（tauri 会读 workspace，但保持同步更稳）

# 2) 本地先跑一遍，别让 CI 当第一道关
cd app
npm run typecheck && cargo test --workspace && npm run build:web

# 3) 打 tag（必须是 v 开头，workflow 用 tag 推版本号）
git tag v0.1.0
git push origin v0.1.0
```

CI 会产出 **draft release**，不做自动发布。draft 里应当有：

- `Freeport_0.1.0_aarch64.dmg` + `.sha256`
- `Freeport_0.1.0_x64.dmg` + `.sha256`
- `Freeport_0.1.0_x64-setup.exe` + `.sha256`
- 配了更新密钥时还有 `*.app.tar.gz` / `*.sig` 与 `latest.json`

## 2. 发之前必须验的四件事

在一台**没装过开发环境**的机器上：

```bash
# mac：签名与公证都过了没
codesign -dv --verbose=4 /Applications/Freeport.app
spctl -a -vvv -t install /Applications/Freeport.app     # 期望 accepted, source=Notarized Developer ID
xcrun notarytool log <submission-id> --key ...          # 只在被拒时看

# 校验和
shasum -a 256 -c Freeport_0.1.0_aarch64.dmg.sha256
```

清单：

1. **装完能开**：双击不报「已损坏」；首次启动网关自动起来（`lsof -i :8787` 有监听）。
2. **升级不丢数据**：旧版本里接入一个厂商 → 覆盖安装新版本 → 厂商、凭据、账本还在。
3. **卸载无残留**：删除 `.app`；数据目录 `~/Library/Application Support/dev.freeport.app`
   仍在（这是刻意的，凭据在里面），文档里要讲清楚「删目录才是彻底清除」。
4. **注入可回滚**：注入一个 Agent → 回滚 → 与注入前逐字节一致。

四条都过再点 publish。

## 3. 出问题怎么退

- **刚发出去发现严重 bug**：GitHub release 页面编辑 → 取消勾选「Set as the latest release」
  → 重新勾选上一个稳定版本为 latest。已下载的用户不会自动降级，所以同时在 release 正文置顶说明。
- **更新包签名错**：不要重打同一个 tag。删 tag（`git tag -d` + `git push --delete`）、
  修完提版本号、发新 tag。老客户端验签失败会安静地跳过这次更新，不会变砖。

## 4. 崩溃与诊断

- 崩溃现场写到 `<数据目录>/crash/crash-<时间>.log`，只留最近 20 份，不上传。
- 设置页「保存诊断包」导出一份脱敏 JSON：版本、厂商与健康、账本、最近 20 条日志、
  崩溃文件列表。**不含**明文密钥、不含掩码、不含网关 authToken。
- 用户报障时先要这份 JSON，比让他描述现象快得多。

## 5. 商业化口径：免费开放期

**当前状态（2026-10-25）**：`app/crates/freeport-gateway/src/pro.rs` 的
`FREE_FOR_ALL = true`，全部增强功能（报表 / 自动备份 / 会话持久化 / 远程接入等）
对所有用户免费。配套语义：

- `license::state()` 返回 `status = "free_era"`、`tier = "pro"`，以 Pro 形态示人；
- 前端不展示任何激活 / 订阅入口（`License.tsx` 已删），设置页的对应区块叫「增强功能」；
- 激活与订阅的**服务器**（`server-api/`）已在 v0.1 清理时整体移除——首发即全免费，
  它没有存在的必要。客户端侧机制（`freeport-license` + `gateway/license.rs`）保留但休眠；
  设备挑战-应答的 e2e 用例随服务器一起移除（git 历史可查），离线加固用例保留。

**翻回收费前必须核对**：

1. 激活 / 订阅服务已按 `docs/SUBSCRIBE-DESIGN.md` 重建并上线（v0.1 前的完整服务器实现
   在 git 历史，tag `v0.1.0` 之前的提交里可找回）；
2. `FREE_FOR_ALL` 改回 `false`，跑 `cargo test -p freeport-gateway --test license_e2e`
   与 `--test hardening_e2e`（防破解回归）；
3. 设置页「增强功能」的文案与 Pro 门禁重新对齐（部分开关的禁用态会回来）；
4. EULA 里的收费条款与实际定价一致。
