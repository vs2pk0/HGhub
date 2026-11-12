# 上游安全审查与合并记录（2026-11-11）

## 审查结论与基线

审查目标为 `https://github.com/vs2pk0/HGhub/tree/main`，取得的 main 提交为 `b5890765782aea839ba4bdeffa22fbffe6f408ca`。本地原始分支为 `security-audit`，提交 `555a521`。两者共同祖先为 `ee12e496`；上游有 11 个本地未包含的提交，本地有 30 个上游未包含的提交。

在本次源代码、依赖清单、安装/构建脚本、原生权限、网络与账号入口的静态检查中，**未发现能证明主观恶意的后门、挖矿、隐蔽远程命令执行、读取本机密钥/通讯录/短信或向无关站点外传资料的代码**。这不等于证明作者动机，也不等于审计了线上服务、所有第三方依赖或已发布安装包。不能仅凭源码结论为已有二进制背书。

上游新增的主要是 `native/android`、`native/ios` 和原生构建工作流。Android 仅声明 INTERNET 权限；iOS 的后台音频声明与桌面小窗播放功能对应。业务账号发往黄果两个固定站点，镜像地址从 `huangguoai.pages.dev` 获取；构建中的腾讯、华为和阿里镜像是依赖下载入口。没有发现另设的隐蔽账号收集端点。站点本身及其返回的内容仍是外部信任边界。

