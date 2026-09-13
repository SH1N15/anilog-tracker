# 版本发布

本文档供 AniLog 维护者发布 Windows 和 Android 正式版本时使用。

> **流程范围：** `v0.6.0` 起的正式版使用 Tauri 2 构建链。旧 `dist:all` 和 `android/` Gradle 命令只用于 v0.5 Electron/Capacitor 回退，不得用于新版本发布。

### Tauri 正式构建

Windows 两个 edition 必须串行构建，并在每次构建后用包含 edition 和版本号的文件名暂存 NSIS 安装包：

```powershell
npm run tauri:build
npm run tauri:build:original
```

Android 必须显式使用兼容的 JBR/JDK 21。本机 JDK 17 会出现 Java 21 源版本错误，JDK 25 会在 Gradle/AGP 后期失败。标准版和 Original 会写入同一 APK 输出路径，因此每次构建后必须立即复制暂存；Original 的实际包名由 `ANILOG_ANDROID_EDITION=original` 在 Gradle 中切换：

```powershell
npm run tauri:android:build
npm run tauri:android:build:original
```

两条 Android 发布命令均固定传入 `--target aarch64`。签名后按第 4 节逐个验证 ABI、包名、`versionName`、`versionCode`、证书指纹和对齐状态。beta/rc 必须标记为 Pre-release；正式版必须为非 Pre-release，并在发布成功后标记为 Latest。

## 1. 更新版本号

发布新版本前，至少检查以下位置：

- `package.json` 与 `package-lock.json` 中的桌面版本号
- `src-tauri/Cargo.toml` 与 `src-tauri/Cargo.lock` 中的 crate 版本号
- `src-tauri/tauri.conf.json` 中的 `version` 和 `bundle.android.versionCode`
- 生成的 `src-tauri/gen/android/app/tauri.properties` 与同目录 `build.gradle.kts` 中最终写入的 Android 版本
- Android 网络请求使用的 AniLog User-Agent 版本
- README 中指向最新版 Android APK 的链接
- `AGENTS.md`、`docs/MAINTAINER_HANDOFF.md` 中的正式版本、ABI 与回归入口，以及本机忽略的交接文档
- `release-notes/vX.Y.Z.md` 发布说明

`android/app/build.gradle` 属于 v0.5 Capacitor 回退工程，不是 Tauri 正式版的版本源。只有维护 v0.5 回退发布时才检查该文件。

Android 每次发布必须增加 `versionCode`。覆盖升级还要求使用与旧版本相同的发布密钥。
Tauri 的预发布标识不会自动产生不同的 Android `versionCode`，因此 beta、rc 和正式版也必须逐次手动增加 `bundle.android.versionCode`。
例如 `v0.7.4-rc.1` 使用 `12`，正式 `v0.7.4` 使用 `13`；正式包必须重新构建，不能只给 rc 文件改名。

只发布原名版时，标准版可以继续指向上一版本；发布说明必须明确每个附件所属版本，避免用户误认为标准版也已更新。

## 2. 验证代码

先安装锁定版本的依赖，再执行 Tauri 正式版构建和测试：

