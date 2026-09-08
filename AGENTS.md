# AniLog AI 维护入口

本文件是后续 AI/自动化代理进入仓库时必须先读的项目约束。完整背景、架构、数据流、构建、发布说明见 [`docs/MAINTAINER_HANDOFF.md`](docs/MAINTAINER_HANDOFF.md)。

## 当前状态

- 当前正式版：`v0.7.2`，使用 React + Tauri 2 + Rust，共享 Windows/Android 业务核心，并标记为 GitHub Latest。
- Android 正式附件仅发布 `arm64-v8a`；Standard 与 Original 均不得回退为 universal APK。
- `electron/` 和 `android/` 继续作为 v0.5 回退实现保留；删除旧架构必须另行规划，不得夹带在普通修改中。
- 开始工作前先运行 `git status --short`，保留用户已有修改，不要擅自清理或重置。

## 强约束

- 标准版使用 Cargo feature `standard`；Original 使用 `original`。两者不能同时启用。
- Original 的 Rust、前端和 Android 三层都不得请求 Bangumi；标准版默认 Bangumi 反代为 `https://sh1n.cc.cd/v0`。
- 取消追番只删除对应作品的未完成任务；已完成任务必须作为观看历史保留。
- WebDAV 只同步 `following`、`tasks`、`followingDeletedAt`，不得同步设备设置、通知开关、缓存或凭据。
- WebDAV 凭据不得进入状态 JSON、日志、提交、Issue 或文档。Windows 密码使用 Credential Manager，Android 密码使用 Android Keystore。
- Android 的 `createWatchTasks=false` 只关闭手机端自动创建观看任务，不得关闭播出通知。
- Android 后台依赖 AlarmManager/WorkManager，不要求进程常驻；不要改造成高频轮询。
- Android 每次 beta、rc、正式发布都必须递增 `bundle.android.versionCode`，并使用与 v0.5.0 相同的发布证书，否则无法覆盖升级。不得提交密钥、alias、密码或本机签名路径。
- `src-tauri/tauri.android-original.conf.json` 不是 Original 实际包名的唯一依据；`ANILOG_ANDROID_EDITION=original` 会在 Gradle 中切换为 `io.anilog.android.original`。
- 不要为了统一数字而直接统一旧浏览器实现与 Rust 实现的 Bangumi resolver version；两条实现需分别分析。

## 修改与验证

- 依赖曾为释放磁盘空间而清理；首次构建先执行 `npm ci`。
- 标准版 Rust：`cargo test --manifest-path src-tauri/Cargo.toml --features standard`。
- Original Rust：`cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features original`。
- Windows 开发：`npm run tauri:dev` / `npm run tauri:dev:original`。
- Windows 构建：`npm run tauri:build` / `npm run tauri:build:original`。
- 改共享状态、任务、同步、通知或跨平台桥接时，必须验证两个 edition 和 Windows/Android 两端。
- 只改文档时检查 Markdown 链接和 `git diff --check`，无需恢复依赖或重新打包。
- 发布前阅读 [`docs/MAINTAINER_HANDOFF.md`](docs/MAINTAINER_HANDOFF.md) 和 [`docs/RELEASING.md`](docs/RELEASING.md)，并确认使用的是 Tauri 正式流程还是 v0.5 回退流程。

## 安全边界

- 不把 WebDAV 账户、应用专用密码、签名密钥及其本机路径或用户状态提交到 Git。
- 不自动提交、推送、合并 PR、发布 Release 或标记 Latest，除非用户明确授权。
- 构建产物可清理，但 `release/tauri-v0.6.0-beta.1`、`release/tauri-v0.6.0-beta.2`、`release/tauri-v0.6.0`、`release/tauri-v0.7.1` 和 `release/tauri-v0.7.2` 是本机发布安装包备份且受 Git 忽略。删除前应再次征得用户同意。
