# AniLog 标准版 Bangumi 迁移 — Schema 冻结文档（Phase 0）

> 分支：`codex/bangumi-standard-migration`
> 状态：Phase 0 历史契约；当前 v0.7.2 修复线对播出权威的补充约束见 §6。
> 配套（本机）进度锚点：`LOCAL_MIGRATION_PROGRESS.md`（契约基准，本文档不得与之冲突）；产品方案：`LOCAL_BANGUMI_STANDARD_MIGRATION_PLAN.md`。

## 1. 文档目的与范围

本文件冻结 AniLog 标准版（Cargo feature `standard`）向 Bangumi 主数据源迁移过程中，跨 Rust（Windows/Android）、React 前端、Android 原生（Java/Kotlin）三层的共享数据契约，作为并行开发的唯一接口基准。

- **Phase 0 只做 additive 演进**：在现有 `anilog-state.json` 与 WebDAV 文档结构上追加新字段与新模型，不删除、不重命名任何旧字段。旧 v0.6 与回退实现（`electron/`、`android/` 的 v0.5）必须仍能读取新状态文件。
- **`STATE_VERSION` 维持在 2**：新增的 `bangumi` 设置块、同步状态、缓存等均以可选字段方式并入 v2 顶层，老读取方通过 `merge_defaults` 重建缺失默认值。真正的破坏性主键切换推迟到 **Phase 2 才升 `STATE_VERSION = 3`**（见 §4、§12）。
- **坚果云 WebDAV 文档 `SYNC_VERSION` 保持 1**：跨设备同步通道始终只携带 `following`、`tasks`、`followingDeletedAt`（见 §9），不因迁移而扩容。
- 覆盖范围：旧 schema（v2）现状清单、新 schema（additive）字段定义、主键迁移策略、端点基准、播出优先级、缓存与频率、Token 存储、坚果云边界、Android 桥兼容、测试验收清单、版本与回读兼容。

代码事实基准以 `src-tauri/src/lib.rs` 为准（行号基于当前分支），本文档所有旧字段清单均逐行核对。

## 2. 旧 schema（v2，现状）

### 2.1 `anilog-state.json` 顶层字段

`default_state`（`src-tauri/src/lib.rs` 约 95-119 行）写入的顶层键：

| 字段 | 类型 | 单位 / 语义 | 备注 |
|---|---|---|---|
| `version` | number | 固定 `STATE_VERSION = 2`（约第 40 行） | `merge_defaults` 每次加载都会 `target.insert("version", 2)` 强制回写 |
| `following` | array | `FollowingEntry` 列表，见 §2.2 | |
| `tasks` | array | `Task` 列表，见 §2.3 | |
| `seenAiringEvents` | array<string> | 已见过的播出事件 id 去重集 | 上限 2000，超出头部裁剪（约第 213-217 行） |
| `bangumiTitles` | object | `Record<string, BangumiTitleMatch>`（标题 resolver 缓存） | 默认 `{}`；前端 `src/types.ts` 镜像 |
| `settings` | object | 设备设置，见 §2.4 | |
| `lastSyncAt` | number | **秒** 时间戳 | `now_seconds()`；sync 窗口起点 |
| `lastTaskReminderDate` | string | `"YYYY-MM-DD"` 或 `""` | 每日任务提醒去重标记 |
| `syncMetadata` | object | 含 `followingDeletedAt` 墓碑表，见 §2.5 | |

`ensure_sync_metadata`（约 148-218 行）在加载时补齐缺失结构，并对 `following`/`tasks` 逐条补齐 `syncUpdatedAt`（毫秒）。

### 2.2 `FollowingEntry`（追番条目）

`toggle_follow` 中的构造（`src-tauri/src/lib.rs` 约 875-881 行）精确字段：