```powershell
npm ci
npm run tauri:build
npm run tauri:build:original
npm run tauri:android:build
npm run tauri:android:build:original
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

`build:all`、`build:android`、`build:android-original` 和 `dist:all` 仍保留给 v0.5 Electron/Capacitor 回退验证，不能作为 v0.6.0 Tauri 发布流程的替代命令。

涉及 WebDAV 时，除自动测试外，建议使用测试账户完成一次 Windows 与 Android 双向同步验证。测试账户不得写入仓库。
涉及 Android 桥接、调度或任务时，还要分别以 Standard/Original 运行
`src-tauri/gen/android/gradlew.bat -p src-tauri/gen/android :app:testUniversalDebugUnitTest --console=plain`。
只有界面预览或桌面 Cargo 测试通过，不能替代 Android 原生验证。

## 3. 构建 Windows 安装包

```powershell
npm run tauri:build
npm run tauri:build:original
```

两个 edition 会复用 Tauri bundle 输出目录，因此必须串行构建并在每次构建后立即复制为发布文件名：

- `AniLog-Windows-vX.Y.Z-x64-setup.exe`
- `AniLog-Original-Windows-vX.Y.Z-x64-setup.exe`

使用 PE VersionInfo 检查标准版 `ProductName=AniLog`、Original `ProductName=AniLog Original`，并确认两者的 `ProductVersion` 都与发布版本一致。Windows 安装包没有商业代码签名时，发布说明中应保留“未知发布者”提示。

原名版需验证应用内中英文切换，升级安装不能覆盖已保存的界面语言；不要把旧 Electron 安装器的语言选择行为当作 Tauri 安装器已经具备的能力。

## 4. 构建并签名 Android APK

使用 Tauri 构建仅含 arm64-v8a 的标准版 Release APK，并立即复制暂存：

```powershell
npm run tauri:android:build
```

随后构建 Original，并在覆盖共同输出路径前确认标准版已暂存：

```powershell
npm run tauri:android:build:original
```

发布文件名使用 `AniLog-Android-vX.Y.Z-arm64-v8a.apk` 和 `AniLog-Original-Android-vX.Y.Z-arm64-v8a.apk`。标准版包名是 `io.anilog.android`，Original 包名是 `io.anilog.android.original`，两者签名要求相同。

签名密钥和密码只能保存在维护者的安全备份中，不得提交到 Git、发布附件或问题讨论。后续 Android 更新必须继续使用同一密钥。

使用 Android SDK Build Tools 验证签名、版本号和对齐状态：

```powershell
apksigner verify --verbose --print-certs AniLog-Android-vX.Y.Z-arm64-v8a.apk
aapt dump badging AniLog-Android-vX.Y.Z-arm64-v8a.apk
zipalign -c -P 16 -v 4 AniLog-Android-vX.Y.Z-arm64-v8a.apk
```

确认 ZIP 中只存在 `lib/arm64-v8a/libanilog_lib.so`，不得包含 `armeabi-v7a`、`x86` 或 `x86_64`。同时确认签名证书指纹与上一个正式版本一致，并确认 `versionCode`、`versionName` 正确。

## 5. 生成校验值

```powershell
Get-FileHash -Algorithm SHA256 `
  'release\tauri-vX.Y.Z\AniLog-Windows-vX.Y.Z-x64-setup.exe', `
  'release\tauri-vX.Y.Z\AniLog-Original-Windows-vX.Y.Z-x64-setup.exe', `
  'release\tauri-vX.Y.Z\AniLog-Android-vX.Y.Z-arm64-v8a.apk', `
  'release\tauri-vX.Y.Z\AniLog-Original-Android-vX.Y.Z-arm64-v8a.apk'
```

把四个 SHA-256 值写入对应发布说明。

仅发布原名版时，校验 `AniLog-Original-Windows-vX.Y.Z-x64-setup.exe` 与 `AniLog-Original-Android-vX.Y.Z-arm64-v8a.apk` 两个文件即可。

## 6. 提交并发布

1. 确认 `git status` 不包含签名密钥、密码、缓存、本地数据或构建目录。
2. 提交版本号、代码、文档和发布说明，在 PR CI 通过后合并到 `main`。
3. 从已合并的 `main` 源码重新构建并验证四个附件。把最终 SHA-256 回填发布说明；若需追加纯文档提交，必须确认应用源码、依赖和构建配置未变，再将 `vX.Y.Z` 标签指向最终发布提交。
4. 标题使用 `AniLog vX.Y.Z`，正文使用对应的发布说明。
5. 上传本次版本对应的 Windows 安装包和已签名 Android APK；仅更新原名版时只上传两个 Original 附件。
6. 正式版取消 Pre-release，标记为 Latest 并发布；beta/rc 只标记 Pre-release。
7. 发布后重新下载四个附件并校验 SHA-256，同时核对 Latest、标签提交、远端 README 和本地交接记录。只收到上传成功响应不算验证完成。

不要覆盖旧版本附件；每个版本使用独立标签和文件名，以便用户回退与校验。