仓库内 `gradle-wrapper.jar` 的 SHA-256 为 `2db75c40782f5e8ba1fc278a5574bab070adccb2d21ca5a6e5ed840888448046`，与 [Gradle 官方摘要](https://services.gradle.org/distributions/gradle-8.11.1-wrapper.jar.sha256) 一致。标准 `gradlew` 中用于参数引用的 eval 属于启动器逻辑，未发现额外载荷。

## 合并前修复

| 风险 | 触发方式 | 修复 |
| --- | --- | --- |
| 账号请求体被重定向重发 | 原生 HTTP 客户端默认跟随重定向，307/308 可能将密码信封重发至新目标 | 关闭传输层自动重定向；密码、Bearer token、变更请求均不跟随。游客逻辑 GET 保留最多五跳的校验与 POST 信封，媒体 GET 保留校验后的重定向 |
| 线路与会话竞态 | 登录等待期间切换线路，或旧注销晚于新登录返回 | 主线程一次性捕获 origin、token、会话代数；发送与保存使用同一快照；A→B→A 也会拒绝旧登录；旧注销不能清除新会话 |
| 原生 token 明文备份 | SharedPreferences/UserDefaults 中直接保存 token | Android 改为 Android Keystore AES-GCM，origin 作为认证附加数据；iOS 改用仅本机可用的 Keychain 项。移除旧明文字段；无法确认归属的旧会话需要重新登录，偏好、历史、收藏保留 |
| 内网地址与镜像 origin 校验不全 | IPv6、IPv4 变体、DNS 私网答案、镜像中夹带路径/凭据 | 补齐 URL、保留地址及纯 HTTPS origin 校验。Android 所有 API、封面、ExoPlayer 网络统一使用检查 DNS 答案的 OkHttp 客户端；iOS 每跳检查 URL 与 DNS。保留 `198.18.0.0/15` FakeIP |
| iOS 回环代理被任意本机页面驱动 | 固定 `/v?u=...` 路径且无连接数/接收超时边界 | 路径携带每次启动的随机 capability，限制并发与 socket 收发等待，复用目标/重定向检查，清单相对路径使用最终响应地址，保留原 SIGPIPE 修复 |
| 桌面导航误放行不透明来源 | 将媒体 CORS 的 `null` origin 复用于导航，导致 data/file 等地址进入同一判断 | 拆开导航判断；显式允许应用协议与前端 HTTP(S) 来源，拒绝 data/file/javascript 等导航；媒体 CORS 兼容性及用户显式开启的 HTTP(S) 兼容模式保留 |
| 桌面 Unix 会话文件被其他用户读取 | 旧 session.json 使用默认权限创建 | 应用数据目录收紧为 0700，会话文件创建、保存和旧文件恢复均设置为 0600 |
| 构建下载包未经完整性检查即运行 | 从镜像下载 Gradle 后直接解压执行 | wrapper 和原生发布工作流均固定官方 Gradle 8.11.1 zip SHA-256；校验失败会尝试下一个镜像。签名密钥通过 EXIT trap 清理，构建默认 token 只读，发布 job 单独授权写入 |

Gradle zip 固定值为 `f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6`，来源为 [Gradle 官方摘要](https://services.gradle.org/distributions/gradle-8.11.1-bin.zip.sha256)。

## 功能保留与合并方式

使用普通双亲合并保留双方提交历史，不以任一方覆盖整个目录。上游原生 Android/iOS、画中画、自动续集、换线和 SIGPIPE 修复全部保留。本地已有磁盘缓存、前台播放优先级、音轨/字幕同步缓存、严格模式/CIDR 设置、手势、播放诊断与历史续播保留。本地 `src/`、原有测试和播放脚本没有因同步被回退；新增前端变更仅为安全测试命令。Tauri 和原生移动端分别保留各自版本号。

新增 `.github/workflows/checks.yml` 在 PR、push、merge_group 和手动触发时运行，使用只读权限，不依赖发布签名密钥。工作流覆盖桌面构建/Rust/浏览器回归、原生 Android debug 编译与安全测试、iOS 模拟器编译及 Swift 回归。这为合并队列提供可重复的检查入口；是否启用 GitHub auto-merge、分支保护及必需检查由目标仓库设置决定，本地提交不能替代服务端策略。

## 验证结果

- `pnpm build`：通过，包含应用与现有测试 TypeScript 检查；有既有的 JS 包体积提示。
- `cargo check --locked --offline --manifest-path src-tauri/Cargo.toml`：通过。
- Rust 本地单元测试：54 passed / 0 failed / 9 ignored。9 个默认跳过项中包括 5 个真实联网测试、3 个回环测试和 1 个浏览器桥接 fixture；随后单独运行了 3 个回环测试，均通过。
- `pnpm test:cache`：23 项通过。
- `pnpm test:security`：70 项通过，包含两套用户代理的图片协议分支和安全设置保存/失败回归；这仍是在 Chromium 中模拟平台，不是原生 macOS/Windows 实测。
- `pnpm test:api-loading`：首页与分类的 2 组错误、重试、按需加载回归通过。
- `pnpm test:playback`：16 个场景通过，包含真实 Rust 缓存桥接、MP4 范围请求复用、HLS、字幕、历史恢复、片尾回拖、自动续集与失效存储；测试拦截外部请求，不发送真实账号。
- 原生 Android：使用隔离安装、按官方摘要校验的 JDK 17 / Gradle 8.11.1 和 Android SDK 36 完成 `:app:assembleDebug :app:testDebugUnitTest`，生成 debug APK，4 项安全测试全部通过（0 failed / 0 errors）。覆盖密码与 token 不跟随重定向、游客 POST 信封保持、私网跳转拒绝、混合 DNS 答案、IPv4/IPv6 变体及 FakeIP。没有使用发布签名密钥，也未在真机运行 Android Keystore 或播放器。
- 原生 iOS：本机是 Linux；后续 [GitHub macOS CI](https://github.com/kkylin-si/HGhub/actions/runs/43035957245/job/130574692216) 已通过模拟器编译、Swift 安全回归与 SIGPIPE 测试。尚未在 iOS 真机验证 Keychain/AVPlayer。
- workflow YAML、XML、plist、Python 脚本语法及 `git diff --check`：通过。

## 剩余边界

iOS 的 DNS 检查发生在 URLSession 连接前，不能像 Rust 的 resolve_to_addrs 或 Android 的自定义 Dns 一样固定最终连接地址；DNS 重绑定、可信系统代理/TUN 的最终解析仍需依赖平台与代理配置。未声称它具备与 Rust 完全相同的 DNS 固定保证。

桌面 session.json 仍是当前用户可读的明文，Unix 已限制权限，Windows 依赖应用数据目录的用户 ACL；同一用户权限下的恶意进程不在此保护范围内。旧日志、旧备份或已复制的诊断不会被本次升级追溯清除。第三方依赖与业务响应的资源消耗仍需持续维护，本次未将已有媒体能力改成不兼容的大小限制。

## CI 后续修正

首次推送后的 Android job 在 SDK 准备阶段失败：`android-actions/setup-android@v3` 默认安装 `tools platform-tools`，其中旧包 `tools` 已无法从 SDK 仓库获取。工作流显式设置 `packages: platform-tools`，保留后续 Android 36 / build-tools 36.0.0 安装；不修改应用功能或测试内容。此修复推送到 fork 的 `security-audit` 分支重新验证；未创建 PR、发布安装包或变更目标 main 及仓库保护设置。

同时处理 Node.js 20 action 运行时弃用警告：所有工作流中的 checkout、setup-node、setup-java、pnpm setup、Android setup、artifact 上传/下载和 release action 分别迁移到已核对声明 Node 24 的 v6、v6、v5、v5、v4、v6/v7、v3。应用构建仍使用原先配置的 Node.js 22、JDK 17 和 pnpm 10；Rust 缓存 action 的 v2 已声明 Node 24，因此保持原版本。

桌面回归 runner 调整为正式可用的 `ubuntu-26.04`。原 24.04 运行的依赖步骤曾持续超过 26 分钟；升级系统本身不保证网络下载更快，因此同时只安装缺失包、关闭推荐包安装、使用非交互安装，并给 APT 设置 30 秒连接超时和两次重试（后续 Playwright 安装也适用）。初始依赖步骤最多等待 10 分钟，索引刷新和包安装分别输出耗时。原有编译、媒体与浏览器回归检查保留；实际加速效果以新 runner 的云端耗时为准。
