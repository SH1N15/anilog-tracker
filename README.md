# AniLog

本地优先的跨平台追番工具（Windows / Android）：新番日程提醒、打勾式看番清单、可选的 Bangumi 账户同步与 WebDAV 双端同步。
Local-first anime schedule, notification, episode task manager, optional Bangumi account sync, and WebDAV device sync for Windows and Android.

[![CI](https://github.com/SH1N15/anilog-tracker/actions/workflows/ci.yml/badge.svg)](https://github.com/SH1N15/anilog-tracker/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

## 界面预览

### Windows

![AniLog 季度新番列表](docs/images/season.png)

![AniLog 观看任务](docs/images/tasks.png)

### Android

<p align="center">
  <img src="docs/images/android-season.png" alt="AniLog Android 季度新番列表" width="360">
</p>

## 为什么使用 AniLog

- 自动整理每季度新番及下一集播出时间
- 新一集播出后在 Windows 或 Android 发送通知，并可创建待看任务
- 可按自定义时间每日汇总提醒尚未完成的待看任务
- 基础功能无需登录 AniList 或 Bangumi；标准版可选填 Bangumi Access Token，同步追番状态、评分和观看进度
- 默认本地保存；WebDAV 与 Bangumi 是两个独立同步通道，可分别启用
  
## 下载与安装

前往 [GitHub Releases](https://github.com/SH1N15/anilog-tracker/releases) 下载需要的版本：

### 正式版 v0.7.2

- [`AniLog-Windows-v0.7.2-x64-setup.exe`](https://github.com/SH1N15/anilog-tracker/releases/download/v0.7.2/AniLog-Windows-v0.7.2-x64-setup.exe)：Windows 标准版
- [`AniLog-Original-Windows-v0.7.2-x64-setup.exe`](https://github.com/SH1N15/anilog-tracker/releases/download/v0.7.2/AniLog-Original-Windows-v0.7.2-x64-setup.exe)：Windows 原名版
- [`AniLog-Android-v0.7.2-arm64-v8a.apk`](https://github.com/SH1N15/anilog-tracker/releases/download/v0.7.2/AniLog-Android-v0.7.2-arm64-v8a.apk)：Android 标准版，仅支持 arm64-v8a
- [`AniLog-Original-Android-v0.7.2-arm64-v8a.apk`](https://github.com/SH1N15/anilog-tracker/releases/download/v0.7.2/AniLog-Original-Android-v0.7.2-arm64-v8a.apk)：Android 原名版，仅支持 arm64-v8a

`v0.7.2` 已正式发布，进一步修复 Bangumi 分季作品的本地/全局集号混用、季度列表显示全局集号、旧观看任务残留，以及 Android 分季分钟级时间匹配。它会覆盖对应 edition 的旧版安装；升级前建议先启用 WebDAV 同步或备份 Windows 安装目录中的 `data` 文件夹。

- 支持 Windows 10/11 x64，以及使用 arm64-v8a 的 Android 7.0 或更高版本
- 番剧日程需要网络连接；标准版的在线中文标题查询还会访问 Bangumi API 或配置的反代
- Windows 安装包尚未进行商业代码签名，可能显示“未知发布者”提示
- Windows 更新安装会保留安装目录下的 `data` 文件夹

Android 版使用项目发布密钥签名。公开下载包及其 SHA-256 校验值以 GitHub Releases 页面为准。标准版与原名版使用不同包名，可以同时安装。

Windows 版将追番记录、观看任务、缓存和设置保存在 `<安装目录>\data`；Android 版保存在系统分配的应用数据目录。默认情况下两端数据彼此独立；启用 WebDAV 后，仅同步追番、观看任务和取消追番记录。卸载 Windows 版或移动安装目录前，请备份对应的 `data` 文件夹；卸载 Android 应用会清除手机端本地记录。

## 功能

- 自动打开当前季度的新番列表，并可切换年份、季度、类型与搜索条件
- 可按本机时区将新番按星期分组，也可切换为完整列表
- 自主添加或取消追番，按本机时区显示下一集播出时间
- Windows 应用可驻留系统托盘，定时检查 AniList 播出日程
- Windows 每个版本只运行一个实例，并可选择隐藏托盘图标
- 新一集播出后发送系统通知，并自动创建待看任务
- 可选每日待看提醒：自定义本机提醒时间，仅有待看任务时发送一次摘要，点击通知直接打开任务列表
- Android 使用系统后台调度校正日程，无需常驻进程；可关闭手机端的自动待看任务，仅保留通知
- 使用用户自己的通用 WebDAV 账户双向同步追番和观看任务，支持离线修改后的冲突合并
- 标准版可选连接 Bangumi 账户：同步追番状态、评分和已完成集数；Token 仅保存在系统安全存储中，不上传 WebDAV
- 新番列表优先显示 Bangumi 中文标题，匹配不到时回退到英文
- 中文标题会用于搜索、通知和观看任务，也可在“我的追番”中自定义
- 原名版完全不连接 Bangumi；Windows 安装时可选中英文，Windows 与 Android 均可在“偏好设置 → 语言与番名”中切换界面语言及英文、罗马字或日文标题
- 勾选每集任务，保留已完成观看记录
- 默认本地保存；WebDAV 同步为可选功能，缓存、通知和设备偏好不会上传

## 开发

需要 Node.js 22。

```powershell
npm ci
npm run dev
```

使用原名版配置开发运行：

```powershell
npm run dev:original
```

`v0.7.2` 是当前 Tauri 2 正式版。新架构共享 React 界面与 Rust 核心；旧 Electron/Capacitor 代码暂时保留为回退路径。当前状态、构建命令和回归重点见 [docs/TAURI_MIGRATION.md](docs/TAURI_MIGRATION.md)。

完整的环境配置、构建和测试说明见 [CONTRIBUTING.md](CONTRIBUTING.md)，维护者发布版本时请参考 [docs/RELEASING.md](docs/RELEASING.md)。

## 使用说明

1. 在“季度新番”中选择要追的番剧。
2. Windows 版可保持运行或最小化到系统托盘；Android 版无需常驻后台。
3. 番剧播出后，AniLog 会发送系统通知；启用自动任务时还会添加一条观看任务。
4. 看完后在“观看任务”中勾选对应集数，完成观看任务。
5. 如需双端同步，在两台设备的“偏好设置 → 跨设备同步”中填写同一个 WebDAV 账户，测试成功后启用同步。
6. 标准版如需同步 Bangumi 追番、评分或观看进度，在“偏好设置 → Bangumi 账户”填写 Access Token；它与 WebDAV 同步相互独立。

每日待看提醒默认关闭，可在“偏好设置 → 更新提醒”中启用并选择时间。提醒只读取当前设备的本地任务，不会额外访问 AniList、Bangumi 或 WebDAV；没有待看任务时不会显示通知。设备在设定时间关机或休眠时，AniLog 会在下次启动或恢复后补发当日提醒，同一天最多发送一次。

WebDAV 会在应用启动或恢复前台、数据发生修改时检查更新，也可手动点击“立即同步”；Windows 后台运行期间还会每 15 分钟检查一次，Android 不需要为 WebDAV 常驻后台。同步文件位于 WebDAV 根目录的 `AniLog/anilog-sync.json`。Windows 使用系统安全存储加密密码，Android 使用系统 Keystore；坚果云等服务应填写第三方应用密码，而不是账户登录密码。服务不可用时，本地追番、任务和通知不受影响。

Windows 开机自启使用隐藏启动参数，登录后直接驻留托盘，不主动打开主窗口。Windows 注销或关机时会停止后台同步后退出。

Android 首次追番时需要允许通知。未授予“准时通知”权限时系统仍会发送通知，但可能略有延迟；部分设备还需要将 AniLog 的电池策略设为“不限制”。在系统设置中“强行停止”应用会暂停后台调度，重新打开一次即可恢复。

标准版使用 AniList GraphQL API 获取公开番剧与分钟级播出日程，使用 Bangumi 逐集数据确定作品/分季的本地集号，并使用 `bangumi-data` 和 Bangumi API 补充中文标题与条目信息。基础功能无需账号；Bangumi Access Token 仅在启用账户同步时需要。AniList 暂时不可用时，应用不会用周播锚点猜测集数，而是保留最近一次可信的精确时间或退回日期级信息，待服务恢复后自动纠偏。

Bangumi 账户同步与 WebDAV 是两个独立通道。Bangumi 账户同步可以拉取追番状态、回写本地状态/评分/观看进度；WebDAV 只同步 `following`、`tasks` 和取消追番记录，不同步 Token、缓存、设备设置或通知开关。

AniLog 原名版只使用 AniList GraphQL API。它不会加载 `bangumi-data`，不会注册 Bangumi 通信接口，也不会向 Bangumi 官方 API 或第三方反代发送请求。默认按“英文 → 罗马字 → 日文”显示标题，也可以在设置中改变首选顺序。应用的其他功能与中文标题标准版一致。

## English

AniLog Original is a local-first anime schedule, release notification, and episode task tracker for Windows and Android. It uses AniList titles only and never connects to Bangumi or a third-party Bangumi proxy.

- The Windows installer lets you choose English or Simplified Chinese and remembers that choice on first launch.
- The Android Original app follows the system language on first launch. You can switch languages later under **Settings → Language and titles**.
- English title order defaults to English, then Romaji, then Japanese. The order can be changed in Settings.
- Following, watch tasks, notifications, local storage, and optional user-owned WebDAV sync work in both languages.
- Optional daily watch-task summaries use local data only and open the task list when tapped.

Download the bilingual Original builds from [GitHub Releases](https://github.com/SH1N15/anilog-tracker/releases). Windows 10/11 x64 and Android 7.0 or later are supported.

中文标题首先来自 `bangumi-data` 的本地节目数据，缺失条目再通过 Bangumi API 查询。应用只查询进入可视区域且尚未缓存的条目，并在网络异常时自动暂停请求。

本地解析优先读取 `bangumi-data` 提供的 AniList ID 映射，再回退到规范化标题、完整首播日期、季度或 Stage 编号、词序相似度和作品类型匹配。一条 AniList 作品对应多个 Bangumi 篇章时，仅在中文标题拥有足够长的共同前缀时合并显示。匹配缓存带有解析器版本，升级算法后会自动重新检查旧的未匹配与歧义结果。

中国大陆网络无法直连 Bangumi 时，标准版默认使用项目维护者部署的反代地址。项目维护者不保证该反代一直生效，用户也可在“偏好设置 → 中文标题网络”中填写其他 HTTPS API 反代地址，或清空地址改用官方 API。地址可填域名根路径或以 `/v0` 结尾的 API 根路径，应用会测试连接后保存；反代失败时自动尝试官方 API，最终回退到本地标题数据。

默认反代基于 [makabaka11/bangumi-proxy-workers](https://github.com/makabaka11/bangumi-proxy-workers) 部署。反代服务可看到请求来源 IP 和被搜索的番剧标题；应用不发送追番清单、观看任务或 Bangumi 登录凭据。

`bangumi-data` 由 [bangumi-data 项目](https://github.com/bangumi-data/bangumi-data)维护，依据 CC BY 4.0 许可使用。

## 许可证

AniLog 源代码使用 [MIT License](LICENSE)。第三方数据和依赖保留各自的许可证，标准版详情见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)，原名版说明见 [THIRD_PARTY_NOTICES_ORIGINAL.md](THIRD_PARTY_NOTICES_ORIGINAL.md)。

AniLog 是非官方客户端，与 AniList、Bangumi 及相关权利方不存在隶属或背书关系。
