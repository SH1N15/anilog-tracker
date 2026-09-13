# Tauri 架构与迁移说明

AniLog 自 `v0.6.0` 起正式采用 Tauri 2，当前正式版为 `v0.7.4`。现有 `electron/`、`android/` 与 v0.5.0 构建流程暂时保留为回退路径，后续清理必须作为独立任务规划。

## 架构

- `src/`：两端共用的 React 界面和类型。
- `src/platform/tauri.ts`：React 到 Tauri command 的平台适配层。
- `src-tauri/src/lib.rs`：Rust 共享核心，负责状态、AniList、季度缓存、追番、观看任务、Bangumi 标题和 WebDAV 合并。
- `src-tauri/src/mobile.rs`：Rust 与 Android 原生插件的桥接。
- `src-tauri/src/mobile_state.rs`：完整原生快照、任务时间戳和本机派生日程合并。
- `src-tauri/gen/android/`：Tauri Android 工程；AlarmManager、WorkManager、通知、Keystore 和 WebDAV 传输继续由 Android 原生代码实现。

标准版启用 Cargo feature `standard`，以 Bangumi 为季度条目和逐集身份的主数据源，构建内置 `bangumi-data` 映射与元数据，AniList 只为已匹配集数补充精确时间。Bangumi 季度链无缓存且不可用时保留 AniList 回退。Original 启用 `original`，使用空映射并在 Rust、前端和 Android 三层阻断 Bangumi 请求。两种 feature 不应同时启用。

## 已接通的功能

- Windows 托盘、关闭到托盘、`--hidden` 开机启动、后台 AniList 同步、播出通知和每日待看摘要。
- Android 后台调度、播出通知、待看任务开关、旧 Capacitor 原生记录恢复和应用私有数据目录。
- 原子保存、旧 Electron JSON 迁移、季度缓存和清理缓存。
- WebDAV 冲突合并；Windows 密码使用 Credential Manager，Android 密码使用 Keystore。
- Windows 首次迁移会读取旧 Electron 的 `webdav-config.json`，通过 Chromium `Local State` 与 Windows DPAPI 解密旧密码，再转存到 Credential Manager；如果密码无法解密，仍会保留地址和用户名并提示重新输入。
- WebDAV 启动后同步一次，本地追番或任务变化后延迟 5 秒合并同步，空闲时最多每 15 分钟检查一次；同步锁会阻止手动与自动同步并发，未完整配置凭据时不会发起请求。
- 标准版与 Original 的 Windows、Android 包名和前端资源隔离。
- Windows 隐藏启动或关闭到托盘后会销毁 WebView2，仅保留 Rust 后台进程；再次打开时按需重建界面。
- Windows 托盘只响应一次左键抬起事件，左键不会打开右键菜单，并使用窗口重建锁防止连续点击创建重复窗口。
- Windows 新番与每日待看通知注册激活回调；应用仍在后台运行时，点击系统通知会重建并聚焦主窗口。
- Windows 标准版与 Original 分别保持单实例；重复启动会恢复已有窗口，不会重复启动后台任务。
- Windows 可隐藏托盘图标，后台同步和通知保持不变；再次启动快捷方式可恢复已有窗口。
- 新番列表可按本机星期分组，也可切换回完整列表。
- 中文标题解析链请求串行且至少间隔 450 ms，整体不可用时暂停 10 分钟；成功中文名缓存 180 天，未匹配的未来作品缓存 1 天，已播作品缓存 7 天。其他 Bangumi API 由各自客户端限流和缓存。
- `v0.7.4` 的完整 Android 快照合并、日期/时刻分离、分篇任务保留和进度修复，详见 [WATCH_STATE_REGRESSIONS.md](WATCH_STATE_REGRESSIONS.md)。

## 本地数据

- Windows release：`<可执行文件目录>/data`；WebView2 的缓存和 localStorage 位于其中的 `webview-data`。
- Windows debug：Tauri 的应用数据目录。
- Android：系统分配的应用私有数据目录。

Android 的 AlarmManager 和 WorkManager 任务由系统持有，不要求应用进程常驻；系统可能在界面未打开时执行低频同步或提醒，具体时机受平台调度影响。

首次启动时，Windows 会尝试迁移旧 Electron `anilog-state.json`。Android 仅在新状态为空时读取旧版 SharedPreferences 中的追番、待看任务和提醒设置；仅存在于旧 WebView localStorage 的已完成任务不能直接恢复，WebDAV 用户可从同步文件恢复完整数据。

## 开发与测试

需要 Node.js 22、Rust 1.85 或更高版本。当前 Android 构建使用 JBR/JDK 21、Android SDK 36 和 NDK `27.2.12479018`。本机 JDK 17 和 JDK 25 均不用于正式构建。

```powershell
npm ci
npm run tauri:dev
npm run tauri:dev:original

cargo test --manifest-path src-tauri/Cargo.toml --features standard
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features original
```

Windows NSIS：

```powershell
npm run tauri:build
npm run tauri:build:original
```

输出位于 `src-tauri/target/release/bundle/nsis/`。

Android Debug APK：

```powershell
# JAVA_HOME must already point to a compatible JBR/JDK 21 installation.
$env:PATH="$env:JAVA_HOME\bin;$env:PATH"
$env:ANILOG_ANDROID_EDITION='standard'
npx tauri android build --debug --target aarch64 --features standard --apk --ci

$env:ANILOG_ANDROID_EDITION='original'
npx tauri android build --debug --target aarch64 `
  --config src-tauri/tauri.original.conf.json `
  --config src-tauri/tauri.android-original.conf.json `
  --features original --apk --ci
```

两个 Android 变体会写入同一个 APK 输出路径，连续构建时后一个会覆盖前一个；发布脚本必须在每次构建后立即复制并使用明确的版本文件名。正式发布命令固定使用 `--target aarch64`，正式 Release 附件只允许包含 `arm64-v8a`；Debug 配置可能保留多 ABI。

## 发布回归重点

- 在 Android 真机上验证首次启动、通知权限、准时调度、重启恢复、待看任务开关和厂商电池策略。
- 使用测试 WebDAV 账户完成 Windows 与 Android 的离线修改、冲突合并和凭据恢复。
- 验证从现有 Electron/Capacitor 正式版覆盖升级后的数据迁移。
- 使用既有发布密钥生成并验证两个 Android release APK；签名密钥不得提交到仓库。
- 在真实 Windows 注销或关机流程中确认不出现退出错误。
- 人工点击托盘图标确认销毁后的界面可以重建，并确认 Windows 通知点击行为。
- 确认 Tauri 安装路径可写并验证卸载、重装与 `data` 备份行为。

本机开发构建的抽样结果：隐藏启动约 19 MB 工作集，打开界面时 WebView2 进程树约 376 MB，关闭到托盘后约 30 MB。数值会随 WebView2 版本、页面内容和统计口径变化，只用于确认后台渲染器已释放，不作为正式性能承诺。

Tauri 已成为正式架构，但现阶段仍不删除旧架构。Electron/Capacitor 的删除条件、迁移终止版本和仓库清理应另行设计与验证。