| 字段 | 类型 | 单位 / 语义 |
|---|---|---|
| `id` | number | AniList 媒体 id（旧主键，v3 前的唯一实体 id） |
| `title` | object | `AnimeTitle`：`{ native, romaji, english }`（可空字符串） |
| `displayTitle` | string | 展示标题，来自 `followed_title_fields` |
| `titleSource` | string | `"anilist" \| "bangumi" \| "custom"` |
| `bangumiId` | number \| null | Bangumi subject id（resolver 结果，可空） |
| `coverImage` | string | 取 `medium` 优先，回落 `extraLarge` |
| `format` | string \| null | |
| `episodes` | number \| null | |
| `seasonYear` | number \| null | |
| `startDate` | object \| null | `{ year, month, day }` |
| `nextAiringEpisode` | object \| null | `{ episode, airingAt(秒), timeUntilAiring? }` |
| `siteUrl` | string | AniList 页面地址 |
| `followedAt` | number | **秒** 时间戳（`now_seconds()`） |
| `syncUpdatedAt` | number | **毫秒** 时间戳（`now_millis()`），LWW 依据 |

> 单位陷阱（务必注意）：`followedAt` 为秒，`syncUpdatedAt` 为毫秒；二者相差 1000 倍，混用会导致墓碑/LWW 判断错乱。

### 2.3 `Task`（观看任务）

`apply_airing_schedules` 中的构造（`src-tauri/src/lib.rs` 约 1128 行）精确字段：

| 字段 | 类型 | 单位 / 语义 |
|---|---|---|
| `id` | string | 格式 `` `{animeId}-{episode}` ``（约第 1108 行），**Phase 0 保留此旧 id 格式可读** |
| `animeId` | number | 关联 `FollowingEntry.id` |
| `animeTitle` | string | 冗余展示标题 |
| `coverImage` | string | |
| `episode` | number | 集数 |
| `airingAt` | number | **秒** 时间戳（AniList airingAt） |
| `status` | string | `"pending" \| "completed"` |
| `createdAt` | number | **秒** 时间戳（`now`） |
| `completedAt` | number \| null | **秒**；`null` 表示未完成 |
| `syncUpdatedAt` | number | **毫秒** 时间戳 |

> 单位陷阱：任务里 `airingAt`/`createdAt`/`completedAt` 为秒，`syncUpdatedAt` 为毫秒。

### 2.4 `settings`（全字段，含默认值）

`default_state`（约 102-114 行）：

| 字段 | 类型 | 默认值（standard / original） |
|---|---|---|
| `uiLanguage` | string | `"zh-CN"` / `"en-US"` |
| `pollIntervalMinutes` | number | `5` |
| `launchAtLogin` | boolean | `false` |
| `minimizeToTray` | boolean | `true` |
| `showTrayIcon` | boolean | `true` |
| `notifyWhenAired` | boolean | `true` |
| `createWatchTasks` | boolean | `true` |
| `dailyTaskReminderEnabled` | boolean | `false` |
| `dailyTaskReminderTime` | string | `"20:00"`（正则 `^([01]\d|2[0-3]):[0-5]\d$`） |
| `bangumiApiBaseUrl` | string | 标准版 `https://sh1n.cc.cd/v0`（`DEFAULT_BANGUMI_PROXY`，约第 38 行）/ 原版 `""` |
| `titlePreference` | string | `"auto"` |

> `bangumiApiBaseUrl` 可进普通设置（反代地址非敏感）。Token 不在此块内（见 §8）。

### 2.5 墓碑语义与 LWW

- 删除追番：`mark_following_deleted`（约 477-480 行）写入 `syncMetadata.followingDeletedAt[{animeId}] = now_millis()`（**毫秒**）。
- 复活清除：`mark_following_changed`（约 463-475 行）在重新追番时刷新该条目 `syncUpdatedAt = now_millis()` 并从 `followingDeletedAt` **移除**对应墓碑（resurrection 清墓碑）。
- LWW 合并：`merge_document_into_state`（约 600-706 行）用 `record_timestamp`（优先 `syncUpdatedAt`，回落 `followedAt × 1000` / `createdAt × 1000` 秒转毫秒，约 499-506 行）比较时间戳选 winner；`choose_record`（约 516-537 行）时间戳相等时用 `stable_record` 规范化字符串做确定性兜底。
- 复活判定：`record_timestamp(winner) > 墓碑时间` 才保留条目（约 641 行）。

