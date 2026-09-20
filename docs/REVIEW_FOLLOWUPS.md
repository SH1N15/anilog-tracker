# 代码审查待办（codex/tauri-migration）

来源：2026-07-30 对 `v0.6.0-beta.1` Tauri 迁移的审查，以及 `v0.6.0-beta.2` 修复阶段的复核。

当前 GitHub 正式版为 `v0.7.5`。异常观看历史、计数、同步与网络生命周期修复已经进入正式版；下文仍是历史核验和独立安全待办，不代表自动扩大后续维护范围。

## beta.2 已实现并完成公开验证

- **[#4] Windows 单实例**：使用 Windows 限定的官方 `tauri-plugin-single-instance`，重复启动统一调用现有窗口重建、显示与聚焦路径。标准版与 Original 的 identifier 不同，可以各运行一个实例。
- **[#5] 新番按星期分组**：前端按本机时区使用下一次有效播出时间分组，顺序为星期一至星期日，无日程作品最后显示；保留完整列表模式。
- **[#6] 可隐藏托盘图标**：增加仅 Tauri Windows 显示的本机设置。隐藏图标不停止后台同步与通知；再次启动会由单实例回调恢复窗口。
- **提醒时间正则**：改用 `std::sync::LazyLock` 预编译，并增加 `00:00`、`23:59` 及非法格式测试。
- **Android versionCode**：beta.2 使用 `6`；v0.6.0 正式版递增为 `7`。Original 继承基础配置；实际包名继续由 `ANILOG_ANDROID_EDITION=original` 在 Gradle 中切换。

以上项目已在 beta.2 发布阶段完成 Windows/Android 构建、版本号与签名验证；v0.6.0 正式版继续复用相同回归门禁，并将 Android 附件固定为仅 `arm64-v8a`。

## 已核验，无需改代码

### Bangumi 搜索请求方法

`POST /v0/search/subjects?limit=12&offset=0` 配合 JSON 请求体是 Bangumi 官方 v0 API 的正确契约。2026-07-30 使用与应用相同的请求形状实测官方 API 返回 HTTP 200；GET 返回 404。保持当前 POST 实现，不能按旧审查意见改为 GET。

### Android Original 版本来源

`src-tauri/tauri.android-original.conf.json` 不需要重复声明 `versionCode`。Tauri 会将基础配置写入生成的 `tauri.properties`，标准版和 Original 共用该版本号；Gradle 只按 edition 环境变量切换 `applicationId`。

## 已知状态（未排期）

### CSP 仍关闭

`src-tauri/tauri.conf.json` 当前为：

```json
"security": { "csp": null }
```

此项保留为已知状态，当前不改动。若维护者另行要求启用 CSP，需要同时覆盖 Tauri IPC、远程 AniList 图片、React 内联背景样式和 Vite 开发 WebSocket，并在 Windows/Android 两端单独验证，且不得放宽远程脚本来源。

### Original 中文产品名显示（已修正）

2026-09-13 核对当前 `src/edition.ts`，Original + `zh-CN` 已返回 `AniLog 原名版`。旧乱码待办不再适用；后续回归检查应用内中英文标题和升级语言保留，不能据旧记录重复修改正确字符串。

## 先前已处理

- **清理散落的 preview 日志**：根目录占位日志已删除，`.gitignore` 已覆盖 `*.log`。
- **移除未用依赖 `unicode-normalization`**：代码无引用，依赖已从 Cargo 清单移除。
