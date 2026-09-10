# AniLog 开发与维护交接

本文档面向后续维护者和 AI，记录项目事实、关键行为和发布约束。它不是用户使用手册，也不包含任何密码、密钥或账户信息。

> `v0.7.3` 已正式发布：在 `v0.7.2` 的 Bangumi 逐集播出权威基础上，修复 Android 季度缓存重启后失效、过期缓存阻塞首屏，以及窄屏详情页横向溢出。Standard 的逐集身份/`episodeId` 权威仍为 Bangumi `/v0/episodes`；AniList 只在确认同一集后提供分钟级时间。AniList 暂时 403 时不得用 `bangumi-data` 的周播锚点猜测后续集数；系统降级为最近一次可信 AniList 时间或日期级时间，恢复后自动纠偏。WebDAV 合并后必须用本地 Bangumi episode 缓存再愈合一次。

## 1. 项目现状

AniLog 是本地优先的 Windows/Android 追番工具，提供季度新番、追番更新通知、逐集观看任务和用户自有 WebDAV 双端同步。

- 仓库：`https://github.com/SH1N15/anilog-tracker`
- 正式架构：React + Tauri 2 + Rust
- 正式版：`v0.7.3`，GitHub Latest
- 当前开发基线：`v0.7.3`
- Android `versionCode`：`11`（仅 `arm64-v8a`）
- Android 正式 Release 附件 ABI：仅 `arm64-v8a`；Debug 配置仍可能包含其他 ABI，不能将正式包限制泛化到开发包

Tauri 已成为正式架构。`electron/` 和 `android/` 是 v0.5 Electron/Capacitor 的回退路径，删除条件和迁移终止版本应另行规划，不能夹带在普通修改中。

## 2. 目录与职责

| 路径 | 职责 | 状态 |
| --- | --- | --- |
| `src/` | React 共享界面、类型、国际化和平台适配入口 | 现行 |
| `src/platform/tauri.ts` | 前端调用 Tauri command 的接口层 | 现行 |
| `src-tauri/src/lib.rs` | Rust 共享核心：状态、AniList、季度缓存、追番、任务、中文标题、WebDAV、Windows 生命周期 | 现行 |
| `src-tauri/src/mobile.rs` | Rust 到 Android 原生插件的桥接 | 现行 |
| `src-tauri/gen/android/` | Tauri Android 工程；AlarmManager、WorkManager、通知、Keystore、WebDAV 传输 | 现行 |
| `electron/` | v0.5 Windows Electron 实现 | 稳定版回退，保留 |
| `android/` | v0.5 Android Capacitor 实现 | 稳定版回退，保留 |
| `scripts/` | 构建和行为回归脚本 | 共用 |
| `release-notes/` | 各版本发布说明 | 共用 |
| `docs/` | 迁移、发布和维护文档 | 共用 | 

修改共享业务行为时，先确认逻辑属于 React、Rust 还是 Android 原生层。不要在多个层重复实现同一规则，除非旧架构回退路径也需要同步修复。

## 3. Edition 模型

项目同时发布标准版和 Original。

| 项目 | 标准版 | Original |
| --- | --- | --- |
| Cargo feature | `standard` | `original` |
| 标题来源 | AniList + 本地映射 + Bangumi | 仅 AniList |
| 界面语言 | 中文 | 中文/英文 |
| 标题偏好 | 中文优先 | 英文/罗马字/日文可排序 |
| Windows 包名 | `io.anilog.desktop` | `io.anilog.desktop.original` |
| Android 包名 | `io.anilog.android` | `io.anilog.android.original` |

`standard` 和 `original` 不能同时启用。Original 必须在 Rust、前端和 Android 三层阻断 Bangumi 请求，不能只隐藏界面入口。

标准版的中文标题来源依次包含 `bangumi-data` 精简本地映射和 Bangumi API/反代。默认反代为 `https://sh1n.cc.cd/v0`，这是项目维护者自建服务。请求串行执行，间隔至少 450 ms；失败后暂停 10 分钟。成功标题缓存 180 天，未来作品未匹配缓存 1 天，已播作品未匹配缓存 7 天。没有可靠中文名的候选应过滤，不要用机器直译伪造正式标题。

注意：`src-tauri/tauri.android-original.conf.json` 中可能仍显示标准标识符。Android Original 的实际 application ID 由 `ANILOG_ANDROID_EDITION=original` 在 `src-tauri/gen/android/app/build.gradle.kts` 中切换，不能只根据 Tauri JSON 判断。

## 4. 业务数据与不变量

核心状态保存在 `anilog-state.json`，主要包含追番、任务、设置和同步元数据。以下规则属于产品契约：