### 2.6 WebDAV 文档结构

`document_from_state`（约 552-579 行）产出的跨设备文档，`SYNC_VERSION = 1`（约第 41 行）：

```json
{
  "version": 1,
  "updatedAt": <毫秒，全部记录的最大时间戳>,
  "following": [ ...FollowingEntry 全字段... ],
  "tasks": [ ...Task 全字段... ],
  "followingDeletedAt": { "<animeId>": <毫秒> }
}
```

- **只含三业务字段**：`following`、`tasks`、`followingDeletedAt`（外加 `version`、`updatedAt` 元数据）；`settings`、`seenAiringEvents`、`bangumiTitles`、`lastSyncAt` 等设备/缓存字段**不进文档**。
- 写入前 `following`/`tasks` 分别按 id、animeId、episode、status 合法性过滤并排序（`following` 按 `id`，`tasks` 按 `id`）。
- `normalize_document`（约 581-591 行）加载远端：校验 `version`，仅重建上述三字段并再跑一次 `document_from_state` 归一化。
- **5 MB 上限**：`MAX_SYNC_BYTES = 5 * 1024 * 1024`（约第 49 行，桌面侧）。

## 3. 新 schema（additive，Phase 0 冻结）

Phase 0 不改变 `STATE_VERSION`（仍为 2），以下均为可选新增字段/模型。`merge_defaults` 对缺失键补默认；未知键在加载/写回中保留（见 §12）。

### 3.1 `bangumi` 设置块

镜像契约 C2 `BangumiSyncSettings`。**只进本地状态，绝不进坚果云文档**（§9）。默认值：

| 字段 | 类型 | 默认值 | 语义 |
|---|---|---|---|
| `apiBaseUrl` | string | `""` | 空=官方 `api.bgm.tv`；非空=反代地址（普通设置，非敏感） |
| `syncEnabled` | boolean | `false` | Bangumi 同步总开关 |
| `pullCollections` | boolean | `true` | 从 Bangumi 读取收藏 |
| `pushLocalChanges` | boolean | `false` | 本地追番变化写回 Bangumi |
| `pushCompletedEpisodes` | boolean | `false` | 完成任务自动上传单集进度 |
| `pullExternalStatus` | boolean | `true` | Bangumi 外部状态拉取 |
| `conflictPolicy` | string | `"latest"` | `"latest" \| "localFirst" \| "bangumiFirst"`（`ConflictPolicy`） |
| `preferredBroadcastSites` | array<string> | `["bangumi","ani_one","ani_one_asia","gamer","unext"]` | 播出选站优先级（§6） |

> 落地位置（设置块挂在 `settings.bangumi` 还是顶层与 `settings` 并列）由实现者定，本文档冻结字段清单与默认值本身；Original 版**不写该块**（硬不变量 1）。

### 3.2 subject 记录模型

契约 C2 `BangumiSubjectRecord`。Phase 2 起 `subjectId` 成为标准版主键；Phase 0 只冻结结构。

| 字段 | 类型 | 单位 / 语义 |
|---|---|---|
| `subjectId` | number | **主键**（Bangumi subject id） |
| `source` | string | `"bangumi"`（实体来源标记） |
| `title` | string | 展示标题 |
| `titleOriginal` | string \| null | 原文标题 |
| `titleRomaji` | string \| null | |
| `coverImage` | string | |
| `format` | string | |
| `episodes` | number \| null | |
| `airing` | object \| null | `{ nextEpisode: number, nextAiringAt: number(秒) }` |
| `bangumiStatus` | string | 收藏状态（映射 type 1-5，§5） |
| `rating` | number \| null | Bangumi 评分 |
| `watchedEpisode` | number \| null | 进度 |
| `anilistId` | number \| null | **兼容字段**，仅迁移/回退查询用，可空 |
| `mapping` | object | `{ method, confidence, updatedAt(秒) }` |
| `mappingPending` | boolean | 迁移中间态标记（尚未建立映射时为 true） |
| `lastPulledFromBangumiAt` | number | **秒** 时间戳 |
| `lastPushedToBangumiAt` | number | **秒** 时间戳 |
| `lastPulledPayloadHash` | string | 上次拉取的 payload 哈希 |
| `lastPushedPayloadHash` | string | 上次推送的 payload 哈希 |
| `lastChangedBy` | string | `"local" \| "bangumi" \| "webdav"`（`LastChangedBy`） |

