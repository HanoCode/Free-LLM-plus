# Freeport 最终用户许可协议（EULA）

最后更新：2026-10-20

Freeport（下称「本软件」）由 HanoCode 及贡献者发布。安装或使用本软件，即表示你接受本协议。如果你不同意，请不要安装或使用。

---

## 1. 许可范围

本软件以 **AGPL-3.0-or-later** 发布。你可以在该许可证条款下自由使用、修改、再分发，包括商业使用；若你向他人提供修改后的版本（含以网络服务形式提供），须按 AGPL-3.0 提供对应源码。

本协议不限制、也不替代 AGPL-3.0 赋予你的权利；两者冲突时，以 AGPL-3.0 为准。本协议约束的是「使用本软件接入第三方模型服务」这一行为本身。

## 2. 你带来的凭据

本软件本身不提供模型服务。你在其中填入的 API Key、OAuth 令牌、Web 会话等，均是**你本人合法持有**的凭据，并且：

1. 你已阅读并遵守对应服务商的条款与配额规则；
2. 你**不得**用本软件共享账号、转售免费额度、绕开付费墙，或把凭据提供给他人使用；
3. 你理解：把凭据注入到团队仓库的项目级配置里，等于把它提交进版本历史——本软件默认只写客户端认定的「个人本地文件」，但最终提交什么由你决定。

凭据在本机加密存储（AES-256-GCM，密钥文件权限 0600），**不会上传到任何服务器**。

## 3. 本软件不会做的事

- 不代注册账号，不伪造设备指纹，不模拟浏览行为以绕过风控；
- 不读取系统浏览器的 Cookie 库（Web 会话只能由你手动粘贴）；
- 不把任何第三方服务包装成自己的服务；
- 不采集遥测。日志、崩溃记录只写在本机数据目录。

## 4. 第三方服务

通过本软件发起的每一次请求都由你直接发往对应服务商，受该服务商的条款与隐私政策约束。上游的额度变更、限流、封禁与本软件无关，本软件只能如实报告失败原因与建议的恢复时间。

## 5. 不提供担保

本软件按「现状」提供，不附带任何明示或暗示的担保，包括但不限于适销性、特定用途适用性与不侵权。你自行承担使用本软件的后果，包括因上游条款变化导致的账号风险。

在适用法律允许的最大范围内，作者与贡献者不对任何间接、附带、特殊或后果性损害承担责任。

## 6. 数据与卸载

所有数据位于本机应用数据目录（macOS：`~/Library/Application Support/dev.freeport.app`；Windows：`%APPDATA%\dev.freeport.app`）。卸载前可在设置页导出迁移包；卸载后该目录不会自动删除，删除它即彻底清除厂商、凭据与账本。

注入到 Agent 配置里的条目可通过注入页逐项回滚；回滚会还原到你注入前的原文件内容（含字节级缩进）。

## 7. 变更

本协议可能随版本更新。变更会随发布说明公布，继续使用即视为接受新版本。

## 8. 联系方式

问题与漏洞：<https://github.com/HanoCode/Free-LLM-plus/issues>

---

# Freeport End User License Agreement (summary)

Freeport is licensed under **AGPL-3.0-or-later**. This EULA does not restrict those rights; it governs the act of using Freeport to reach third-party model providers.

You must only use credentials you lawfully hold, and must not share accounts, resell free quota, or bypass paywalls. Credentials are encrypted at rest on your machine and never uploaded. Freeport does not create accounts, forge device fingerprints, read your browser's cookie store, or collect telemetry.

The software is provided **as is**, without warranty. Third-party services remain governed by their own terms. All local data lives in the app data directory and is removed only when you delete it.
