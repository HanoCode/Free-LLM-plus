# Freeport 最终用户许可协议（EULA）

最后更新：2026-10-25

Freeport（下称「本软件」）由 HanoCode 发布。安装或使用本软件，即表示你接受本协议。如果你不同意，请不要安装或使用。

---

## 1. 许可范围

本软件为**专有软件**（proprietary software），版权归 HanoCode 所有，保留全部权利。HanoCode 授予你**个人的、不可转让的、不可再许可的**使用许可，前提是你遵守本协议：

1. 你可以出于正常使用目的安装和运行本软件；
2. 你**不得**复制、分发、出租、转售或再许可本软件，也不得发布本软件的修改版或衍生版；
3. 你**不得**对本软件进行反向工程、反编译或反汇编，法律明确允许的除外；
4. 你**不得**绕过或规避本软件的任何技术保护措施或许可验证机制（包括但不限于许可证校验、功能门禁、设备绑定、激活与续签流程），也不得开发、传播或协助他人获取任何实施上述绕过的工具、补丁或修改版；
5. **Pro 功能**仅在持有有效许可证（激活码 / 许可证 token）期间可用。许可证按席位限制设备数，到期或吊销后 Pro 功能自动降级为免费版，免费功能不受影响；席位可在「授权管理」页自助解绑。

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

Freeport is **proprietary software**, © HanoCode, all rights reserved. HanoCode grants you a personal, non-transferable license to install and run it as-is. You may not redistribute, resell, sublicense, or publish modified or derivative builds. You may not reverse engineer the software or circumvent its license verification and technical protection measures (activation, device binding, feature gating) beyond what the law explicitly allows.

Pro features require a valid license. Licenses are seat-limited and degrade to the free tier on expiry or revocation; free features are unaffected.

You must only use credentials you lawfully hold, and must not share accounts, resell free quota, or bypass paywalls. Credentials are encrypted at rest on your machine and never uploaded. Freeport does not create accounts, forge device fingerprints, read your browser's cookie store, or collect telemetry.

The software is provided **as is**, without warranty. Third-party services remain governed by their own terms. All local data lives in the app data directory and is removed only when you delete it.