`mapping.method`：`"local" \| "external" \| "title-year" \| "manual"`（`MappingMethod`）。
`mapping.confidence`：`"high" \| "medium" \| "low"`（`MappingConfidence`）。

> **四方时间戳与 payload hash 的必要性**：Bangumi 官方注明收藏 `updated_at` 在评分/评价/章节观看状态修改时可能不更新，因此冲突解决**禁用 `updated_at` LWW**（仅参考），改用 `lastPulledPayloadHash` / `lastPushedPayloadHash` 判断外部是否变化与幂等去重，并用 `lastChangedBy` 区分本地/坚果云/Bangumi 三方来源，避免循环写回。

### 3.3 episode 记录模型

契约 C2 `BangumiEpisodeRecord`。**旧任务 id 格式（`{animeId}-{episode}`）Phase 0 保留可读**，映射成功后才补充 subject/episode 字段；迁移中间态为 `{ "anilistId": <旧id>, "subjectId": null, "mappingPending": true }`，不覆盖旧数据。

| 字段 | 类型 | 单位 / 语义 |
|---|---|---|
| `id` | string | 任务/记录标识（迁移期兼容旧 `{animeId}-{episode}` 格式） |
| `subjectId` | number \| null | 关联 subject；未映射时为 `null` |
| `episodeId` | number \| null | Bangumi episode id |
| `episodeNumber` | number \| null | 集数（不假设一定是简单整数） |
| `episodeSortKey` | string | 稳定排序键 |
| `episodeType` | string | `"regular" \| "special" \| "movie" \| "ova" \| "unknown"`（serde 别名同名） |
| `title` | string | |
| `status` | string | `"pending" \| "completed"`（对齐旧任务状态） |
| `completedAt` | number \| null | **秒** |
| `syncUpdatedAt` | number | **毫秒** 时间戳 |

> Episode type 与 Bangumi API type 的关系见 §5（0 本篇 / 1 SP / 2 OP / 3 ED），迁移引擎映射为上面五个枚举值。

### 3.4 本地-only 同步状态五字段

契约 C2 `BangumiSyncStatus`。**明确：绝不进坚果云文档、不参与业务同步**，仅本机展示与前台过期补偿使用。

| 字段 | 类型 | 单位 / 语义 |
|---|---|---|
| `lastFullSyncAt` | number | **秒**，上次完整同步成功 |
| `lastWebDavSyncAt` | number | **秒**，上次坚果云同步 |
| `lastBangumiSyncAt` | number | **秒**，上次 Bangumi 同步 |
| `lastScheduleSyncAt` | number | **秒**，上次播出调度同步 |
| `lastSyncError` | string | 最近一次同步错误摘要（不含 Token） |

> 前台恢复策略：距 `lastFullSyncAt` 超过 15 分钟即触发过期完整同步（见进度文档 §8.2/8.3）；此为只读展示与补偿依据，不写入 WebDAV。

## 4. 主键迁移策略

标准版实体主键从 AniList `id` 迁移到 Bangumi `subjectId`。Phase 0 只冻结映射规则与中间态，实际切换在 Phase 2（届时升 `STATE_VERSION = 3`）。

**映射优先级（从高到低）：**

1. 已保存的**手动映射**（`mapping.method = "manual"`）。
2. **已知映射表**（bangumi-data 离线表 `anilistIndex` / `bySubject`，契约 C1）。
3. **外部关联 ID**（Bangumi API 返回的 `external` 关联，含 AniList id）。
4. **标题 + 年份 + 季节 + 集数** 综合匹配（`mapping.method = "title-year"`）。
5. **用户确认**（低置信/多候选走确认对话框）。

