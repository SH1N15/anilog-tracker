# 参与开发

感谢你改进 AniLog。提交修改前，请先在本地完成对应平台的构建和测试。

开始处理代码前，请先阅读 [AI 维护入口](AGENTS.md) 和 [开发与维护交接](docs/MAINTAINER_HANDOFF.md)。其中记录了 Tauri 迁移状态、双版本隔离、任务保留、WebDAV 合并、Android后台与签名连续性等不能破坏的约束。

## 环境要求

- Node.js 22
- npm
- Rust 1.85+
- Windows 10/11（运行和打包桌面版）
- JBR/JDK 21、Android SDK/compile target 36、NDK `27.2.12479018`（当前 Tauri Android 正式构建链）

旧 Capacitor 回退工程的 `android/app/build.gradle` 使用 Build Tools 36.1.0；这不是 Tauri 的 SDK 版本要求。

## 安装依赖

```powershell
npm ci
```

如果 Electron 下载受网络限制，可在 PowerShell 中临时使用镜像后重新安装：

```powershell
$env:ELECTRON_MIRROR='https://npmmirror.com/mirrors/electron/'
node node_modules\electron\install.js
```

## 开发运行

Windows 标准版：

```powershell
npm run tauri:dev
```

Windows 原名版：

```powershell
npm run tauri:dev:original
```

原名版的中英文文案集中使用 `src/i18n.ts` 的语言工具。新增用户可见文本时，应同时提供中文和英文，并验证标准版仍固定使用中文。

仅做浏览器预览时运行 `npm run dev:tauri:web` 或 `npm run dev:tauri-original:web`，地址通常为 `http://127.0.0.1:5173/`。浏览器不能验证托盘、原生通知或 Android 后台。旧 `npm run dev` / `dev:original` 启动的是 Electron 回退实现。

## Tauri 2 开发

Tauri 2 是当前 `v0.7.4` 的正式架构。运行两个桌面变体：

```powershell
npm run tauri:dev
npm run tauri:dev:original
```

验证 Rust 共享核心与 Tauri 前端：

```powershell
npm run build:tauri:web
npm run build:tauri-original:web
cargo test --manifest-path src-tauri/Cargo.toml --features standard
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features original
```

完整的 Windows、Android 构建方式和迁移限制见 [docs/TAURI_MIGRATION.md](docs/TAURI_MIGRATION.md)。

## 构建 Windows 版

构建 Tauri 标准版和 Original NSIS 安装包：

```powershell
npm run tauri:build
npm run tauri:build:original
```

安装包生成在 `src-tauri/target/release/bundle/nsis/`。旧 Electron 构建链暂时作为 v0.5.0 稳定版的回退路径：

```powershell
npm run dist
npm run dist:original
npm run dist:all
```

安装包生成在 `release` 目录。若 Electron 解压阶段报告 `EPERM`，可使用项目中已安装的 Electron 运行时：

```powershell
npx electron-builder --win nsis --config.electronDist=node_modules/electron/dist
```

## 构建 Android 版

Tauri Android 的标准版、Original 构建命令、NDK 要求和输出路径见 [docs/TAURI_MIGRATION.md](docs/TAURI_MIGRATION.md)。每个发布版本都必须增加 `src-tauri/tauri.conf.json` 中的 `bundle.android.versionCode`，并使用与现有正式版相同的密钥签名。

旧 Capacitor 构建链暂时保留为 v0.5.0 稳定版的回退路径：

首次构建前，请使用 Android Studio 安装所需 SDK，并确保 `android/local.properties` 指向本机 Android SDK。

```powershell
npm run android:sync
.\android\gradlew.bat -p android assembleStandardDebug
```

Debug APK 位于 `android/app/build/outputs/apk/standard/debug/app-standard-debug.apk`。

构建使用 AniList 原名、支持中英文界面的 Android Original：

```powershell
npm run android:sync:original
.\android\gradlew.bat -p android assembleOriginalDebug
```

Original Debug APK 位于 `android/app/build/outputs/apk/original/debug/app-original-debug.apk`，包名为 `io.anilog.android.original`，可与标准版同时安装。每次切换 Android 版本时都应先执行对应的 `android:sync` 命令，确保 Capacitor 网页资源与目标版本一致。

`android/local.properties`、APK、Gradle 缓存、构建目录和签名密钥均被 Git 忽略。不要把密码、应用专用密码、签名密钥或其他凭据提交到仓库。

## 测试

```powershell
npm run build:tauri:web
npm run build:tauri-original:web
cargo test --manifest-path src-tauri/Cargo.toml --features standard
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features original
npm run test:daily-task-reminder
npm run test:editions
npm run test:state-refresh
npm run test:task-retention
npm run test:window-lifecycle
npm run test:webdav-sync
npm run test:webdav-service
npm run test:cache-storage
npm run test:season-cache
npm run test:season-grouping
npm run test:data
npm run test:bangumi
npm audit --omit=dev --audit-level=high
```

`build:all`、`build:android`、`build:android-original` 和 `dist:all` 属于 v0.5 Electron/Capacitor 回退验证；不作为 Tauri 正式版主测试清单的替代。

修改 Android 桥接、任务或播出逻辑时，在兼容 JBR 21 会话中分别设置
`ANILOG_ANDROID_EDITION=standard` 和 `original`，串行运行：

```powershell
.\src-tauri\gen\android\gradlew.bat -p src-tauri/gen/android :app:testUniversalDebugUnitTest --console=plain
```

任务保留、时间精度、前后台快照和进度回归契约见
[docs/WATCH_STATE_REGRESSIONS.md](docs/WATCH_STATE_REGRESSIONS.md)。测试不得读取或修改真实 WebDAV 账户与安装目录状态。

修改共享状态、同步逻辑或跨平台接口时，应同时验证 Windows 与 Android 构建。只修改文档时无需重新打包安装程序，但应检查 Markdown 链接和命令是否正确。

## 提交修改

- 保持改动聚焦，避免夹带无关的格式化或生成文件。
- 为行为变化补充或更新测试。
- 提交前确认 `git status` 中没有缓存、密钥、APK 或本地数据。
- 在提交说明中简要描述用户可见的变化。

维护者的完整版本发布流程见 [docs/RELEASING.md](docs/RELEASING.md)。