- 新一集播出时，为追番作品创建未完成观看任务。
- 取消追番时，仅删除该作品的未完成任务。
- 已完成任务作为观看历史保留，取消追番也不能删除。
- 重复刷新、后台同步和通知回调不能产生重复任务。
- Android `createWatchTasks=false` 只禁止手机自动创建任务；播出通知仍应照常工作。
- 用户手动完成/恢复任务和跨端合并都要维护记录的更新时间。

改动追番删除或任务生成逻辑后，至少运行任务保留、状态刷新、WebDAV 合并相关测试，并人工验证“完成任务保留、未完成任务删除、刷新不重复”。

## 5. WebDAV 同步

WebDAV 使用用户自己的账户，远端文件固定为 `AniLog/anilog-sync.json`。文档版本为 `1`，最大接受 5 MB。

只同步：

- `following`
- `tasks`
- `followingDeletedAt` 删除墓碑

不予同步：

- 设备设置和界面偏好
- 通知开关与 Android 的本机任务开关
- AniList/Bangumi/图片/季度缓存
- WebDAV 地址、用户名、密码和其他凭据

合并时使用 `syncUpdatedAt` 或记录更新时间选择较新版本。删除墓碑用于防止另一设备把已取消的追番重新带回。新增、删除、完成任务和追番变化都必须更新同步时间。

Windows 启动后进行一次同步，本地变化会延迟合并，空闲时最多约 15 分钟检查一次。Windows 非密码配置存于 `webdav-tauri.json`，密码进入 Credential Manager。Android 密码进入 Android Keystore，原生层负责传输。不要在日志中打印 Authorization、完整响应文档或密码。

## 6. 后台与通知

### Windows

- 默认 AniList 检查间隔为 5 分钟。
- 开机自启应直接进入托盘，不显示主窗口。
- 左键单击托盘直接恢复单一主窗口；不应先闪现右键菜单，也不能生成两个窗口。
- 右键单击才显示托盘菜单。
- 点击新番通知应恢复并聚焦主窗口。
- 标准版与 Original 分别保持单实例；重复启动应恢复已有窗口，不得重复启动后台任务。
- 托盘图标可以隐藏；隐藏后后台同步与通知保持运行，再次启动快捷方式应恢复已有窗口。
- 系统关机/注销时应安静退出，不弹错误。

### Android

- 不要求应用进程常驻。
- WorkManager 当前周期同步间隔为 6 小时。
- AlarmManager 用于已知播出时间和每日待看提醒；这是为了可靠调度，不意味着高频访问 AniList。
- WorkManager/AlarmManager 由系统持有，即使用户没有打开界面也可能执行；厂商省电策略仍可能延迟后台任务，属于平台限制。
- Android 7.0+，`minSdk 24`。

## 7. 本地数据与迁移

| 平台/模式 | 数据位置 |
| --- | --- |
| Windows release | `<安装目录>\data` |
| Windows debug | Tauri 应用数据目录 |
| Android | 系统应用私有目录 |

季度数据位于 `season-cache`。正式 Tauri Standard 的 Bangumi 季度缓存保存在 `season-cache/bangumi-cache`，TTL 为 24 小时；即使过期也先显示可信快照，网络刷新在后台完成。旧浏览器/Capacitor 备用链的活跃季度 TTL 为 6 小时、历史季度为 30 天，并使用压缩快照和条目数限制，避免 Android WebView localStorage 配额导致重启后缓存丢失。不要把两条缓存链混为一谈。图片也由应用缓存管理，但缓存不参与 WebDAV。界面提供当前缓存大小和“清理缓存”。

Windows 会尝试迁移旧 Electron 状态和 WebDAV 非密码配置。Android 仅在新状态为空时迁移旧 Capacitor SharedPreferences。只存在旧 WebView localStorage 中的已完成任务无法直接迁移，这是当前已知限制。迁移逻辑必须可重复执行且不能覆盖已经存在的新状态。

## 8. 开发环境

通用要求：

- Node.js 22
- Rust 1.85+
- Windows 10/11
- Android 使用 JDK 17 或 21，不支持 JDK 25
- Android SDK 36
- Android NDK `27.2.12479018`

维护者本机曾验证的路径如下，仅供排障；其他机器应使用自己的安装路径，不要硬编码：