**不自动强配的场景：**

- 仅标题相同（不足以判定 high confidence）。
- `confidence = low` 或多候选（`ambiguous`）。
- 电影、OVA、特别篇（`episodeType`/`format` 非普通 TV 连载）——续作、特别篇必须允许用户确认。
- 映射未确认时**不自动把 Bangumi 进度写回错误作品**。

**硬不变量：**

- 旧 AniList 数据**不被覆盖**。
- 映射失败**不删除追番或任务**（迁移中间态保留 `anilistId`、`subjectId = null`、`mappingPending = true`）。

## 5. 端点基准

官方主机 `https://api.bgm.tv`；v0 接口前缀 `/v0`，`/calendar` 在**根路径**（非 `/v0` 下）。双基址解析：`resolve_base_urls` → 官方 `root=https://api.bgm.tv`、`v0=https://api.bgm.tv/v0`；反代地址若含 `/v0` 后缀则 `root = strip("/v0")`、`v0 = 原串`。标准版默认反代为 `https://sh1n.cc.cd/v0`（`DEFAULT_BANGUMI_PROXY`）。

| 用途 | 方法 + 路径 | 备注 |
|---|---|---|
| 每日放送 | `GET {root}/calendar` | 根路径 |
| 季度列表 | `GET {v0}/subjects?type=2&year=YYYY&month=MM&limit=&offset=` | 分页；**无 `/seasons` 端点** |
| 条目详情（含总评分） | `GET {v0}/subjects/{id}` | 含 `rating`/`tags`/`infobox`/`date`/`eps`；v0 `Subject` **无 `air_weekday`**（仅 legacy `/calendar`） |
| 集数 | `GET {v0}/episodes?subject_id={id}&type=&limit=&offset=` | **不存在** `GET {v0}/subjects/{id}/episodes`；limit 默认 100、上限 200；响应 `Paged_Episode = {total,limit,offset,data}` |
| 角色 | `GET {v0}/subjects/{id}/characters` | |
| 关联条目 | `GET {v0}/subjects/{id}/subjects` | |
| 当前用户 | `GET {v0}/me` | testConnection 用它；v0 `User` 字段为 `user_group` |
| 用户收藏列表 | `GET {v0}/users/{username}/collections?subject_type=2&limit=&offset=` | username 来自 `/v0/me`（读端点用 `{username}` 实名） |
| 单条收藏 | `GET {v0}/users/{username}/collections/{subject_id}` | **复数 `collections`**；单数 `collection` 路径不存在 |
| 写：收藏 | `POST/PATCH {v0}/users/-/collections/{subject_id}` | 官方以 `-` 占位 = 当前 token 用户；body `UserSubjectCollectionModifyPayload` 全可选（type, rate(0-10), ep_status, vol_status, comment, private, tags）；不缓存，hash 幂等 |
| 写：单集进度 | `PUT {v0}/users/-/collections/-/episodes/{episode_id}` | body `{type}`；不缓存，hash 幂等 |
| 写：批量进度 | `PATCH {v0}/users/-/collections/{subject_id}/episodes` | body `{episode_id:[...], type}`；不缓存，hash 幂等 |

**枚举基准：**

- 季度 → 月份映射：`WINTER = 1-3`、`SPRING = 4-6`、`SUMMER = 7-9`、`FALL = 10-12`。
- 收藏 type（SubjectCollectionType）：`1 = wish`（想看）、**`2 = done`（看过）**、**`3 = doing`（在看）**、`4 = on_hold`（搁置）、`5 = dropped`（弃番）。
- 单集收藏类型（EpisodeCollectionType）：`0 = 未收藏`、`1 = 想看`、`2 = 看过`、`3 = 抛弃`。
- episode type（EpType，0-6）：`0 = 本篇`、`1 = SP`、`2 = OP`、`3 = ED`、`4 = PV`、`5 = MAD`、`6 = 其他`（迁移映射到 §3.3 `episodeType`）。
- 写路径 `-` 占位：官方 OpenAPI 写操作统一以 `-` 代表当前 token 用户（Bearer 认证，写操作 scope `write:collection`）；读端点仍用 `{username}` 实名（`/v0/me` → `username` → `/v0/users/{username}/collections`）。
- **`ep_status` 写回限制**：`UserSubjectCollectionModifyPayload` 的 `ep_status`/`vol_status` 官方注明**只能用于修改书籍条目进度** → 动画进度必须走 episodes 端点；`UserSubjectCollection` 响应中 `ep_status` 仍可读。
- **无 webhook**：外部变化只能定时/前台拉取。
- **`updated_at` 注意事项**（官方注释原文）：「本时间并不代表条目的收藏时间。修改评分，评价，章节观看状态等收藏信息时未更新此时间是一个 bug。请不要依赖此特性」→ 冲突解决禁用其做 LWW（见 §3.2）。
- 其他 v0 事实：错误体 `ErrorDetail{title, description, details?}`；分页信封统一 `{total, limit, offset, data}`。

## 6. 播出时间权威与 bangumi-data 交付

当前实现不再把 `bangumi-data` 的首播锚点当作逐集事实。作品有
`anilistId` 时，Standard 的逐集播出链按以下规则裁决：

1. **Bangumi `/v0/episodes`**：先确认作品、普通集号、`episodeId`，并提供
   `airdate` 的日期级事实。它可以反映停播、延期、改档后的逐集记录；不得用
   `begin + P7D` 代替缺失的逐集记录。
2. **AniList**：只有在 AniList 条目与 Bangumi `sort` 集号一致时，成功响应中的
   `airingAt` 才覆盖该集的日期和分钟。它是分钟级提醒和改档纠偏的权威来源。
   成功响应写入本地权威缓存，最多保留 7 天供上游短暂不可用时复用。
3. **降级**：AniList 不可用时使用最近一次可信 AniList 时间；没有可信时间时只
   使用 Bangumi 日期级信息，不伪造分钟级时刻。恢复后下一轮同步自动纠偏。

`bangumi-data` 只承担离线映射、季度列表和展示元数据；其 `begin`、`broadcast`
和 `sites[]` 不得在 Standard 生产同步中生成后续任务或通知时间。无 `anilistId`
且 Bangumi 没有逐集日期的孤立条目，才可以使用离线锚点作为最后的展示级回退，
不得把它当作分钟级提醒事实。

- `begin`：ISO 8601（含秒）。
- `broadcast`：RFC 5545 周期串，如 `R/2026-07-08T13:00:22.000Z/P7D`；规则 `R/<start>/P7D` → 以 chrono `Local` 换算下次本地播出时刻。golden 测试向量共享（Rust / JS / Java 三份一致）。
- `preferredBroadcastSites` 默认 `["bangumi","ani_one","ani_one_asia","gamer","unext"]`，存设置可调（§3.1）。

**bangumi-data 三层交付：**

1. 构建**内置**快照（build.rs 产物 `bangumi-map.json` v2，契约 C1），用于映射和季度展示。
2. **Bangumi API/反代逐集缓存**（`episodes-{subjectId}.json`，默认 24h）。
3. **AniList 权威缓存**（成功响应写入，分钟级数据最多保留 7 天；云端合并后仅本地重放）。

bangumi-data（npm 包 0.3.215）字段：`begin`(ISO 含秒)、`broadcast`(RFC5545)、`sites[]`(含 anilist/bangumi 平台 id)、`name_cn`、`type`、`date`。

## 7. 缓存与频率表

照抄进度文档 `LOCAL_MIGRATION_PROGRESS.md` §缓存与频率：