- Android SDK：`D:\Android\SDK`
- Android Studio/AVD 数据：`D:\Android\ASData`
- Android Studio JBR 21：`D:\Program Files\Android\Android Studio\jbr`
- 系统默认 `JAVA_HOME` 曾指向 `C:\Program Files\Java\jdk-17`，但 v0.7.3 正式 Android 构建实际使用上述 JBR 21
- Gradle wrapper：`src-tauri/gen/android/gradlew.bat`，版本 `8.14.3`
- Android NDK：`D:\Android\SDK\ndk\27.2.12479018`

v0.7.3 构建机实测版本为 Node.js `24.18.0`、npm `11.16.0`、Rust/Cargo `1.97.1`、OpenJDK `21.0.10`、GitHub CLI `2.96.0`。项目最低要求仍以锁文件、CI 和上方通用要求为准，不要把这组实测版本误当成唯一可用版本。

项目曾清理 `node_modules` 和 Rust/Android 大型构建输出以释放空间。因此新会话开始开发前通常需要：

```powershell
npm ci
```

## 9. 常用构建与测试

Windows 开发：

```powershell
npm run tauri:dev
npm run tauri:dev:original
```

Windows NSIS：

```powershell
npm run tauri:build
npm run tauri:build:original
```

Rust 两个 edition：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --features standard
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features original
```

Android Tauri：

```powershell
npm run tauri:android:dev
npm run tauri:android:build
npm run tauri:android:build:original
```

两条正式构建命令都固定使用 `--target aarch64`；发布 APK 必须只包含 `arm64-v8a`。

### 9.1 v0.7.3 实际构建顺序

Windows Standard 与 Original 共用 bundle 输出目录，Android 两个 edition 也共用 APK 输出路径，因此必须串行构建并在每一步结束后立即复制改名：

1. `npm ci`（依赖已存在且 lockfile 未变时可跳过）。
2. 运行 Standard/Original 两套 Rust 测试和 Node 回归门禁。
3. `npm run tauri:build`，立即复制 `src-tauri/target/release/bundle/nsis/AniLog_<版本>_x64-setup.exe`。
4. `npm run tauri:build:original`，立即复制 `src-tauri/target/release/bundle/nsis/AniLog Original_<版本>_x64-setup.exe`。
5. 在当前 PowerShell 会话设置 `$env:JAVA_HOME` 为兼容的 JDK/JBR 21。
6. `npm run tauri:android:build`，立即复制 `src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`。
7. `npm run tauri:android:build:original`，再次从同一输出位置复制 Original APK。
8. 对两个 APK 分别执行 `zipalign -P 16`，再用同一正式密钥执行 `apksigner sign`。
9. 使用 `apksigner verify --verbose --print-certs`、`aapt dump badging`、`zipalign -c -P 16 -v 4` 和 ZIP 文件清单验证签名、包名、版本、对齐和 ABI。
10. 计算四件套 SHA-256，写入 `release-notes/vX.Y.Z.md`，再提交、打标签和创建正式 Release。

虽然 Tauri 产物路径中写着 `universal`，只要构建命令带 `--target aarch64`，最终包可以且应该只有 `lib/arm64-v8a/libanilog_lib.so`；是否为单 ABI 必须看 APK 内容，不能根据目录名判断。

### 9.2 版本源与构建产物

发布前需要同步更新：

- `package.json`、`package-lock.json`：前端/npm 版本。
- `src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`：Rust crate 版本。
- `src-tauri/tauri.conf.json`：Tauri 版本与 Android `versionCode`。
- `README.md`、`docs/MAINTAINER_HANDOFF.md`、`release-notes/vX.Y.Z.md`：下载链接、当前版本和校验值。

主要输出位置：

| 产物 | 实际输出路径 |
| --- | --- |
| Windows Standard | `src-tauri/target/release/bundle/nsis/AniLog_<版本>_x64-setup.exe` |
| Windows Original | `src-tauri/target/release/bundle/nsis/AniLog Original_<版本>_x64-setup.exe` |
| Android unsigned APK | `src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk` |
| Android Rust `.so` | `src-tauri/target/aarch64-linux-android/release/libanilog_lib.so` |

仓库根目录的 `android/` 是旧 Capacitor 回退工程。对它执行 `gradlew assembleStandardRelease` 得到的不是当前 Tauri 正式版；当前正式 Android 工程是 `src-tauri/gen/android/`。

修改共享状态、同步、通知、任务或桥接时，两套 edition 的 Rust 测试和两个目标平台都需要验证。只改文档时运行 `git diff --check` 和 Markdown 链接检查即可。更完整的命令见 [`../CONTRIBUTING.md`](../CONTRIBUTING.md) 与 [`TAURI_MIGRATION.md`](TAURI_MIGRATION.md)。

## 10. CI

`.github/workflows/ci.yml` 在 Windows 上使用 Node 22，覆盖：

- `npm ci`
- Electron/Capacitor Web 回退构建
- Tauri 标准版和 Original Web 构建
- 两套 Cargo feature 测试
- edition、状态刷新、任务保留、窗口生命周期、WebDAV、缓存、季度缓存、数据迁移和 Bangumi 回归测试
- production dependency audit

CI 目前只对 `main` 的 push 和目标为 `main` 的 PR 触发。迁移分支上的普通 push 不一定有 CI 结果，发布前不能据此假定已验证。

## 11. 发布约束

- beta/rc 使用 GitHub Pre-release，不标记 Latest；正式版标记为 Latest。
- Android 每一次对外构建都要递增 `bundle.android.versionCode`，包括 beta 到 beta。
- Android 覆盖更新必须使用与 `v0.5.0` 相同的证书。已知发布证书 SHA-256 为 `a20feecdff2c6489f634d1c30b5eb35873ca119ffde95f6b708ca474c6dface8`，仅用于校验，不代表可恢复密钥。
- 不在仓库中保存签名密钥、alias、密码、WebDAV 测试账户或本机密钥路径。
- Windows 安装包没有商业代码签名，用户可能看到“未知发布者”。发布说明需如实提示。
- 发布前逐个验证附件的 edition、包名、版本号、签名、SHA-256 和安装升级路径。
- 发布、推送、合并 PR 和修改 Latest 都是外部状态变更，必须获得维护者明确授权。

当前 [`RELEASING.md`](RELEASING.md) 以 Tauri 正式发布流程为主，并保留 v0.5 Electron/Capacitor 回退说明。不要运行旧流程中的 `dist:all` 当作 Tauri 包。

## 12. 已知风险与后续事项

- Tauri 已成为正式架构，仍需继续收集 Windows/Android 反馈。
- AniList 是分钟级播出时间的唯一可靠来源；`bangumi-data` 记录的是首播/流媒体锚点，不足以推断临时改档、停播或后续每集的分钟。AniList 返回 403/429/5xx 时只能保留最近一次可信时间或退回日期级信息，禁止按周播锚点“补推”下一集。
- 现有状态文件可能来自 v0.7.0 以前的旧键、WebDAV 删除墓碑或旧版错误任务。排障时先备份 `<安装目录>\data`，再记录 `anilog-state.json`、`season-cache` 和同步文件的时间戳；不要直接删除坚果云远端文件来“解决”本地显示问题。
- 追番列表的 `nextAiringEpisode` 是派生缓存，不属于 WebDAV 同步字段。若出现“加入后短暂有时间、随后消失”，优先检查权威刷新失败、旧删除墓碑合并和本地/远端 `subjectId` 是否重复，而不是把缓存字段重新写入同步文档。
- 季度首屏慢通常来自缓存未命中、过期刷新仍在进行或旧 WebView localStorage 配额迁移；Tauri 主链必须先显示可信快照再后台刷新，不能恢复成首屏等待全量 Bangumi 分页。
- Android 详情页只应在窄屏上换行和纵向排列；不要用固定宽度卡片或横向滚动掩盖布局问题。后台同步的执行时间受 WorkManager、AlarmManager 和厂商省电策略影响，不能承诺精确到分钟。
- 旧架构仍占用仓库空间，但当前有明确回退价值，不应仅为减小目录而删除。
- Rust 与 Android AniList User-Agent 从各自构建版本生成，发布时需确认版本源同步更新。
- `src/api.ts` 旧浏览器路径的 Bangumi resolver version 为 4，Rust 实现为 5。它们是不同实现，未经迁移分析不能强制改成相同数字。
- Android 后台及时性受系统和厂商调度影响，不能承诺精确到分钟。
- Tauri 正式替代旧架构后，需要单独制定删除 Electron/Capacitor 的条件、数据迁移终止版本和仓库清理提交，不应夹带在普通功能 PR 中。

## 13. 接手检查清单

1. 阅读根目录 `AGENTS.md`、本文件和与任务相关的迁移/发布文档。
2. 运行 `git status --short`，确认分支和用户现有修改。
3. 判断改动针对 Tauri 现行架构、v0.5 回退架构，还是两者都要覆盖。
4. 判断是否影响两个 edition、两个平台、迁移数据或 WebDAV 冲突规则。
5. 只恢复完成任务所需的依赖，避免无意义重建数十 GB 产物。
6. 按风险运行测试，并记录无法完成的人工验证。
7. 检查提交内容不含凭据、签名文件、本地状态、APK/EXE 或大型缓存。
8. 未获授权时停在本地修改，不自行推送、合并或发布。