| 数据 | 端点 | TTL | 请求时机 |
|---|---|---|---|
| 离线快照（映射+展示元数据） | 构建内置+反代快照 | 12-24h 检查 | 启动/到期；绝不按卡片 |
| 每日放送 | `/calendar`（根路径） | 6h | 前台过期才刷新 |
| 季度列表 | `/v0/subjects` 分页 | 24h，按月+页独立缓存 | 前台过期才刷新 |
| 条目详情（含总评分） | `/v0/subjects/{id}` | 24h，SWR | 打开详情或关注时 |
| Bangumi 逐集事实 | `/v0/episodes?subject_id={id}` | 24h，失败可用旧缓存 | 追番同步/WebDAV 合并后 |
| AniList 分钟级播出 | `nextAiringEpisode` + `airingSchedule` | 成功响应最多保留 7 天；纠偏缓存 30min | 桌面同步；Android 可用时补充 |
| 用户收藏 | `/v0/users/{username}/collections` | 前台 15min / 后台 6h | 仅启用 Bangumi 同步 |
| 写操作 | PATCH/POST | 不缓存 | 用户动作立即，hash 幂等 |

**纪律：**

- 同资源 **single-flight**（`request_gate` + 每资源 in-flight map，契约 C2）。
- 同 subject 不并发重复请求。
- 全局并发信号量 **1-2**（独立于标题 resolver 的 450ms 串行锁）。
- **429** 尊重 `Retry-After` + 指数退避（不假设官方限流数值）；`parse_retry_after(header) -> Option<Duration>`。
- **手动刷新**绕缓存但 **60s 冷却**。
- **N+1 禁令**：禁止对季度卡片逐卡请求详情；详情仅打开/关注时惰性请求（SWR 24h），卡片本体不请求。

## 8. Token 存储契约

- Bangumi Access Token **只进系统凭据存储**：
  - Windows：Credential Manager，**服务名 `io.anilog.bangumi`**（`KeyringTokenStore`，account `default`，契约 C2）。
  - Android：**Android Keystore**（Phase 1 的 `BangumiTokenStore.java`，仿 WebDAV 存储实现）。
- Token **永不进入** `anilog-state.json`、状态 JSON、日志、错误信息、WebDAV 文档、Git 提交、Issue、Release、测试快照。
- `BangumiTokenStore` trait：`load / store / clear` + `TokenStoreError`；测试用 `MemoryTokenStore`，其他平台 `Unsupported`。
- **反代地址（`apiBaseUrl`）可以进普通设置**（非敏感）；Token 不可以。
- Bearer Token 只在请求时运行时注入，`Authorization: Bearer <token>` 不进任何日志。

## 9. 坚果云边界

- 坚果云 WebDAV 文档**仍只同步三业务字段**：`following`、`tasks`、`followingDeletedAt`（外加 `version=1`、`updatedAt` 元数据），`SYNC_VERSION` 保持 1（§2.6）。
- **不同步**：`bangumi` 设置块（§3.1）、本地-only 同步状态五字段（§3.4）、Bangumi 拉取缓存、AniList/Bangumi 作品缓存、`settings`、`seenAiringEvents`、`bangumiTitles`、通知开关、Android 后台状态、任何凭据。
- `document_from_state` 输出三字段不变，由**回归测试锁定**（§11），任何 additive 字段不得泄漏进文档。
- 坚果云与 Bangumi 是两条独立通道，互不替代、互不泄漏。

## 10. Android 桥兼容

- `anilog-state.json` 与 WebDAV 文档由 Android 侧 `MobileStore` 以**动态 JSON 解析**处理；新增的可选字段（`bangumi` 块、同步状态、subject/episode 附加字段）对其解析路径透明——老桥读不认识的键不报错、不丢弃。
- `mobile::configure` / payload 增量字段在 Phase 2/4 追加，Phase 0 只冻结契约不改变现有桥行为。
- Phase 4 的 Java 侧 `BackgroundSyncWorker` 将按同一契约实现：坚果云合并（三字段、LWW、墓碑、resurrection 清墓碑）与 `broadcast` 解析（RFC5545 `R/<start>/P7D` → 本地下次播出）。合并与 broadcast 解析的 **golden 测试向量与 Rust/JS 共享**，保证三层结果一致。

## 11. 测试验收清单

每条 schema 契约对应至少一项测试（Phase 0 门禁：`cargo test standard` + `cargo test original` + 12 个 `npm run test:*` + `npm audit` + `git diff --check`）：

| 契约 | 测试名（建议） | 覆盖 |
|---|---|---|
| §3 subject/episode/settings/status serde | `bangumi_*_serde_round_trip` | 序列↔反序列化幂等，别名（`episodeType`）稳定 |
| §2 旧 v2 状态 | `loads_legacy_v2_state` / `merge_defaults_fills_missing` | 旧状态加载、缺键补默认、未知键保留 |
| §2.4 单位不变量 | `sync_updated_at_millis_followed_at_seconds` | `followedAt` 秒、`syncUpdatedAt` 毫秒不混用 |
| §9 WebDAV 三字段 | `document_from_state_only_three_fields` | 回归锁定：新增字段不进文档 |
| §5 双基址 | `resolve_base_urls_official_and_proxy` | 官方/反代（含 `/v0` 后缀）拆分 |
| §5 season→month | `season_to_month_mapping` | WINTER1-3/SPRING4-6/SUMMER7-9/FALL10-12 |
| §6 broadcast 解析 | `parse_broadcast_rrule_local` | RFC5545 → 本地下次播出，golden 向量 |
| §6 选站 | `pick_broadcast_site_by_preference` | `preferredBroadcastSites` 顺序生效 |
| §7 429 退避 | `parse_retry_after` / `rate_limited_backoff` | `Retry-After` → Duration |
| §8 Token | `token_never_in_state_or_webdav_or_log` | Token 不出现在状态/文档/日志 |
| §4 映射优先级 | `mapping_priority_manual_offline_external_titleyear` | 优先级、低置信不强配 |
| §2.5 墓碑 | `resurrection_clears_tombstone` | 重新追番清墓碑、LWW 复活判定 |
| §5 硬不变量 | `original_makes_no_bangumi_request` | original feature 零 Bangumi 请求 |

## 12. 版本与回读兼容

- **Phase 0/1 保持 `STATE_VERSION = 2`**：所有新增均为可选字段。v0.6 读取方（含回退的 `electron/`、`android/` v0.5）行为：
  - 顶层与 `settings` 缺失的键由 `merge_defaults`（约 121-146 行）用默认值重建；
  - 未知键（新增的 `bangumi` 块、同步状态等）在加载时**保留**（`merge_defaults` 只补缺不删未知键），写回时原样持久化；
  - WebDAV 文档由 `document_from_state`/`normalize_document` 只认三字段，新字段不进出文档，双向兼容。
- **Phase 2 引入 `STATE_VERSION = 3`**：`following` 增 `anilistId`/`source`/`mapping`/`mappingPending`；任务增 `subjectId`/`episodeId`/`episodeSortKey`/`episodeType`；旧 `{animeId}-{episode}` id 仍可读。`merge_defaults` 需同时接受 v2 与 v3（硬不变量 7）。
- **回退风险**：一旦升到 v3 且实体主键切为 `subjectId`，v0.5/v0.6 旧读取方按 AniList `id` 处理可能错乱；因此 v3 属破坏性变更，须单独版本 + 迁移说明发布（Phase 5），且保留 `anilistId` 兼容字段。回退安装旧版前应先导出/回滚状态，避免旧版用不完整字段覆盖新数据。

---

## 冻结声明

本文件在 **Phase 0 冻结**，作为 AniLog 标准版 Bangumi 迁移的跨层 schema 契约基准。任何后续阶段（Phase 1-5）对本契约的改动，必须在下方"变更记录"追加变更行（日期 + 阶段 + 变更摘要 + 影响的章节），并同步更新 `LOCAL_MIGRATION_PROGRESS.md` 的对应接口契约小节。文档为公开可提交文件，**不得写入任何凭据、Token、签名材料或真实用户数据**；端点表只列已确认项，不做未验证的端点猜测。

## 变更记录

| 日期 | 阶段 | 变更摘要 | 影响章节 |
|---|---|---|---|
| 2026-09-06 | Phase 0 | 初版冻结 | 全部 |
| 2026-09-06 | Phase 0 | 按官方 v0.yaml 修正集数端点/收藏枚举(2=Done,3=Doing)/单条收藏复数路径/写操作 `-` 路径与 ep_status 书籍限制 | §5 |
