//! AniLog 标准版（Cargo feature `standard`）Bangumi 核心模块 — Phase 0。
//!
//! 本模块是 `docs/BANGUMI_STANDARD_MIGRATION_SCHEMA.md`（Phase 0 冻结契约）的 Rust 侧
//! 单一来源：serde 数据模型、Bangumi v0 API 响应类型、端点/枚举常量、Token 存储契约、
//! 双基址解析、API 错误模型、broadcast（RFC5545 `R/<start>/P<nD|nW>`）解析与选站纯函数、
//! 以及 `BangumiClient` trait 与 fixture 实现。
//!
//! 边界约束（见 `AGENTS.md` / schema 冻结文档）：
//! - 本模块只在 `standard` feature 下编译；Original 产物中不得出现任何 Bangumi 代码。
//! - 业务时间戳单位冻结为**秒**，`syncUpdatedAt` 一律为**毫秒**（与旧状态字段一致）。
//! - Token 只进系统凭据存储，绝不进入状态 JSON、日志、WebDAV 文档或错误信息。

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration as StdDuration;

// ---------------------------------------------------------------------------
// A. serde 数据模型（camelCase 对齐前端 src/types.ts，Phase 0 冻结）
// ---------------------------------------------------------------------------

/// 冲突解决策略（`bangumi.conflictPolicy`）。
///
/// 注意 Bangumi 官方注明收藏 `updated_at` 在评分/评价/章节观看状态修改时可能不更新，
/// 因此冲突解决禁用 `updated_at` LWW，仅参考 `payload hash` 与 `lastChangedBy`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// 以时间戳较新者为准（默认）。
    #[default]
    Latest,
    /// 本地记录优先。
    LocalFirst,
    /// Bangumi 远端记录优先。
    BangumiFirst,
}

/// 主键映射方式（`BangumiSubjectRecord.mapping.method`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingMethod {
    /// 离线映射表（build.rs 内置 bangumi-map.json）。
    #[default]
    Local,
    /// Bangumi API 外部关联 ID（含 AniList id）。
    External,
    /// 标题 + 年份 + 季节 + 集数综合匹配。
    TitleYear,
    /// 用户手动确认/指定。
    Manual,
}

/// 映射置信度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MappingConfidence {
    #[default]
    High,
    Medium,
    Low,
}

/// 集数类型（Bangumi `type` 0-6 归一化后的五值枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EpisodeType {
    #[default]
    Regular,
    Special,
    Movie,
    Ova,
    Unknown,
}

/// 记录最后被哪一方修改（防循环写回：hash 相同则跳过推送）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LastChangedBy {
    #[default]
    Local,
    Bangumi,
    WebDav,
}

/// 播出选站默认优先级（`preferredBroadcastSites` 默认值，schema §6）。
pub fn default_preferred_broadcast_sites() -> Vec<String> {
    vec![
        "bangumi".into(),
        "ani_one".into(),
        "ani_one_asia".into(),
        "gamer".into(),
        "unext".into(),
    ]
}

/// `bangumi` 设置块（挂在状态顶层，与 `settings` 并列；只进本地状态，
/// 绝不进坚果云文档）。Original 版不写该块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiSyncSettings {
    /// 反代/自定义 API 基址；空 = 官方 `https://api.bgm.tv`。非敏感，可进普通设置。
    pub api_base_url: String,
    /// Bangumi 同步总开关。
    pub sync_enabled: bool,
    /// 从 Bangumi 读取收藏。
    pub pull_collections: bool,
    /// 本地追番变化写回 Bangumi（默认关闭）。
    pub push_local_changes: bool,
    /// 完成任务自动上传单集进度（默认关闭）。
    pub push_completed_episodes: bool,
    /// Bangumi 外部状态拉取。
    pub pull_external_status: bool,
    /// 三方冲突策略。
    pub conflict_policy: ConflictPolicy,
    /// 播出选站优先级（schema §6）。
    pub preferred_broadcast_sites: Vec<String>,
}

impl Default for BangumiSyncSettings {
    fn default() -> Self {
        Self {
            api_base_url: String::new(),
            sync_enabled: false,
            pull_collections: true,
            push_local_changes: false,
            push_completed_episodes: false,
            pull_external_status: true,
            conflict_policy: ConflictPolicy::Latest,
            preferred_broadcast_sites: default_preferred_broadcast_sites(),
        }
    }
}

/// 播出信息（`BangumiSubjectRecord.airing`）；`nextAiringAt` 单位为**秒**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiAiring {
    pub next_episode: Option<i64>,
    pub next_airing_at: Option<i64>,
}

/// 主键映射元数据；`updatedAt` 为**秒**级时间戳（0 表示未知）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiMapping {
    pub method: MappingMethod,
    pub confidence: MappingConfidence,
    pub updated_at: i64,
}

impl Default for BangumiMapping {
    fn default() -> Self {
        Self {
            method: MappingMethod::Local,
            confidence: MappingConfidence::High,
            updated_at: 0,
        }
    }
}

/// subject 记录（Phase 2 起 `subjectId` 成为标准版主键；Phase 0 只冻结结构）。
///
/// 时间单位：`lastPulledFromBangumiAt` / `lastPushedToBangumiAt` 为**秒**；
/// `syncUpdatedAt` 为**毫秒**（沿用旧状态 LWW 约定）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiSubjectRecord {
    pub subject_id: i64,
    pub source: String,
    pub title: String,
    pub title_original: Option<String>,
    pub title_romaji: Option<String>,
    pub cover_image: String,
    pub format: Option<String>,
    pub episodes: Option<i64>,
    pub airing: Option<BangumiAiring>,
    pub bangumi_status: Option<String>,
    pub rating: Option<f64>,
    pub watched_episode: Option<i64>,
    /// 兼容字段：旧 AniList id，仅迁移/回退查询用，可为空。
    pub anilist_id: Option<i64>,
    pub mapping: Option<BangumiMapping>,
    /// 迁移中间态标记：尚未建立映射时为 `true`（`{anilistId, subjectId: null, mappingPending: true}`）。
    pub mapping_pending: bool,
    pub last_pulled_from_bangumi_at: Option<i64>,
    pub last_pushed_to_bangumi_at: Option<i64>,
    /// 上次拉取 payload 哈希（冲突解决禁用 `updated_at` LWW，改用 hash）。
    pub last_pulled_payload_hash: Option<String>,
    pub last_pushed_payload_hash: Option<String>,
    pub last_changed_by: Option<LastChangedBy>,
    pub sync_updated_at: Option<i64>,
}

impl Default for BangumiSubjectRecord {
    fn default() -> Self {
        Self {
            subject_id: 0,
            source: "bangumi".into(),
            title: String::new(),
            title_original: None,
            title_romaji: None,
            cover_image: String::new(),
            format: None,
            episodes: None,
            airing: None,
            bangumi_status: None,
            rating: None,
            watched_episode: None,
            anilist_id: None,
            mapping: None,
            // 新记录默认处于迁移中间态（未映射）。
            mapping_pending: true,
            last_pulled_from_bangumi_at: None,
            last_pushed_to_bangumi_at: None,
            last_pulled_payload_hash: None,
            last_pushed_payload_hash: None,
            last_changed_by: None,
            sync_updated_at: None,
        }
    }
}

/// 集数/观看任务记录。旧任务 id 格式 `{animeId}-{episode}` Phase 0 保留可读；
/// 迁移中间态为 `{anilistId: <旧id>, subjectId: null, mappingPending: true}`，不覆盖旧数据。
///
/// 时间单位：`completedAt` / `createdAt` / `airingAt` 为**秒**，`syncUpdatedAt` 为**毫秒**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiEpisodeRecord {
    pub id: String,
    pub subject_id: Option<i64>,
    pub episode_id: Option<i64>,
    /// 集数不假设一定是简单整数。
    pub episode_number: Option<f64>,
    /// 稳定排序键。
    pub episode_sort_key: String,
    pub episode_type: EpisodeType,
    pub title: Option<String>,
    /// `"pending" | "completed"`（对齐旧任务状态）。
    pub status: String,
    pub completed_at: Option<i64>,
    pub created_at: Option<i64>,
    /// 旧任务兼容（AniList id）。
    pub anime_id: Option<i64>,
    pub airing_at: Option<i64>,
    pub sync_updated_at: Option<i64>,
    pub last_changed_by: Option<LastChangedBy>,
}

impl Default for BangumiEpisodeRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            subject_id: None,
            episode_id: None,
            episode_number: None,
            episode_sort_key: String::new(),
            episode_type: EpisodeType::Unknown,
            title: None,
            status: "pending".into(),
            completed_at: None,
            created_at: None,
            anime_id: None,
            airing_at: None,
            sync_updated_at: None,
            last_changed_by: None,
        }
    }
}

/// 本地-only 同步状态五字段：绝不进坚果云文档、不参与业务同步，仅本机展示与
/// 前台过期补偿（15 分钟）使用。时间单位为**秒**。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiSyncStatus {
    pub last_full_sync_at: Option<i64>,
    pub last_web_dav_sync_at: Option<i64>,
    pub last_bangumi_sync_at: Option<i64>,
    pub last_schedule_sync_at: Option<i64>,
    /// 最近一次同步错误摘要（不含 Token）。
    pub last_sync_error: Option<String>,
}

/// Bangumi 用户摘要（前端展示用）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiUserSummary {
    pub id: i64,
    pub username: String,
    pub nickname: String,
    pub avatar_url: Option<String>,
}

// ---------------------------------------------------------------------------
// B. API 响应类型（按官方 v0.yaml 已核实的形状；字段宁可少而准，全部带
//    serde default 以前向兼容。注意 v0 Subject 没有 air_weekday。）
// ---------------------------------------------------------------------------

/// v0 分页信封（`total`/`limit`/`offset`/`data`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Paged<T> {
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub data: Vec<T>,
}

/// Subject/角色图片字段。角色的 `medium`/`small` 是全身立绘的中心方形裁剪
/// 缩略图（只剩腰以下），`large` 是未裁剪全身图——extras 角色映射必须优先
/// `large`（见 lib.rs `subject_image_url`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiSubjectImages {
    pub common: Option<String>,
    pub large: Option<String>,
    pub medium: Option<String>,
    pub small: Option<String>,
    pub grid: Option<String>,
}

/// Subject 评分（v0 rating）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiSubjectRating {
    pub rank: Option<u32>,
    pub total: Option<u32>,
    pub score: Option<f64>,
}

/// Subject 收藏人数统计（v0 collection）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiSubjectCollectionStats {
    pub wish: Option<u32>,
    pub collect: Option<u32>,
    pub doing: Option<u32>,
    pub on_hold: Option<u32>,
    pub dropped: Option<u32>,
}

/// Subject 标签。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiTag {
    pub name: String,
    pub count: u32,
}

/// Subject infobox 条目（`value` 可为字符串或数组）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiInfobox {
    pub key: String,
    pub value: Value,
}

/// v0 Subject（`GET {v0}/subjects/{id}` 与季度列表条目共用；列表条目可能缺字段，
/// 全部带 default）。**v0 Subject 没有 air_weekday 字段**。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiSubject {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    /// 放送日期 `"YYYY-MM-DD"`。
    pub date: Option<String>,
    pub images: Option<BangumiSubjectImages>,
    pub eps: Option<u32>,
    pub rating: Option<BangumiSubjectRating>,
    pub collection: Option<BangumiSubjectCollectionStats>,
    pub tags: Vec<BangumiTag>,
    pub summary: Option<String>,
    pub platform: Option<String>,
    #[serde(default)]
    pub nsfw: bool,
    pub infobox: Vec<BangumiInfobox>,
}

/// v0 Episode（`GET {v0}/episodes?subject_id=...` / `{v0}/subjects/{id}/episodes`）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiEpisode {
    pub id: i64,
    /// v0 字段名是 `type`（0 本篇 / 1 SP / 2 OP / 3 ED / 4 预告 / 5 MAD / 6 其他）。
    #[serde(rename = "type")]
    pub ep_type: u32,
    pub name: String,
    pub name_cn: Option<String>,
    pub sort: Option<f64>,
    pub ep: Option<f64>,
    pub airdate: Option<String>,
    pub duration: Option<String>,
    pub desc: Option<String>,
    pub subject_id: Option<i64>,
    /// 讨论数（v0 为整数）。
    pub comment: Option<i64>,
}

/// v0 角色（`GET {v0}/subjects/{id}/characters` 条目；actors 等未声明字段忽略）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiCharacter {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    pub images: Option<BangumiSubjectImages>,
    pub relation: String,
}

/// v0 关联条目（`GET {v0}/subjects/{id}/subjects` 条目）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiRelatedSubject {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    pub relation: String,
    pub images: Option<BangumiSubjectImages>,
}

/// v0 用户头像。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiUserProfileAvatar {
    pub large: Option<String>,
    pub medium: Option<String>,
    pub small: Option<String>,
}

/// v0 用户（`GET {v0}/me`）。注意 v0 分组字段是 `user_group`。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiUserProfile {
    pub id: i64,
    pub username: String,
    pub nickname: String,
    pub avatar: Option<BangumiUserProfileAvatar>,
    pub sign: Option<String>,
    pub user_group: Option<u32>,
}

/// v0 用户收藏条目（读取与写入共用形状；`type` 即 [`SubjectCollectionType`]）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiCollection {
    pub subject_id: i64,
    pub subject_type: u32,
    pub rate: Option<u8>,
    /// v0 字段名是 `type`（1 wish / 2 done / 3 doing / 4 on_hold / 5 dropped）。
    #[serde(rename = "type")]
    pub collection_type: u32,
    pub tags: Vec<String>,
    pub ep_status: Option<u32>,
    pub vol_status: Option<u32>,
    /// 注意：官方注明评分/评价/章节观看状态修改可能不更新 `updated_at`，
    /// 不得作为冲突 LWW 依据（schema §3.2）。
    pub updated_at: Option<String>,
    pub private: Option<bool>,
    pub comment: Option<String>,
    /// 列表/单条读取响应中的内嵌条目概要（SlimSubject，官方可选字段）。
    pub subject: Option<BangumiSlimSubject>,
}

/// v0 SlimSubject（`UserSubjectCollection.subject`，官方 schema：必填 id/name/
/// name_cn/images/eps 等；本结构只取创建 following 条目所需字段，其余忽略）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiSlimSubject {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    /// 放送日期 `"YYYY-MM-DD"`。
    pub date: Option<String>,
    pub images: Option<BangumiSubjectImages>,
    pub eps: Option<u32>,
}

/// v0 错误响应体（`{"title", "description", "details"}`）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiErrorBody {
    pub title: String,
    pub description: String,
    pub details: Option<Value>,
}

/// `/calendar`（根路径）星期分组。该端点是旧版非 v0 形状，字段宽松处理。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiCalendarDay {
    pub weekday: BangumiCalendarWeekday,
    pub items: Vec<BangumiCalendarItem>,
}

/// `/calendar` 星期信息。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiCalendarWeekday {
    pub id: Option<u32>,
    pub en: Option<String>,
    pub cn: Option<String>,
    pub ja: Option<String>,
}

/// `/calendar` 条目（旧版形状：与 v0 Subject 不同，含 `air_date`/`air_weekday` 等；
/// 未声明字段忽略）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BangumiCalendarItem {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    pub air_date: Option<String>,
    pub images: Option<BangumiSubjectImages>,
    pub eps: Option<u32>,
    pub rating: Option<BangumiSubjectRating>,
    pub url: Option<String>,
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// 枚举常量（端点/写路径使用；文档注释即单一来源）
// ---------------------------------------------------------------------------

/// Bangumi **条目**收藏状态（`GET/PUT/PATCH {v0}/users/.../collections` 的 `type`）。
///
/// 官方 v0 枚举：**2 是 Done（看过），不是 Doing；3 才是 Doing（在看）**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SubjectCollectionType {
    /// 1 = 想看。
    Wish = 1,
    /// 2 = 看过（Done）。
    Done = 2,
    /// 3 = 在看（Doing）。
    Doing = 3,
    /// 4 = 搁置。
    OnHold = 4,
    /// 5 = 弃番。
    Dropped = 5,
}

impl SubjectCollectionType {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Wish),
            2 => Some(Self::Done),
            3 => Some(Self::Doing),
            4 => Some(Self::OnHold),
            5 => Some(Self::Dropped),
            _ => None,
        }
    }
}

/// Bangumi **单集**收藏/进度状态（写进度端点的 body `{"type": N}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EpisodeCollectionType {
    /// 0 = 未收藏。
    NotCollected = 0,
    /// 1 = 想看。
    Wish = 1,
    /// 2 = 看过。
    Watched = 2,
    /// 3 = 抛弃。
    Dropped = 3,
}

impl EpisodeCollectionType {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::NotCollected),
            1 => Some(Self::Wish),
            2 => Some(Self::Watched),
            3 => Some(Self::Dropped),
            _ => None,
        }
    }
}

/// Bangumi 集数类型（v0 Episode `type` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EpType {
    /// 0 = 本篇。
    Main = 0,
    /// 1 = SP（特别篇）。
    Sp = 1,
    /// 2 = OP。
    Op = 2,
    /// 3 = ED。
    Ed = 3,
    /// 4 = 预告。
    Preview = 4,
    /// 5 = MAD。
    Mad = 5,
    /// 6 = 其他。
    Other = 6,
}

impl EpType {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Main),
            1 => Some(Self::Sp),
            2 => Some(Self::Op),
            3 => Some(Self::Ed),
            4 => Some(Self::Preview),
            5 => Some(Self::Mad),
            6 => Some(Self::Other),
            _ => None,
        }
    }
}

/// [`SubjectCollectionType`] 数字 → following 条目 `bangumiStatus` 字符串
/// （前端类型 `src/types.ts`：`'wish'|'doing'|'done'|'on_hold'|'dropped'`）。
pub fn collection_status_name(collection_type: u32) -> Option<&'static str> {
    match collection_type {
        1 => Some("wish"),
        2 => Some("done"),
        3 => Some("doing"),
        4 => Some("on_hold"),
        5 => Some("dropped"),
        _ => None,
    }
}

/// following 条目 `bangumiStatus` 字符串 → 写回用的 v0 收藏 type 数字
/// （状态驱动追踪，任务 4）：wish=1 / done=2 / doing=3 / on_hold=4。
/// 空 / null / 未知值（含 dropped——条目被取消追番后不会留在 following）
/// 视为 doing=3，维持旧版写回行为（向后兼容 anilist 来源迁移条目）。
pub fn collection_status_value(status: &str) -> u32 {
    match status {
        "wish" => SubjectCollectionType::Wish.as_u32(),
        "done" => SubjectCollectionType::Done.as_u32(),
        "on_hold" => SubjectCollectionType::OnHold.as_u32(),
        _ => SubjectCollectionType::Doing.as_u32(),
    }
}

/// 收藏关心字段的规范化 payload 哈希（sha256 十六进制小写；schema §3.2）。
///
/// 规范化规则（本地与远端两侧统一，保证 `H_local == H_remote` 判定可比）：
/// - 只取 `{type, rate, ep_status, comment, tags, private}`，键名固定、
///   serde_json 默认 BTreeMap 顺序输出；
/// - `comment` 空串归一为 `null`（本地不存评价）；
/// - `tags` 排序后参与哈希；
/// - `private=false` 归一为 `null`（本地不跟踪私有标记；`true` 参与哈希，
///   使私有收藏在拉取侧总是被识别为外部变化）。
pub fn collection_payload_hash_parts(
    collection_type: u32,
    rate: Option<u8>,
    ep_status: Option<u32>,
    comment: Option<&str>,
    tags: &[String],
    private: Option<bool>,
) -> String {
    let comment = comment.map(str::trim).filter(|comment| !comment.is_empty());
    let mut tags: Vec<&str> = tags.iter().map(String::as_str).collect();
    tags.sort_unstable();
    let object = serde_json::json!({
        "type": collection_type,
        "rate": rate,
        "ep_status": ep_status,
        "comment": comment,
        "tags": tags,
        "private": private.filter(|is_private| *is_private),
    });
    let canonical = serde_json::to_string(&object).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// 远端收藏记录的 payload 哈希（[`collection_payload_hash_parts`] 的便捷封装）。
pub fn collection_payload_hash(collection: &BangumiCollection) -> String {
    collection_payload_hash_parts(
        collection.collection_type,
        collection.rate,
        collection.ep_status,
        collection.comment.as_deref(),
        &collection.tags,
        collection.private,
    )
}

/// 同步建议（`BangumiSyncReport.suggestions` 条目；前端契约：
/// `{subjectId, nameCn, type}`，`type` 为 collection type 数字）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiSyncSuggestion {
    pub subject_id: i64,
    pub name_cn: Option<String>,
    #[serde(rename = "type")]
    pub collection_type: u32,
}

/// 一次 Bangumi 同步的报告（`bangumi_sync_now` 的 `report` 字段；camelCase）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BangumiSyncReport {
    /// 本次拉取到的远端收藏条数。
    pub pulled: u32,
    /// 因远端 doing 新建本地追番的条数。
    pub followed: u32,
    /// 因远端 dropped 取消本地追番的条数（只删未完成任务）。
    pub unfollowed: u32,
    /// 因远端 done 补完成的本地任务数。
    pub completed_tasks: u32,
    /// wish/on_hold/墓碑阻恢复等仅建议项。
    pub suggestions: Vec<BangumiSyncSuggestion>,
    /// 冲突策略下记录的冲突数（conflictPolicy=latest 时方向不明即计数）。
    pub conflicts: u32,
    /// 成功写回 Bangumi 的请求次数（收藏 + 批量进度）。
    pub pushed: u32,
    /// 错误摘要（不含任何 Token/Authorization 材料）。
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// C. Token 存储
// ---------------------------------------------------------------------------

/// Token 存取错误。`Display` 输出绝不包含 Token 本身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStoreError {
    /// 平台凭据存储错误（携带平台侧消息）。
    Platform(String),
    /// 凭据内容编码/序列化问题。
    Serialization,
    /// 其他错误（参数校验、未接入的平台等）。
    Other(String),
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenStoreError::Platform(message) => write!(f, "凭据存储平台错误：{message}"),
            TokenStoreError::Serialization => write!(f, "凭据内容编码无效"),
            TokenStoreError::Other(message) => write!(f, "凭据存储错误：{message}"),
        }
    }
}

impl std::error::Error for TokenStoreError {}

/// Bangumi Access Token 存储契约：Token 只进系统凭据存储
/// （Windows Credential Manager / Android Keystore），绝不进入
/// 状态 JSON、日志、WebDAV 文档或提交。
pub trait BangumiTokenStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, TokenStoreError>;
    /// 存储前应 trim 并拒绝空值。
    fn store(&self, token: &str) -> Result<(), TokenStoreError>;
    fn clear(&self) -> Result<(), TokenStoreError>;
}

/// Windows Credential Manager 服务名（与 WebDAV 的 `io.anilog.webdav` 区分）。
pub const BANGUMI_CREDENTIAL_SERVICE: &str = "io.anilog.bangumi";
/// 单用户应用使用固定 account。
pub const BANGUMI_CREDENTIAL_ACCOUNT: &str = "default";

/// Windows 实现：Credential Manager（keyring v3，`windows-native` feature）。
#[cfg(target_os = "windows")]
pub struct KeyringTokenStore;

#[cfg(target_os = "windows")]
impl Default for KeyringTokenStore {
    fn default() -> Self {
        Self
    }
}

#[cfg(target_os = "windows")]
impl KeyringTokenStore {
    pub fn new() -> Self {
        Self
    }
    fn entry() -> Result<keyring::Entry, TokenStoreError> {
        keyring::Entry::new(BANGUMI_CREDENTIAL_SERVICE, BANGUMI_CREDENTIAL_ACCOUNT)
            .map_err(TokenStoreError::from)
    }
}

#[cfg(target_os = "windows")]
impl From<keyring::Error> for TokenStoreError {
    fn from(error: keyring::Error) -> Self {
        match error {
            keyring::Error::BadEncoding(_) => TokenStoreError::Serialization,
            // keyring v3 的平台错误载荷是 Box<dyn Error>，取其 Display 消息。
            keyring::Error::PlatformFailure(source) => {
                TokenStoreError::Platform(source.to_string())
            }
            other => TokenStoreError::Other(other.to_string()),
        }
    }
}

#[cfg(target_os = "windows")]
impl BangumiTokenStore for KeyringTokenStore {
    fn load(&self) -> Result<Option<String>, TokenStoreError> {
        match Self::entry()?.get_password() {
            Ok(value) => Ok(Some(value)),
            // 从未存储过密码时 keyring 返回 NoEntry，语义上是"无 Token"。
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn store(&self, token: &str) -> Result<(), TokenStoreError> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Err(TokenStoreError::Other("Bangumi Token 不能为空".into()));
        }
        Self::entry()?.set_password(trimmed)?;
        Ok(())
    }

    fn clear(&self) -> Result<(), TokenStoreError> {
        match Self::entry()?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// 非 Windows 桌面平台的占位实现（与 lib.rs WebDAV 非 Windows 平台写法一致）。
/// Android 在 Phase 1 通过 Keystore 桥（`BangumiTokenStore.java`）接入。
pub struct UnsupportedTokenStore;

impl Default for UnsupportedTokenStore {
    fn default() -> Self {
        Self
    }
}

impl BangumiTokenStore for UnsupportedTokenStore {
    fn load(&self) -> Result<Option<String>, TokenStoreError> {
        Err(TokenStoreError::Platform(
            "当前平台的安全凭据存储尚未接入".into(),
        ))
    }
    fn store(&self, _token: &str) -> Result<(), TokenStoreError> {
        Err(TokenStoreError::Platform(
            "当前平台的安全凭据存储尚未接入".into(),
        ))
    }
    fn clear(&self) -> Result<(), TokenStoreError> {
        Err(TokenStoreError::Platform(
            "当前平台的安全凭据存储尚未接入".into(),
        ))
    }
}

/// 测试用内存实现（进程内，绝不落盘）。仅测试构建存在，绝不进产物。
#[cfg(test)]
pub(crate) struct MemoryTokenStore {
    value: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl Default for MemoryTokenStore {
    fn default() -> Self {
        Self {
            value: std::sync::Mutex::new(None),
        }
    }
}

#[cfg(test)]
impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl BangumiTokenStore for MemoryTokenStore {
    fn load(&self) -> Result<Option<String>, TokenStoreError> {
        Ok(self
            .value
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| TokenStoreError::Other("token store lock poisoned".into()))?)
    }
    fn store(&self, token: &str) -> Result<(), TokenStoreError> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return Err(TokenStoreError::Other("Bangumi Token 不能为空".into()));
        }
        let mut guard = self
            .value
            .lock()
            .map_err(|_| TokenStoreError::Other("token store lock poisoned".into()))?;
        *guard = Some(trimmed.to_string());
        Ok(())
    }
    fn clear(&self) -> Result<(), TokenStoreError> {
        let mut guard = self
            .value
            .lock()
            .map_err(|_| TokenStoreError::Other("token store lock poisoned".into()))?;
        *guard = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// D. 双基址与端点 URL
// ---------------------------------------------------------------------------

/// 官方根地址（`/calendar` 在根路径，v0 接口在 `/v0` 前缀下；**无 /seasons**）。
pub const OFFICIAL_BANGUMI_ROOT: &str = "https://api.bgm.tv";

/// 双基址：`root`（`/calendar` 等）与 `v0`（v0 接口前缀）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BangumiBaseUrls {
    pub root: String,
    pub v0: String,
}

/// 解析双基址（纯函数）：
/// - 配置为空/空白 → 官方 `root = https://api.bgm.tv`、`v0 = https://api.bgm.tv/v0`；
/// - 非 `https://` 前缀的配置视为无效，**回退官方基址**（明确可测语义：绝不降级为 http）；
/// - 以 `/v0` 结尾（忽略末尾斜杠）→ `root` 为剥离 `/v0` 后的串、`v0` 为原串；
/// - 否则 → `root` 为原串、`v0` 为原串 + `/v0`。
pub fn resolve_base_urls(configured: &str) -> BangumiBaseUrls {
    let trimmed = configured.trim();
    if trimmed.is_empty() || !trimmed.starts_with("https://") {
        return official_base_urls();
    }
    let normalized = trimmed.trim_end_matches('/');
    if let Some(root) = normalized.strip_suffix("/v0") {
        BangumiBaseUrls {
            root: root.to_string(),
            v0: normalized.to_string(),
        }
    } else {
        BangumiBaseUrls {
            root: normalized.to_string(),
            v0: format!("{normalized}/v0"),
        }
    }
}

fn official_base_urls() -> BangumiBaseUrls {
    BangumiBaseUrls {
        root: OFFICIAL_BANGUMI_ROOT.to_string(),
        v0: format!("{OFFICIAL_BANGUMI_ROOT}/v0"),
    }
}

/// 收藏读取/写入的固定 subject_type：2 = 动画（schema §5）。
pub const SUBJECT_TYPE_ANIME: u32 = 2;
/// 季度列表分页 limit 上限。
pub const SEASON_SUBJECTS_LIMIT_MAX: u32 = 50;
/// 集数列表分页 limit 上限（注意：集数是 `/v0/episodes`）。
pub const SUBJECT_EPISODES_LIMIT_MAX: u32 = 200;

/// `GET {root}/calendar`（根路径，非 /v0 下）。
pub fn calendar_url(base: &BangumiBaseUrls) -> String {
    format!("{}/calendar", base.root)
}

/// `GET {v0}/subjects?type=2&year=&month=&limit=&offset=`（季度列表；无 /seasons 端点）。
pub fn season_subjects_url(
    base: &BangumiBaseUrls,
    year: u32,
    month: u32,
    limit: u32,
    offset: u32,
) -> String {
    format!(
        "{}/subjects?type=2&year={year}&month={month}&limit={}&offset={offset}",
        base.v0,
        limit.min(SEASON_SUBJECTS_LIMIT_MAX)
    )
}

/// `GET {v0}/subjects/{id}`。
pub fn subject_detail_url(base: &BangumiBaseUrls, subject_id: i64) -> String {
    format!("{}/subjects/{subject_id}", base.v0)
}

/// `GET {v0}/episodes?subject_id=&type=&limit=&offset=`（集数是 /v0/episodes）。
pub fn subject_episodes_url(
    base: &BangumiBaseUrls,
    subject_id: i64,
    limit: u32,
    offset: u32,
) -> String {
    format!(
        "{}/episodes?subject_id={subject_id}&limit={}&offset={offset}",
        base.v0,
        limit.min(SUBJECT_EPISODES_LIMIT_MAX)
    )
}

/// `GET {v0}/subjects/{id}/characters`。
pub fn subject_characters_url(base: &BangumiBaseUrls, subject_id: i64) -> String {
    format!("{}/subjects/{subject_id}/characters", base.v0)
}

/// `GET {v0}/subjects/{id}/subjects`（关联条目）。
pub fn subject_related_url(base: &BangumiBaseUrls, subject_id: i64) -> String {
    format!("{}/subjects/{subject_id}/subjects", base.v0)
}

/// `GET {v0}/me`（testConnection 用）。
pub fn me_url(base: &BangumiBaseUrls) -> String {
    format!("{}/me", base.v0)
}

/// `GET {v0}/users/{username}/collections?subject_type=&type=&limit=&offset=`。
/// username 来自 `GET {v0}/me`。
#[allow(clippy::too_many_arguments)]
pub fn user_collections_url(
    base: &BangumiBaseUrls,
    username: &str,
    subject_type: u32,
    limit: u32,
    offset: u32,
) -> String {
    format!(
        "{}/users/{username}/collections?subject_type={subject_type}&limit={}&offset={offset}",
        base.v0,
        limit.min(SUBJECT_EPISODES_LIMIT_MAX)
    )
}

/// `GET {v0}/users/{username}/collections/{subject_id}`（注意路径段是复数 collections）。
pub fn user_collection_url(base: &BangumiBaseUrls, username: &str, subject_id: i64) -> String {
    format!("{}/users/{username}/collections/{subject_id}", base.v0)
}

/// `POST|PATCH {v0}/users/-/collections/{subject_id}`。
/// 官方 spec 用 `-` 占位 = 当前 token 用户。
pub fn update_collection_url(base: &BangumiBaseUrls, subject_id: i64) -> String {
    format!("{}/users/-/collections/{subject_id}", base.v0)
}

/// `PUT {v0}/users/-/collections/-/episodes/{episode_id}`，body `{"type": N}`。
/// `-` 占位 = 当前 token 用户（官方 spec）。
pub fn episode_progress_url(base: &BangumiBaseUrls, episode_id: i64) -> String {
    format!("{}/users/-/collections/-/episodes/{episode_id}", base.v0)
}

/// `PATCH {v0}/users/-/collections/{subject_id}/episodes`，
/// body `{"episode_id": [...], "type": N}`。`-` 占位 = 当前 token 用户（官方 spec）。
pub fn episode_progress_batch_url(base: &BangumiBaseUrls, subject_id: i64) -> String {
    format!("{}/users/-/collections/{subject_id}/episodes", base.v0)
}

/// 季度 → 起始月份三元组（与 lib.rs 的季节字符串风格一致：大写英文）。
/// 未知季节返回 `[0, 0, 0]`，调用方应视为无效输入。
pub fn season_months(season: &str) -> [u32; 3] {
    match season.trim().to_ascii_uppercase().as_str() {
        "WINTER" => [1, 2, 3],
        "SPRING" => [4, 5, 6],
        "SUMMER" => [7, 8, 9],
        "FALL" => [10, 11, 12],
        _ => [0, 0, 0],
    }
}

// ---------------------------------------------------------------------------
// E. 错误模型
// ---------------------------------------------------------------------------

/// Bangumi API 错误。`Display` 输出绝不包含 Authorization 头或 Token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BangumiApiError {
    /// 401。
    Unauthorized { message: String },
    /// 403。
    Forbidden { message: String },
    /// 404。
    NotFound { message: String },
    /// 409。
    Conflict { message: String },
    /// 429；`retry_after` 来自 `Retry-After` 头。
    RateLimited {
        retry_after: Option<StdDuration>,
        message: String,
    },
    /// 5xx 及其他未分类 HTTP 状态。
    ServerError(u16),
    /// 网络层错误（DNS/连接/中断等）。
    Network(String),
    /// 请求超时。
    Timeout,
    /// 响应解析失败。
    Parse(String),
}

impl fmt::Display for BangumiApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BangumiApiError::Unauthorized { message } => {
                write!(f, "Bangumi 认证失败（401）：{message}")
            }
            BangumiApiError::Forbidden { message } => {
                write!(f, "Bangumi 拒绝访问（403）：{message}")
            }
            BangumiApiError::NotFound { message } => {
                write!(f, "Bangumi 资源不存在（404）：{message}")
            }
            BangumiApiError::Conflict { message } => {
                write!(f, "Bangumi 状态冲突（409）：{message}")
            }
            BangumiApiError::RateLimited {
                retry_after,
                message,
            } => match retry_after {
                Some(duration) => write!(
                    f,
                    "Bangumi 请求过于频繁（429），{} 秒后重试：{message}",
                    duration.as_secs()
                ),
                None => write!(f, "Bangumi 请求过于频繁（429）：{message}"),
            },
            BangumiApiError::ServerError(status) => {
                write!(f, "Bangumi 服务端错误（HTTP {status}）")
            }
            BangumiApiError::Network(message) => write!(f, "Bangumi 网络错误：{message}"),
            BangumiApiError::Timeout => write!(f, "Bangumi 请求超时"),
            BangumiApiError::Parse(message) => write!(f, "Bangumi 响应解析失败：{message}"),
        }
    }
}

impl std::error::Error for BangumiApiError {}

/// 由 HTTP 状态 + 响应体 + `Retry-After` 头构造错误。
/// 解析 [`BangumiErrorBody`] 的 `title`/`description` 进 `Display`；
/// **绝不**把 Authorization 头或 Token 拼进错误信息。
pub fn from_status(status: u16, body: &str, retry_after_header: Option<&str>) -> BangumiApiError {
    let parsed: BangumiErrorBody = serde_json::from_str(body).unwrap_or_default();
    let message = compose_error_message(&parsed, body);
    match status {
        401 => BangumiApiError::Unauthorized { message },
        403 => BangumiApiError::Forbidden { message },
        404 => BangumiApiError::NotFound { message },
        409 => BangumiApiError::Conflict { message },
        429 => BangumiApiError::RateLimited {
            retry_after: parse_retry_after(retry_after_header),
            message,
        },
        // 其余状态（含 4xx 如 400、5xx）一律按服务端/未分类错误处理。
        other => BangumiApiError::ServerError(other),
    }
}

fn compose_error_message(parsed: &BangumiErrorBody, raw_body: &str) -> String {
    match (parsed.title.trim(), parsed.description.trim()) {
        ("", "") => {
            // 非 JSON 响应体（如网关 HTML）截断后原样携带，便于排障。
            let truncated: String = raw_body.trim().chars().take(160).collect();
            if truncated.is_empty() {
                "未知错误".into()
            } else {
                truncated
            }
        }
        ("", description) => description.to_string(),
        (title, "") => title.to_string(),
        (title, description) => format!("{title}：{description}"),
    }
}

/// 解析 `Retry-After` 头。支持 delta-seconds（整数秒）形式；
/// HTTP-date 形式（如 `Wed, 21 Oct 2026 07:28:00 GMT`）Phase 0 简化为返回
/// `None`（调用方按无提示退避处理），避免引入额外的日期解析分支。
pub fn parse_retry_after(header: Option<&str>) -> Option<StdDuration> {
    let raw = header?.trim();
    // HTTP-date 含字母与逗号，直接解析 u64 会失败并返回 None。
    raw.parse::<u64>().ok().map(StdDuration::from_secs)
}

// ---------------------------------------------------------------------------
// F. broadcast 解析与选站（纯函数；Phase 4 将在 Java 复刻同逻辑，
//    golden 向量共享：src-tauri/fixtures/bangumi/broadcast-vectors.json）
// ---------------------------------------------------------------------------

/// 选站与 recurrence 步进的安全上限，防止异常输入导致无限循环。
const MAX_RECURRENCE_STEPS: usize = 100_000;

/// 站点级播出时间源（与 JSON 表示解耦：v2 离线映射里键名是 `s`/`i`，
/// golden 向量 fixture 里是 `site`/`id`，本结构只关心站点名与时间字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastSite<'a> {
    pub site: &'a str,
    pub begin: Option<&'a str>,
    pub broadcast: Option<&'a str>,
}

/// 按 `preferred` 站点优先级选出有效播出时间源（Phase 2 播出四级优先 ①；
/// 与 [`next_broadcast_after`] 的选站逻辑一致，供需要规则起点/周期做逐期
/// 推算的调用方——如季度列表 nextAiringEpisode 与本地逐期调度——复用）。
/// 返回 `(begin, broadcast)`：命中站点时返回站点级字段，否则返回条目级字段。
pub fn select_broadcast_source<'a>(
    begin: Option<&'a str>,
    broadcast: Option<&'a str>,
    sites: &[BroadcastSite<'a>],
    preferred: &[String],
) -> (Option<&'a str>, Option<&'a str>) {
    for name in preferred {
        if let Some(site) = sites.iter().find(|site| {
            site.site == name.as_str() && (site.begin.is_some() || site.broadcast.is_some())
        }) {
            return (site.begin, site.broadcast);
        }
    }
    (begin, broadcast)
}

/// 计算下一次播出时刻（严格晚于 `after`），返回 UTC 时刻。
///
/// 1. 选时间源：按 `preferred` 顺序找第一个有 begin/broadcast 的站点；
///    否则使用条目级 begin/broadcast（见 [`select_broadcast_source`]）；
/// 2. 有 broadcast `R/<start>/P<nD|nW>`：从 start 起按周期步进找第一个
///    `> after` 的时刻（周期支持 D/W，够 bangumi-data 的周播场景；其他
///    RFC5545 周期形式不支持，返回 None）；
/// 3. 只有 begin：begin > after 时返回 begin，否则 None（电影等一次性播出）；
/// 4. 全无 → None。
pub fn next_broadcast_after(
    begin: Option<&str>,
    broadcast: Option<&str>,
    sites: &[BroadcastSite<'_>],
    preferred: &[String],
    after: DateTime<impl TimeZone>,
) -> Option<DateTime<Utc>> {
    let (selected_begin, selected_broadcast) =
        select_broadcast_source(begin, broadcast, sites, preferred);
    if let Some(rule) = selected_broadcast {
        if let Some(occurrence) = next_recurrence(rule, &after) {
            return Some(occurrence);
        }
    }
    // broadcast 缺失或无法解析时，回落为一次性 begin（电影/OVA 场景）。
    let start = parse_instant(selected_begin?)?;
    (start > after).then_some(start)
}

/// 解析 `R/<start>/P<nD|nW>` 为（起点 UTC, 周期）。
/// 供 [`next_recurrence`] 步进与 Phase 2 的本地逐期调度生成复用。
pub fn parse_recurrence_rule(rule: &str) -> Option<(DateTime<Utc>, Duration)> {
    let rest = rule.trim().strip_prefix('R')?.strip_prefix('/')?;
    let (start_raw, period_raw) = rest.split_once('/')?;
    let start = parse_instant(start_raw)?;
    let step = parse_period(period_raw)?;
    Some((start, step))
}

/// 解析 `R/<start>/P<nD|nW>` 并步进到第一个严格晚于 `after` 的时刻。
fn next_recurrence(rule: &str, after: &DateTime<impl TimeZone>) -> Option<DateTime<Utc>> {
    let (start, step) = parse_recurrence_rule(rule)?;
    let mut occurrence = start;
    for _ in 0..MAX_RECURRENCE_STEPS {
        if occurrence > *after {
            return Some(occurrence);
        }
        occurrence = occurrence.checked_add_signed(step)?;
    }
    None
}

/// 解析周期段：仅支持 `P<nD>` / `P<nW>`（周播覆盖 bangumi-data 现有数据；
/// 更复杂的 RFC5545 周期形式如 `PT`/复合周期不支持，返回 None）。
fn parse_period(period: &str) -> Option<Duration> {
    let body = period.trim().strip_prefix('P')?;
    if body.len() < 2 {
        return None;
    }
    let (digits, unit) = body.split_at(body.len() - 1);
    let count: i64 = digits.parse().ok()?;
    match unit {
        "D" => Some(Duration::days(count)),
        "W" => Some(Duration::days(count * 7)),
        _ => None,
    }
}

/// 解析 ISO8601 时间戳（含毫秒 / `Z` / 数字偏移），统一转 UTC。
pub(crate) fn parse_instant(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = match trimmed.strip_suffix(['Z', 'z']) {
        Some(rest) => format!("{rest}+00:00"),
        None => trimmed.to_string(),
    };
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// G. BangumiClient trait + FixtureBangumiClient
// ---------------------------------------------------------------------------

/// Bangumi 数据客户端抽象（async fn in trait；Phase 1 提供基于 reqwest 双基址的
/// `HttpBangumiClient`，测试用 [`FixtureBangumiClient`]）。不追求 dyn 兼容。
pub trait BangumiClient {
    /// `GET {root}/calendar`（根路径）。
    fn get_calendar(
        &self,
    ) -> impl Future<Output = Result<Vec<BangumiCalendarDay>, BangumiApiError>>;
    /// `GET {v0}/subjects?type=2&year=&month=&limit=&offset=`（季度列表分页）。
    fn get_season_subjects(
        &self,
        year: u32,
        month: u32,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<Paged<BangumiSubject>, BangumiApiError>>;
    /// `GET {v0}/subjects/{id}`。
    fn get_subject_detail(
        &self,
        subject_id: i64,
    ) -> impl Future<Output = Result<BangumiSubject, BangumiApiError>>;
    /// `GET {v0}/episodes?subject_id=&limit=&offset=`（集数是 /v0/episodes）。
    fn get_subject_episodes(
        &self,
        subject_id: i64,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<Paged<BangumiEpisode>, BangumiApiError>>;
    /// `GET {v0}/subjects/{id}/characters`。
    fn get_subject_characters(
        &self,
        subject_id: i64,
    ) -> impl Future<Output = Result<Vec<BangumiCharacter>, BangumiApiError>>;
    /// `GET {v0}/subjects/{id}/subjects`。
    fn get_subject_related(
        &self,
        subject_id: i64,
    ) -> impl Future<Output = Result<Vec<BangumiRelatedSubject>, BangumiApiError>>;
    /// `GET {v0}/me`。
    fn get_user_profile(&self)
    -> impl Future<Output = Result<BangumiUserProfile, BangumiApiError>>;
    /// `GET {v0}/users/{username}/collections?subject_type=&limit=&offset=`。
    fn get_user_collections(
        &self,
        username: &str,
        subject_type: u32,
        limit: u32,
        offset: u32,
    ) -> impl Future<Output = Result<Paged<BangumiCollection>, BangumiApiError>>;
    /// `GET {v0}/users/{username}/collections/{subject_id}`。
    fn get_user_collection(
        &self,
        username: &str,
        subject_id: i64,
    ) -> impl Future<Output = Result<BangumiCollection, BangumiApiError>>;
    /// `POST|PATCH {v0}/users/-/collections/{subject_id}`（204 无响应体）。
    fn update_collection(
        &self,
        subject_id: i64,
        payload: &Value,
    ) -> impl Future<Output = Result<(), BangumiApiError>>;
    /// `PUT {v0}/users/-/collections/-/episodes/{episode_id}`，body `{"type": N}`（204）。
    fn update_episode_progress(
        &self,
        episode_id: i64,
        collection_type: EpisodeCollectionType,
    ) -> impl Future<Output = Result<(), BangumiApiError>>;
    /// `PATCH {v0}/users/-/collections/{subject_id}/episodes`，
    /// body `{"episode_id": [...], "type": N}`（204）。
    fn update_episode_progress_batch(
        &self,
        subject_id: i64,
        episode_ids: &[i64],
        collection_type: EpisodeCollectionType,
    ) -> impl Future<Output = Result<(), BangumiApiError>>;
    /// 连通性测试（等价 `GET {v0}/me`）。
    fn test_connection(&self) -> impl Future<Output = Result<BangumiUserProfile, BangumiApiError>>;
}

/// fixture 客户端：所有方法返回 `src-tauri/fixtures/bangumi/` 内置样本的解析结果。
/// 写方法按官方 204 无响应体返回 `Ok(())`；错误场景用 [`stub_error`](Self::stub_error)。
pub struct FixtureBangumiClient;

impl Default for FixtureBangumiClient {
    fn default() -> Self {
        Self
    }
}

impl FixtureBangumiClient {
    pub fn new() -> Self {
        Self
    }

    /// 测试辅助：构造与真实 HTTP 状态对应的错误（429 附带 `Retry-After: 120`）。
    pub fn stub_error(status: u16) -> BangumiApiError {
        let body = match status {
            401 => include_str!("../fixtures/bangumi/error-401.json"),
            429 => include_str!("../fixtures/bangumi/error-429.json"),
            _ => "",
        };
        let retry_after = (status == 429).then_some("120");
        from_status(status, body, retry_after)
    }

    fn parse<T: DeserializeOwned>(raw: &'static str) -> T {
        serde_json::from_str(raw).expect("bangumi fixture must parse")
    }
}

impl BangumiClient for FixtureBangumiClient {
    async fn get_calendar(&self) -> Result<Vec<BangumiCalendarDay>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/calendar.json"
        )))
    }

    async fn get_season_subjects(
        &self,
        _year: u32,
        _month: u32,
        _limit: u32,
        _offset: u32,
    ) -> Result<Paged<BangumiSubject>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/subjects-page.json"
        )))
    }

    async fn get_subject_detail(
        &self,
        _subject_id: i64,
    ) -> Result<BangumiSubject, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/subject-detail.json"
        )))
    }

    async fn get_subject_episodes(
        &self,
        _subject_id: i64,
        _limit: u32,
        _offset: u32,
    ) -> Result<Paged<BangumiEpisode>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/subject-episodes.json"
        )))
    }

    async fn get_subject_characters(
        &self,
        _subject_id: i64,
    ) -> Result<Vec<BangumiCharacter>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/subject-characters.json"
        )))
    }

    async fn get_subject_related(
        &self,
        _subject_id: i64,
    ) -> Result<Vec<BangumiRelatedSubject>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/subject-related.json"
        )))
    }

    async fn get_user_profile(&self) -> Result<BangumiUserProfile, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/user-profile.json"
        )))
    }

    async fn get_user_collections(
        &self,
        _username: &str,
        _subject_type: u32,
        _limit: u32,
        _offset: u32,
    ) -> Result<Paged<BangumiCollection>, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/user-collections-page.json"
        )))
    }

    async fn get_user_collection(
        &self,
        _username: &str,
        _subject_id: i64,
    ) -> Result<BangumiCollection, BangumiApiError> {
        Ok(Self::parse(include_str!(
            "../fixtures/bangumi/user-collection.json"
        )))
    }

    async fn update_collection(
        &self,
        _subject_id: i64,
        _payload: &Value,
    ) -> Result<(), BangumiApiError> {
        Ok(())
    }

    async fn update_episode_progress(
        &self,
        _episode_id: i64,
        _collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        Ok(())
    }

    async fn update_episode_progress_batch(
        &self,
        _subject_id: i64,
        _episode_ids: &[i64],
        _collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        Ok(())
    }

    async fn test_connection(&self) -> Result<BangumiUserProfile, BangumiApiError> {
        self.get_user_profile().await
    }
}

// ---------------------------------------------------------------------------
// H. HttpBangumiClient（Phase 1）：真实 HTTP 版 BangumiClient
// ---------------------------------------------------------------------------

/// `Authorization: Bearer <token>` 请求头的构造。
///
/// 该串**只**用于请求头：任何日志、错误 Display/Debug、状态 JSON 都不得包含它
/// （由 `error_display_never_contains_token_material` 系列测试锁定）。
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// 全局并发限流许可数（schema §7 纪律：1-2；独立于标题 resolver 的 450ms 串行锁）。
pub const HTTP_CLIENT_CONCURRENCY: usize = 2;

/// [`BangumiClient`] 的真实 HTTP 实现（reqwest + rustls）。
///
/// 凭据纪律：
/// - 客户端本体**不持久化任何 Token**；Token 每次请求经参数传入，仅在发送瞬间
///   注入 `Authorization` 头，绝不进入日志或错误信息；
/// - 并发纪律：tokio::sync::Semaphore 全局限流（[`HTTP_CLIENT_CONCURRENCY`]）；
///   **Phase 1 不做自动重试**：429 直接返回
///   [`BangumiApiError::RateLimited`]（含 `Retry-After`），**调用方决定退避**
///   （尊重 Retry-After + 指数退避，schema §7）；
/// - 错误映射：HTTP 状态码经 [`from_status`]（解析 ErrorBody）；reqwest 错误
///   `is_timeout` → Timeout、`is_connect`/`is_request` → Network。错误信息只含
///   方法与 URL 路径（不含 query；本客户端 query 无敏感信息，Authorization 头
///   则绝不入错误）。
pub struct HttpBangumiClient {
    client: reqwest::Client,
    /// 主基址（反代或官方）。
    base: BangumiBaseUrls,
    /// `/calendar` 回落基址（默认官方；测试注入 mock 以验证回落语义）。
    fallback: BangumiBaseUrls,
    limiter: Arc<tokio::sync::Semaphore>,
}

impl HttpBangumiClient {
    /// 以现有 reqwest::Client 构造（推荐：复用 AppContext.client，保持 UA 与连接池一致）。
    /// 回落基址为官方 `https://api.bgm.tv`。
    pub fn new(client: reqwest::Client, base: BangumiBaseUrls) -> Self {
        Self::with_fallback(client, base, official_base_urls())
    }

    /// 显式指定回落基址（`/calendar` 反代回落测试用；生产一律走 [`Self::new`]）。
    pub fn with_fallback(
        client: reqwest::Client,
        base: BangumiBaseUrls,
        fallback: BangumiBaseUrls,
    ) -> Self {
        Self {
            client,
            base,
            fallback,
            limiter: Arc::new(tokio::sync::Semaphore::new(HTTP_CLIENT_CONCURRENCY)),
        }
    }

    /// 自带 reqwest Client 的构造：UA `AniLog Tauri/<CARGO_PKG_VERSION>`，
    /// 整体超时 15s（与 lib.rs 主客户端一致，防止无超时挂起）。
    pub fn with_base(base: BangumiBaseUrls) -> reqwest::Result<Self> {
        let builder = reqwest::Client::builder()
            .user_agent(concat!("AniLog Tauri/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(15));
        // Unit-test mock servers must not be redirected through a developer's
        // system proxy; the production AppContext client keeps proxy discovery.
        #[cfg(test)]
        let builder = builder.no_proxy();
        let client = builder.build()?;
        Ok(Self::new(client, base))
    }

    pub fn base(&self) -> &BangumiBaseUrls {
        &self.base
    }

    /// 将 Token 与客户端绑定成一个实现 [`BangumiClient`] 的视图。
    /// Token 只存在于该视图引用中，不进入 HttpBangumiClient 本体。
    pub fn bind<'a>(&'a self, token: &'a str) -> TokenBoundBangumiClient<'a> {
        TokenBoundBangumiClient {
            client: self,
            token,
        }
    }

    /// 统一请求入口：限流 → 注入 Bearer → 发送 → 状态码映射。
    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> Result<reqwest::Response, BangumiApiError> {
        // Phase 1 不做自动重试；429 直接返回 RateLimited{retry_after}，调用方决定退避。
        let _permit = self
            .limiter
            .acquire()
            .await
            .map_err(|_| BangumiApiError::Network("Bangumi 并发信号量已关闭".into()))?;
        let mut request = self
            .client
            .request(method.clone(), url)
            .header(reqwest::header::ACCEPT, "application/json");
        // Token 只在请求时经参数注入，客户端不持久化；该头绝不进入日志/错误。
        if let Some(token) = token {
            request = request.header(reqwest::header::AUTHORIZATION, bearer(token));
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| map_request_error(&method, url, error))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response
            .text()
            .await
            .map_err(|error| map_request_error(&method, url, error))?;
        Err(from_status(status.as_u16(), &text, retry_after.as_deref()))
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: &str,
        token: Option<&str>,
        body: Option<&Value>,
    ) -> Result<T, BangumiApiError> {
        let response = self.send(method.clone(), url, token, body).await?;
        let text = response
            .text()
            .await
            .map_err(|error| map_request_error(&method, url, error))?;
        serde_json::from_str(&text)
            .map_err(|error| BangumiApiError::Parse(format!("Bangumi 响应解析失败：{error}")))
    }

    async fn send_unit(
        &self,
        method: reqwest::Method,
        url: &str,
        token: &str,
        body: &Value,
    ) -> Result<(), BangumiApiError> {
        // 官方写端点成功返回 204 无响应体。
        self.send(method, url, Some(token), Some(body))
            .await
            .map(|_| ())
    }

    /// `GET {root}/calendar`。
    ///
    /// **唯一具有回落语义的方法**：主基址为反代（root ≠ 回落基址）时先请求反代
    /// root；遇 Network/Timeout/ServerError(5xx) 再请求官方 root 一次；两次都失败
    /// 才返回错误（优先返回反代错误）。主数据拉取的回落策略 Phase 2 另行设计，
    /// 不得复用此处逻辑。
    pub async fn get_calendar(&self) -> Result<Vec<BangumiCalendarDay>, BangumiApiError> {
        if self.base.root == self.fallback.root {
            return self.get_calendar_from(&calendar_url(&self.base)).await;
        }
        let primary_error = match self.get_calendar_from(&calendar_url(&self.base)).await {
            Ok(days) => return Ok(days),
            Err(error) => error,
        };
        // 只有网络层失败与 5xx 才回落官方一次；401/404/429 等直接透传。
        match primary_error {
            BangumiApiError::Network(_)
            | BangumiApiError::Timeout
            | BangumiApiError::ServerError(_) => self
                .get_calendar_from(&calendar_url(&self.fallback))
                .await
                .map_err(|_fallback_error| primary_error),
            other => Err(other),
        }
    }

    async fn get_calendar_from(
        &self,
        url: &str,
    ) -> Result<Vec<BangumiCalendarDay>, BangumiApiError> {
        self.send_json(reqwest::Method::GET, url, None, None).await
    }

    /// `GET {v0}/subjects?type=2&year=&month=&limit=&offset=`（公开端点）。
    pub async fn get_season_subjects(
        &self,
        year: u32,
        month: u32,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiSubject>, BangumiApiError> {
        let url = season_subjects_url(&self.base, year, month, limit, offset);
        self.send_json(reqwest::Method::GET, &url, None, None).await
    }

    /// `GET {v0}/subjects/{id}`（公开端点）。
    pub async fn get_subject_detail(
        &self,
        subject_id: i64,
    ) -> Result<BangumiSubject, BangumiApiError> {
        let url = subject_detail_url(&self.base, subject_id);
        self.send_json(reqwest::Method::GET, &url, None, None).await
    }

    /// `GET {v0}/episodes?subject_id=&limit=&offset=`（公开端点；集数是 /v0/episodes）。
    pub async fn get_subject_episodes(
        &self,
        subject_id: i64,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiEpisode>, BangumiApiError> {
        let url = subject_episodes_url(&self.base, subject_id, limit, offset);
        self.send_json(reqwest::Method::GET, &url, None, None).await
    }

    /// `GET {v0}/subjects/{id}/characters`（公开端点）。
    pub async fn get_subject_characters(
        &self,
        subject_id: i64,
    ) -> Result<Vec<BangumiCharacter>, BangumiApiError> {
        let url = subject_characters_url(&self.base, subject_id);
        self.send_json(reqwest::Method::GET, &url, None, None).await
    }

    /// `GET {v0}/subjects/{id}/subjects`（公开端点）。
    pub async fn get_subject_related(
        &self,
        subject_id: i64,
    ) -> Result<Vec<BangumiRelatedSubject>, BangumiApiError> {
        let url = subject_related_url(&self.base, subject_id);
        self.send_json(reqwest::Method::GET, &url, None, None).await
    }

    /// `GET {v0}/me`（Bearer 认证）。
    pub async fn get_user_profile(
        &self,
        token: &str,
    ) -> Result<BangumiUserProfile, BangumiApiError> {
        let url = me_url(&self.base);
        self.send_json(reqwest::Method::GET, &url, Some(token), None)
            .await
    }

    /// `GET {v0}/users/{username}/collections?subject_type=&limit=&offset=`（Bearer）。
    pub async fn get_user_collections(
        &self,
        token: &str,
        username: &str,
        subject_type: u32,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiCollection>, BangumiApiError> {
        let url = user_collections_url(&self.base, username, subject_type, limit, offset);
        self.send_json(reqwest::Method::GET, &url, Some(token), None)
            .await
    }

    /// `GET {v0}/users/{username}/collections/{subject_id}`（Bearer；复数 collections）。
    pub async fn get_user_collection(
        &self,
        token: &str,
        username: &str,
        subject_id: i64,
    ) -> Result<BangumiCollection, BangumiApiError> {
        let url = user_collection_url(&self.base, username, subject_id);
        self.send_json(reqwest::Method::GET, &url, Some(token), None)
            .await
    }

    /// `POST|PATCH {v0}/users/-/collections/{subject_id}`（Bearer）。
    /// `create=true`（无收藏记录）→ POST；否则 PATCH。官方以 `-` 占位当前 token 用户。
    pub async fn update_collection(
        &self,
        token: &str,
        subject_id: i64,
        payload: &Value,
        create: bool,
    ) -> Result<(), BangumiApiError> {
        let url = update_collection_url(&self.base, subject_id);
        let method = if create {
            reqwest::Method::POST
        } else {
            reqwest::Method::PATCH
        };
        self.send_unit(method, &url, token, payload).await
    }

    /// `PUT {v0}/users/-/collections/-/episodes/{episode_id}`，body `{"type": N}`（Bearer）。
    pub async fn update_episode_progress(
        &self,
        token: &str,
        episode_id: i64,
        collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        let url = episode_progress_url(&self.base, episode_id);
        let body = serde_json::json!({"type": collection_type.as_u32()});
        self.send_unit(reqwest::Method::PUT, &url, token, &body)
            .await
    }

    /// `PATCH {v0}/users/-/collections/{subject_id}/episodes`，
    /// body `{"episode_id": [...], "type": N}`（Bearer）。
    pub async fn update_episode_progress_batch(
        &self,
        token: &str,
        subject_id: i64,
        episode_ids: &[i64],
        collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        let url = episode_progress_batch_url(&self.base, subject_id);
        let body = serde_json::json!({"episode_id": episode_ids, "type": collection_type.as_u32()});
        self.send_unit(reqwest::Method::PATCH, &url, token, &body)
            .await
    }

    /// 连通性测试（等价 `GET {v0}/me`，Bearer 认证）。
    pub async fn test_connection(
        &self,
        token: &str,
    ) -> Result<BangumiUserProfile, BangumiApiError> {
        self.get_user_profile(token).await
    }
}

/// reqwest 错误 → [`BangumiApiError`]。信息只含方法与 URL 路径（不含 query），
/// 绝不包含 Authorization 头或 Token。
fn map_request_error(
    method: &reqwest::Method,
    url: &str,
    error: reqwest::Error,
) -> BangumiApiError {
    if error.is_timeout() {
        return BangumiApiError::Timeout;
    }
    let path = url.split('?').next().unwrap_or(url);
    let detail = if error.is_connect() {
        "连接失败"
    } else {
        "请求失败"
    };
    BangumiApiError::Network(format!("HTTP {method} {path}：{detail}"))
}

/// Token 绑定视图：[`HttpBangumiClient::bind`] 的返回值，实现完整 [`BangumiClient`]。
/// Token 以引用持有，不复制、不持久化。
pub struct TokenBoundBangumiClient<'a> {
    client: &'a HttpBangumiClient,
    token: &'a str,
}

impl BangumiClient for TokenBoundBangumiClient<'_> {
    async fn get_calendar(&self) -> Result<Vec<BangumiCalendarDay>, BangumiApiError> {
        self.client.get_calendar().await
    }

    async fn get_season_subjects(
        &self,
        year: u32,
        month: u32,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiSubject>, BangumiApiError> {
        self.client
            .get_season_subjects(year, month, limit, offset)
            .await
    }

    async fn get_subject_detail(&self, subject_id: i64) -> Result<BangumiSubject, BangumiApiError> {
        self.client.get_subject_detail(subject_id).await
    }

    async fn get_subject_episodes(
        &self,
        subject_id: i64,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiEpisode>, BangumiApiError> {
        self.client
            .get_subject_episodes(subject_id, limit, offset)
            .await
    }

    async fn get_subject_characters(
        &self,
        subject_id: i64,
    ) -> Result<Vec<BangumiCharacter>, BangumiApiError> {
        self.client.get_subject_characters(subject_id).await
    }

    async fn get_subject_related(
        &self,
        subject_id: i64,
    ) -> Result<Vec<BangumiRelatedSubject>, BangumiApiError> {
        self.client.get_subject_related(subject_id).await
    }

    async fn get_user_profile(&self) -> Result<BangumiUserProfile, BangumiApiError> {
        self.client.get_user_profile(self.token).await
    }

    async fn get_user_collections(
        &self,
        username: &str,
        subject_type: u32,
        limit: u32,
        offset: u32,
    ) -> Result<Paged<BangumiCollection>, BangumiApiError> {
        self.client
            .get_user_collections(self.token, username, subject_type, limit, offset)
            .await
    }

    async fn get_user_collection(
        &self,
        username: &str,
        subject_id: i64,
    ) -> Result<BangumiCollection, BangumiApiError> {
        self.client
            .get_user_collection(self.token, username, subject_id)
            .await
    }

    async fn update_collection(
        &self,
        subject_id: i64,
        payload: &Value,
    ) -> Result<(), BangumiApiError> {
        // 绑定视图默认 PATCH（更新语义）；Phase 3 由业务层先查记录再决定
        // POST（无记录）/ PATCH，通过 HttpBangumiClient::update_collection 的
        // create 参数表达。
        self.client
            .update_collection(self.token, subject_id, payload, false)
            .await
    }

    async fn update_episode_progress(
        &self,
        episode_id: i64,
        collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        self.client
            .update_episode_progress(self.token, episode_id, collection_type)
            .await
    }

    async fn update_episode_progress_batch(
        &self,
        subject_id: i64,
        episode_ids: &[i64],
        collection_type: EpisodeCollectionType,
    ) -> Result<(), BangumiApiError> {
        self.client
            .update_episode_progress_batch(self.token, subject_id, episode_ids, collection_type)
            .await
    }

    async fn test_connection(&self) -> Result<BangumiUserProfile, BangumiApiError> {
        self.client.get_user_profile(self.token).await
    }
}

/// [`BangumiUserProfile`] → 前端 camelCase JSON（命令 `bangumi_get_user_profile`）。
pub fn bangumi_profile_json(profile: &BangumiUserProfile) -> Value {
    serde_json::json!({
        "id": profile.id,
        "username": profile.username,
        "nickname": profile.nickname,
        "avatar": profile.avatar.as_ref().map(|avatar| {
            serde_json::json!({
                "large": avatar.large,
                "medium": avatar.medium,
                "small": avatar.small,
            })
        }),
        "sign": profile.sign,
        "userGroup": profile.user_group,
    })
}

/// [`BangumiCollection`] → 前端 camelCase JSON（命令 `bangumi_get_user_collections`）。
/// 注意 `type`（收藏状态枚举）键名保持 `type` 不变。
pub fn bangumi_collection_json(collection: &BangumiCollection) -> Value {
    serde_json::json!({
        "subjectId": collection.subject_id,
        "subjectType": collection.subject_type,
        "rate": collection.rate,
        "type": collection.collection_type,
        "tags": collection.tags,
        "epStatus": collection.ep_status,
        "volStatus": collection.vol_status,
        "updatedAt": collection.updated_at,
        "private": collection.private,
        "comment": collection.comment,
    })
}

// ---------------------------------------------------------------------------
// O. Phase 2 映射引擎（AniList → Bangumi 主键解析；schema §4）。
//
// 优先级：已有手动映射 > 离线映射表（anilistIndex / bySubject.a 直查）>
// 标题+年份+季节综合评分（复用 offline_bangumi_match 的评分模式）> 无法判定。
// 强制规则（schema §4 冻结）：
// - 评分差距 < MAPPING_AUTO_SCORE_GAP 的多候选一律 Candidates，不自动绑定；
// - 电影 / OVA / 特别篇 format 一律不自动绑定（走用户确认）；
// - 仅标题相同（无日期佐证）永远拿不到 high。
// ---------------------------------------------------------------------------

/// 单个映射候选（`bangumi_resolve_mapping` 命令 candidates 投影）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingCandidate {
    pub subject_id: i64,
    pub name: String,
    pub name_cn: String,
    pub date: String,
    pub platform: Option<String>,
    pub begin: Option<String>,
    pub score: i64,
}

/// 解析结果：确定绑定 / 多候选待确认 / 无法判定。
#[derive(Debug, Clone, PartialEq)]
pub enum MappingResolution {
    /// 可确定唯一 subject。`method` 只有 Local（离线表直查）与
    /// TitleYear（评分裁决）两种；manual/external 映射由调用层负责。
    Mapped {
        subject_id: i64,
        confidence: MappingConfidence,
        method: MappingMethod,
    },
    /// 存在多个候选或低分：给出候选列表，由用户确认。
    Candidates(Vec<MappingCandidate>),
    /// 无法判定（无任何候选）。
    None,
}

/// 自动绑定要求第一名领先第二名至少该分差（schema §4：多候选不强配）。
pub const MAPPING_AUTO_SCORE_GAP: i64 = 8;
/// 评分达到该值（标题精确 + 日期精确量级）才允许 high 自动绑定。
pub const MAPPING_HIGH_SCORE: i64 = 215;
/// 评分达到该值（标题精确+年份、部分标题+日期等量级）允许 medium 自动绑定。
pub const MAPPING_MEDIUM_SCORE: i64 = 130;
/// 候选列表的展示下限：仅展示有标题信号（或更强）的候选，
/// 避免纯 format 弱分噪声进入确认 UI。
pub const MAPPING_CANDIDATE_MIN_SCORE: i64 = 90;
/// 单个候选的评分权重（与 offline_bangumi_match 保持同一量纲）。
const MAPPING_TITLE_EXACT: i64 = 100;
const MAPPING_TITLE_PARTIAL: i64 = 55;
const MAPPING_DATE_EXACT: i64 = 120;
const MAPPING_YEAR_MATCH: i64 = 30;
const MAPPING_FORMAT_MATCH: i64 = 10;

/// 电影 / OVA / 特别篇不自动绑定（schema §4：续作、特别篇必须允许用户确认）。
fn mapping_format_blocks_auto(anime_format: &str) -> bool {
    matches!(anime_format, "MOVIE" | "OVA" | "SPECIAL")
}

fn mapping_score_subject(anime: &Value, subject: &Value) -> i64 {
    let anime_keys: Vec<String> = ["native", "romaji", "english"]
        .iter()
        .filter_map(|key| anime["title"].get(*key).and_then(Value::as_str))
        .map(crate::normalize_title_key)
        .filter(|key| !key.is_empty())
        .collect();
    let subject_key = crate::normalize_title_key(&crate::value_string(subject.get("t")));
    let subject_date = crate::value_string(subject.get("d"));
    let start_date = crate::anime_start_date(anime);
    let start_year = start_date.get(0..4).unwrap_or_default().to_string();
    let anime_format = crate::value_string(anime.get("format"));
    let mut score = 0;
    if !subject_key.is_empty() {
        if anime_keys.iter().any(|key| key == &subject_key) {
            score += MAPPING_TITLE_EXACT;
        } else if anime_keys
            .iter()
            .any(|key| key.contains(&subject_key) || subject_key.contains(key))
        {
            score += MAPPING_TITLE_PARTIAL;
        }
    }
    if !start_date.is_empty() && subject_date == start_date {
        score += MAPPING_DATE_EXACT;
    } else if !start_year.is_empty() && subject_date.starts_with(&start_year) {
        score += MAPPING_YEAR_MATCH;
    }
    if crate::offline_format_matches(&anime_format, &crate::value_string(subject.get("f"))) {
        score += MAPPING_FORMAT_MATCH;
    }
    score
}

fn mapping_candidate_from(subject: &Value, score: i64) -> MappingCandidate {
    let platform = subject["sites"]
        .as_array()
        .and_then(|sites| sites.first())
        .and_then(|site| site.get("s"))
        .and_then(Value::as_str)
        .map(str::to_string);
    MappingCandidate {
        subject_id: crate::value_i64(subject.get("b")),
        name: crate::value_string(subject.get("t")),
        name_cn: crate::value_string(subject.get("c")),
        date: crate::value_string(subject.get("d")),
        platform,
        begin: subject
            .get("begin")
            .and_then(Value::as_str)
            .map(str::to_string),
        score,
    }
}

/// 评分裁决：对候选池打分排序，按阈值/分差/format 规则决定 Mapped 或 Candidates。
fn mapping_decide_by_score(
    anime: &Value,
    pool: &[Value],
    method: MappingMethod,
) -> MappingResolution {
    if pool.is_empty() {
        return MappingResolution::None;
    }
    let mut ranked: Vec<(i64, &Value)> = pool
        .iter()
        .map(|subject| (mapping_score_subject(anime, subject), subject))
        .collect();
    ranked.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| {
            crate::value_i64(left.1.get("b")).cmp(&crate::value_i64(right.1.get("b")))
        })
    });
    let candidates: Vec<MappingCandidate> = ranked
        .iter()
        .filter(|(score, _)| *score >= MAPPING_CANDIDATE_MIN_SCORE)
        .take(5)
        .map(|(score, subject)| mapping_candidate_from(subject, *score))
        .collect();
    // 电影 / OVA / 特别篇一律不自动绑定（schema §4）。
    if mapping_format_blocks_auto(&crate::value_string(anime.get("format"))) {
        return if candidates.is_empty() {
            MappingResolution::None
        } else {
            MappingResolution::Candidates(candidates)
        };
    }
    let Some((top_score, top)) = ranked.first() else {
        return MappingResolution::None;
    };
    if ranked.len() > 1 && top_score - ranked[1].0 < MAPPING_AUTO_SCORE_GAP {
        return if candidates.is_empty() {
            MappingResolution::None
        } else {
            MappingResolution::Candidates(candidates)
        };
    }
    if *top_score < MAPPING_MEDIUM_SCORE {
        return if candidates.is_empty() {
            MappingResolution::None
        } else {
            MappingResolution::Candidates(candidates)
        };
    }
    MappingResolution::Mapped {
        subject_id: crate::value_i64(top.get("b")),
        confidence: if *top_score >= MAPPING_HIGH_SCORE {
            MappingConfidence::High
        } else {
            MappingConfidence::Medium
        },
        method,
    }
}

/// AniList 追番条目 → Bangumi subject 解析（schema §4 优先级 1-4）。
///
/// `entry` 为 following 条目（`id` 为 AniList id 或已重键的 subjectId，
/// 携带 `title`/`startDate`/`format`/`mapping`/`bangumiId`）。已有手动映射
/// 直接返回 Mapped（不会在自动路径中触发——调用方对已有 mapping 的条目跳过）。
pub fn resolve_mapping_candidates(map: &Value, entry: &Value) -> MappingResolution {
    let anime_id = crate::value_i64(entry.get("id"));
    if anime_id <= 0 {
        return MappingResolution::None;
    }
    // 优先级 1：已有手动映射（防御性；自动路径不会对已有 mapping 的条目调用）。
    let mapping = entry.get("mapping").filter(|value| !value.is_null());
    if crate::value_string(mapping.and_then(|value| value.get("method"))) == "manual" {
        let subject_id = crate::value_i64(entry.get("bangumiId"));
        if subject_id > 0 {
            return MappingResolution::Mapped {
                subject_id,
                confidence: MappingConfidence::High,
                method: MappingMethod::Local,
            };
        }
    }
    let by_subject = map.get("bySubject").and_then(Value::as_object);
    // 优先级 2：离线映射表直查（bySubject.a 精确关联）。
    let linked: Vec<Value> = by_subject
        .map(|subjects| {
            subjects
                .values()
                .filter(|subject| crate::value_i64(subject.get("a")) == anime_id)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if linked.len() == 1 {
        return MappingResolution::Mapped {
            subject_id: crate::value_i64(linked[0].get("b")),
            confidence: MappingConfidence::High,
            method: MappingMethod::Local,
        };
    }
    if linked.len() > 1 {
        // 同一 AniList id 关联多个 subject：仍走评分裁决消除歧义。
        return mapping_decide_by_score(entry, &linked, MappingMethod::Local);
    }
    // anilistIndex 代表条目兜底（build.rs 精选，视为权威）。
    if let Some(subject_id) = map
        .get("anilistIndex")
        .and_then(|index| index.get(anime_id.to_string()))
        .and_then(Value::as_i64)
    {
        if let Some(subject) = by_subject.and_then(|subjects| subjects.get(&subject_id.to_string()))
        {
            return MappingResolution::Mapped {
                subject_id: crate::value_i64(subject.get("b")),
                confidence: MappingConfidence::High,
                method: MappingMethod::Local,
            };
        }
    }
    // 优先级 3/4：标题+年份+季节综合评分（跳过已绑定其它 AniList id 的条目）。
    let pool: Vec<Value> = by_subject
        .map(|subjects| {
            subjects
                .values()
                .filter(|subject| {
                    let linked_id = crate::value_i64(subject.get("a"));
                    linked_id == 0 || linked_id == anime_id
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    mapping_decide_by_score(entry, &pool, MappingMethod::TitleYear)
}

// ---------------------------------------------------------------------------
// 测试（cfg(test)）；整个模块仅在 standard feature 下编译，
// 因此这些测试只进 standard 门禁。
// ---------------------------------------------------------------------------

/// 本地 mock HTTP server（std TcpListener + 手写最小响应，不引新依赖）。
/// 供本模块与 lib.rs 的 Phase 1 命令测试共用；每次连接一个线程，
/// handler 为纯同步闭包 `(method, target, headers, body) -> (status, headers, body)`。
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// 一次被捕获的请求（header 名为小写）。
    #[derive(Debug, Clone)]
    pub struct RequestRecord {
        pub method: String,
        pub target: String,
        pub headers: HashMap<String, String>,
        /// 当前断言只用 method/target/headers；body 保留给写请求断言
        /// （Phase 3 收藏/进度写回），避免届时改 mock 捕获层。
        #[allow(dead_code)]
        pub body: String,
    }

    pub type MockHandler = Arc<
        dyn Fn(&str, &str, &HashMap<String, String>, &str) -> (u16, Vec<(String, String)>, String)
            + Send
            + Sync,
    >;

    pub struct MockBangumiServer {
        addr: SocketAddr,
        requests: Arc<Mutex<Vec<RequestRecord>>>,
    }

    impl MockBangumiServer {
        pub fn spawn(handler: MockHandler) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let addr = listener.local_addr().expect("mock server addr");
            let requests: Arc<Mutex<Vec<RequestRecord>>> = Arc::new(Mutex::new(Vec::new()));
            let captured = requests.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let handler = handler.clone();
                    let captured = captured.clone();
                    thread::spawn(move || {
                        handle_connection(stream, &handler, &captured);
                    });
                }
            });
            Self { addr, requests }
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// 端口号（测试用临时目录名等场景）。
        pub fn port(&self) -> u16 {
            self.addr.port()
        }

        pub fn requests(&self) -> Vec<RequestRecord> {
            self.requests
                .lock()
                .expect("mock server request log")
                .clone()
        }
    }

    fn handle_connection(
        stream: std::net::TcpStream,
        handler: &MockHandler,
        captured: &Mutex<Vec<RequestRecord>>,
    ) {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(reader) => reader,
            Err(_) => return,
        });
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        if length > 0 {
            reader.read_exact(&mut body).ok();
        }
        let body = String::from_utf8_lossy(&body).to_string();
        captured
            .lock()
            .expect("mock server request log")
            .push(RequestRecord {
                method: method.clone(),
                target: target.clone(),
                headers: headers.clone(),
                body: body.clone(),
            });
        let (status, extra_headers, response_body) = handler(&method, &target, &headers, &body);
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            401 => "Unauthorized",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            _ => "Response",
        };
        let mut response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            response_body.len()
        );
        for (name, value) in extra_headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(&response_body);
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }
    use serde_json::json;

    /// 无 tokio rt/macros 依赖的极简 block_on：本模块的 future 均为立即就绪，
    /// 用 noop waker 自旋即可。
    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn fixture_value(name: &str) -> Value {
        let raw = match name {
            "old-state-v2" => include_str!("../fixtures/bangumi/old-state-v2.json"),
            "state-with-bangumi" => include_str!("../fixtures/bangumi/state-with-bangumi.json"),
            _ => panic!("unknown fixture {name}"),
        };
        serde_json::from_str(raw).expect("state fixture must parse")
    }

    // -- 1. serde round-trip -------------------------------------------------

    #[test]
    fn bangumi_sync_settings_round_trip_with_defaults() {
        let settings = BangumiSyncSettings::default();
        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value["syncEnabled"], false);
        assert_eq!(value["conflictPolicy"], "latest");
        let restored: BangumiSyncSettings = serde_json::from_value(value).unwrap();
        assert_eq!(restored, settings);
        // 缺字段也应通过 serde default 重建默认值（前向兼容）。
        let partial: BangumiSyncSettings =
            serde_json::from_value(json!({"syncEnabled": true})).unwrap();
        assert!(partial.sync_enabled);
        assert_eq!(partial.conflict_policy, ConflictPolicy::Latest);
        assert_eq!(
            partial.preferred_broadcast_sites,
            default_preferred_broadcast_sites()
        );
    }

    #[test]
    fn bangumi_subject_record_round_trip() {
        let record = BangumiSubjectRecord {
            subject_id: 45678,
            title: "Re:从零开始的异世界生活 第3章".into(),
            title_original: Some("リ:ゼロから始める異世界生活 第3章".into()),
            title_romaji: Some("Re:Zero 3rd Season".into()),
            cover_image: "https://lain.bgm.tv/pic/cover/c/00/00/45678_pL3cR.jpg".into(),
            format: Some("TV".into()),
            episodes: Some(16),
            airing: Some(BangumiAiring {
                next_episode: Some(4),
                next_airing_at: Some(1_785_357_622),
            }),
            bangumi_status: Some("doing".into()),
            rating: Some(8.2),
            watched_episode: Some(3),
            anilist_id: Some(21355),
            mapping: Some(BangumiMapping {
                method: MappingMethod::External,
                confidence: MappingConfidence::High,
                updated_at: 1_785_300_000,
            }),
            mapping_pending: false,
            last_pulled_from_bangumi_at: Some(1_785_300_000),
            last_pushed_to_bangumi_at: None,
            last_pulled_payload_hash: Some("abc123".into()),
            last_pushed_payload_hash: None,
            last_changed_by: Some(LastChangedBy::Bangumi),
            sync_updated_at: Some(1_785_300_000_000),
            ..Default::default()
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["subjectId"], 45678);
        assert_eq!(value["mapping"]["method"], "external");
        assert_eq!(value["mapping"]["confidence"], "high");
        assert_eq!(value["lastChangedBy"], "bangumi");
        assert_eq!(value["source"], "bangumi");
        assert!(value["mappingPending"] == false);
        let restored: BangumiSubjectRecord = serde_json::from_value(value).unwrap();
        assert_eq!(restored, record);
    }

    #[test]
    fn bangumi_episode_record_round_trip() {
        let record = BangumiEpisodeRecord {
            id: "bgm-episode-98765".into(),
            subject_id: Some(45678),
            episode_id: Some(98765),
            episode_number: Some(4.0),
            episode_sort_key: "0004".into(),
            episode_type: EpisodeType::Regular,
            title: Some("第4话".into()),
            status: "completed".into(),
            completed_at: Some(1_785_300_000),
            created_at: Some(1_785_200_000),
            anime_id: Some(21355),
            airing_at: Some(1_785_357_622),
            sync_updated_at: Some(1_785_300_000_000),
            last_changed_by: Some(LastChangedBy::Local),
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["episodeType"], "regular");
        assert_eq!(value["episodeSortKey"], "0004");
        let restored: BangumiEpisodeRecord = serde_json::from_value(value).unwrap();
        assert_eq!(restored, record);
        // 迁移中间态：subjectId 为 null 仍可解析。
        let intermediate: BangumiEpisodeRecord =
            serde_json::from_value(json!({"id": "21355-4", "episodeSortKey": "0004"})).unwrap();
        assert_eq!(intermediate.subject_id, None);
        assert_eq!(intermediate.episode_type, EpisodeType::Unknown);
        assert_eq!(intermediate.status, "pending");
    }

    #[test]
    fn bangumi_sync_status_and_user_summary_round_trip() {
        let status = BangumiSyncStatus {
            last_full_sync_at: Some(1_785_300_000),
            last_web_dav_sync_at: None,
            last_bangumi_sync_at: Some(1_785_300_000),
            last_schedule_sync_at: None,
            last_sync_error: Some("429 rate limited".into()),
        };
        let value = serde_json::to_value(&status).unwrap();
        let restored: BangumiSyncStatus = serde_json::from_value(value).unwrap();
        assert_eq!(restored, status);

        let user = BangumiUserSummary {
            id: 876543,
            username: "anilog_dev".into(),
            nickname: "阿罗".into(),
            avatar_url: Some("https://lain.bgm.tv/pic/user/m/000/87/65/876543.jpg".into()),
        };
        let value = serde_json::to_value(&user).unwrap();
        assert_eq!(
            value["avatarUrl"],
            "https://lain.bgm.tv/pic/user/m/000/87/65/876543.jpg"
        );
        let restored: BangumiUserSummary = serde_json::from_value(value).unwrap();
        assert_eq!(restored, user);
    }

    // -- 2. 旧 v2 状态 merge_defaults 补 bangumi 块 ---------------------------

    #[test]
    fn merge_defaults_fills_default_bangumi_block_for_legacy_v2_state() {
        let loaded = fixture_value("old-state-v2");
        assert!(loaded.get("bangumi").is_none());

        let following_before = loaded["following"].clone();
        let tasks_before = loaded["tasks"].clone();

        let merged = crate::merge_defaults(loaded, false);

        let expected = serde_json::to_value(BangumiSyncSettings::default()).unwrap();
        assert_eq!(merged["bangumi"], expected);
        // Phase 2（STATE_VERSION 3）：standard 版为记录补 additive 默认键，
        // 既有业务字段与 id 原样保留。
        assert_eq!(merged["following"][0]["id"], following_before[0]["id"]);
        assert_eq!(
            merged["following"][0]["displayTitle"],
            following_before[0]["displayTitle"]
        );
        assert_eq!(
            merged["following"][0]["followedAt"],
            following_before[0]["followedAt"]
        );
        assert_eq!(merged["following"][0]["source"], "anilist");
        assert_eq!(merged["following"][0]["mappingPending"], false);
        assert_eq!(merged["tasks"][0]["id"], tasks_before[0]["id"]);
        assert_eq!(merged["tasks"][0]["episodeType"], "regular");
        assert_eq!(merged["tasks"][0]["episodeSortKey"], "1");
        assert_eq!(merged["version"], crate::STATE_VERSION);
        assert_eq!(merged["settings"]["uiLanguage"], "zh-CN");
    }

    #[test]
    fn merge_defaults_never_fills_bangumi_block_for_original() {
        let loaded = fixture_value("old-state-v2");
        let merged = crate::merge_defaults(loaded, true);
        assert!(merged.get("bangumi").is_none());
    }

    #[test]
    fn merge_defaults_keeps_existing_bangumi_block() {
        let mut loaded = fixture_value("state-with-bangumi");
        loaded["bangumi"]["apiBaseUrl"] = json!("https://proxy.example.com/v0");
        loaded["bangumi"]["conflictPolicy"] = json!("local-first");
        let merged = crate::merge_defaults(loaded, false);
        assert_eq!(
            merged["bangumi"]["apiBaseUrl"],
            "https://proxy.example.com/v0"
        );
        assert_eq!(merged["bangumi"]["conflictPolicy"], "local-first");
    }

    // -- 3. 双基址 ------------------------------------------------------------

    #[test]
    fn resolve_base_urls_official_and_proxy() {
        // 空/空白 → 官方。
        for configured in ["", "   "] {
            let base = resolve_base_urls(configured);
            assert_eq!(base.root, "https://api.bgm.tv");
            assert_eq!(base.v0, "https://api.bgm.tv/v0");
        }
        // 带 /v0 后缀（含末尾斜杠）→ root 剥离，v0 原串。
        let base = resolve_base_urls("https://sh1n.cc.cd/v0");
        assert_eq!(base.root, "https://sh1n.cc.cd");
        assert_eq!(base.v0, "https://sh1n.cc.cd/v0");
        let base = resolve_base_urls("https://sh1n.cc.cd/v0/");
        assert_eq!(base.root, "https://sh1n.cc.cd");
        assert_eq!(base.v0, "https://sh1n.cc.cd/v0");
        // 无 /v0 后缀 → root 原串，v0 追加。
        let base = resolve_base_urls("https://proxy.example.com/bgm");
        assert_eq!(base.root, "https://proxy.example.com/bgm");
        assert_eq!(base.v0, "https://proxy.example.com/bgm/v0");
        // 非 https 配置回退官方（绝不降级为 http）。
        let base = resolve_base_urls("http://insecure.example.com/v0");
        assert_eq!(base.root, "https://api.bgm.tv");
        assert_eq!(base.v0, "https://api.bgm.tv/v0");
    }

    // -- 4. 错误模型 -----------------------------------------------------------

    #[test]
    fn from_status_429_parses_retry_after() {
        let error = from_status(
            429,
            include_str!("../fixtures/bangumi/error-429.json"),
            Some("120"),
        );
        match error {
            BangumiApiError::RateLimited {
                retry_after,
                message,
            } => {
                assert_eq!(retry_after, Some(StdDuration::from_secs(120)));
                assert!(message.contains("Too Many Requests"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn from_status_maps_statuses_and_error_body() {
        let error = from_status(
            401,
            include_str!("../fixtures/bangumi/error-401.json"),
            None,
        );
        assert!(matches!(error, BangumiApiError::Unauthorized { .. }));
        assert!(error.to_string().contains("Unauthorized"));

        assert!(matches!(
            from_status(403, "{}", None),
            BangumiApiError::Forbidden { .. }
        ));
        assert!(matches!(
            from_status(404, "", None),
            BangumiApiError::NotFound { .. }
        ));
        assert!(matches!(
            from_status(409, "", None),
            BangumiApiError::Conflict { .. }
        ));
        assert_eq!(
            from_status(503, "", None),
            BangumiApiError::ServerError(503)
        );
        assert_eq!(
            from_status(400, "", None),
            BangumiApiError::ServerError(400)
        );
    }

    #[test]
    fn error_display_never_contains_token_material() {
        for status in [401u16, 403, 404, 409, 429, 500] {
            let error = from_status(
                status,
                r#"{"title":"Error","description":"请求被拒绝"}"#,
                (status == 429).then_some("30"),
            );
            let display = error.to_string();
            assert!(
                !display.contains("Bearer"),
                "display leaks Bearer: {display}"
            );
            assert!(!display.contains("token"), "display leaks token: {display}");
            assert!(!display.contains("Authorization"));
        }
        // 非 JSON 错误体走截断回退，也绝不附加任何凭据材料。
        let error = from_status(502, "<html>Bad Gateway</html>", None);
        assert!(matches!(error, BangumiApiError::ServerError(502)));
    }

    #[test]
    fn parse_retry_after_supports_seconds_only() {
        assert_eq!(
            parse_retry_after(Some(" 120 ")),
            Some(StdDuration::from_secs(120))
        );
        assert_eq!(
            parse_retry_after(Some("0")),
            Some(StdDuration::from_secs(0))
        );
        // HTTP-date 形式 Phase 0 简化为 None（见函数注释）。
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(parse_retry_after(Some("soon")), None);
        assert_eq!(parse_retry_after(None), None);
    }

    // -- 5. broadcast golden 向量 ----------------------------------------------

    #[test]
    fn broadcast_golden_vectors_pass() {
        let raw = include_str!("../fixtures/bangumi/broadcast-vectors.json");
        let vectors: Value = serde_json::from_str(raw).expect("broadcast vectors fixture");
        for vector in vectors["vectors"].as_array().expect("vectors array") {
            let name = vector["name"].as_str().unwrap_or_default();
            let after_raw = vector["nowLocalISO"].as_str().expect("nowLocalISO");
            let after = DateTime::parse_from_rfc3339(after_raw).expect("parse nowLocalISO");
            let preferred: Vec<String> = vector["preferredSites"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let sites: Vec<BroadcastSite<'_>> = vector["sites"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|site| BroadcastSite {
                            site: site["site"].as_str().unwrap_or_default(),
                            begin: site["begin"].as_str(),
                            broadcast: site["broadcast"].as_str(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let expected = vector["expectedNextLocal"]
                .as_str()
                .expect("expectedNextLocal");

            let next = next_broadcast_after(
                vector["begin"].as_str(),
                vector["broadcast"].as_str(),
                &sites,
                &preferred,
                after,
            )
            .unwrap_or_else(|| panic!("vector {name}: expected a next broadcast"));

            // 结果须以 nowLocalISO 同样的墙钟时区偏移表达。
            let rendered = next.with_timezone(after.offset());
            let actual = rendered.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
            assert_eq!(actual, expected, "vector {name}: broadcast mismatch");
        }
    }

    #[test]
    fn next_broadcast_falls_back_between_sites_and_begin() {
        let preferred = vec!["bangumi".to_string(), "ani_one".to_string()];
        let sites = [
            BroadcastSite {
                site: "bangumi",
                begin: None,
                broadcast: None,
            },
            BroadcastSite {
                site: "ani_one",
                begin: Some("2026-07-08T21:00:00+08:00"),
                broadcast: Some("R/2026-07-08T21:00:00.000+08:00/P7D"),
            },
        ];
        let after = DateTime::parse_from_rfc3339("2026-07-19T00:00:00+08:00").unwrap();
        let next = next_broadcast_after(None, None, &sites, &preferred, after).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-07-22T13:00:00+00:00");

        // 全无时间源 → None。
        let empty: &[BroadcastSite<'_>] = &[];
        assert!(next_broadcast_after(None, None, empty, &preferred, after).is_none());

        // begin-only：未来的一次性播出。
        let next = next_broadcast_after(
            Some("2026-09-12T10:30:00Z"),
            None,
            empty,
            &[],
            DateTime::parse_from_rfc3339("2026-09-01T00:00:00+08:00").unwrap(),
        );
        assert_eq!(next.unwrap().to_rfc3339(), "2026-09-12T10:30:00+00:00");
        // begin-only：已过期 → None。
        assert!(
            next_broadcast_after(
                Some("2026-09-12T10:30:00Z"),
                None,
                empty,
                &[],
                DateTime::parse_from_rfc3339("2026-09-13T00:00:00+08:00").unwrap(),
            )
            .is_none()
        );

        // 周期支持 W（两周）。
        let next = next_broadcast_after(
            Some("2026-07-01T00:00:00Z"),
            Some("R/2026-07-01T00:00:00Z/P2W"),
            empty,
            &[],
            DateTime::parse_from_rfc3339("2026-07-02T00:00:00Z").unwrap(),
        );
        assert_eq!(next.unwrap().to_rfc3339(), "2026-07-15T00:00:00+00:00");
    }

    // -- 6. season_months -------------------------------------------------------

    #[test]
    fn season_to_month_mapping() {
        assert_eq!(season_months("WINTER"), [1, 2, 3]);
        assert_eq!(season_months("SPRING"), [4, 5, 6]);
        assert_eq!(season_months("SUMMER"), [7, 8, 9]);
        assert_eq!(season_months("FALL"), [10, 11, 12]);
        // 大小写/空白宽容；未知季节返回 0 占位。
        assert_eq!(season_months("winter"), [1, 2, 3]);
        assert_eq!(season_months(" FALL "), [10, 11, 12]);
        assert_eq!(season_months("unknown"), [0, 0, 0]);
    }

    // -- 7. FixtureBangumiClient 形状断言 ---------------------------------------

    #[test]
    fn fixture_client_shapes_match_contract() {
        let client = FixtureBangumiClient::new();

        // calendar：2 个星期分组。
        let calendar = block_on(client.get_calendar()).unwrap();
        assert_eq!(calendar.len(), 2);
        assert_eq!(calendar[0].weekday.en.as_deref(), Some("Mon"));
        assert_eq!(calendar[0].items.len(), 2);
        assert_eq!(calendar[0].items[0].id, 45678);

        // 季度分页：2 条。
        let season = block_on(client.get_season_subjects(2026, 7, 50, 0)).unwrap();
        assert_eq!(season.total, 2);
        assert_eq!(season.data.len(), 2);
        assert_eq!(season.data[0].id, 45678);
        assert_eq!(season.data[0].eps, Some(16));
        assert_eq!(
            season.data[0].rating.as_ref().and_then(|r| r.score),
            Some(8.2)
        );

        // 集数：3 条，type 0/1/0。
        let episodes = block_on(client.get_subject_episodes(45678, 100, 0)).unwrap();
        assert_eq!(episodes.data.len(), 3);
        assert_eq!(episodes.data[0].ep_type, EpType::Main.as_u32());
        assert_eq!(episodes.data[1].ep_type, EpType::Sp.as_u32());
        assert_eq!(episodes.data[0].sort, Some(4.0));

        // 收藏：2 条；type 语义 3=Doing / 5=Dropped。
        let collections = block_on(client.get_user_collections("anilog_dev", 2, 30, 0)).unwrap();
        assert_eq!(collections.data.len(), 2);
        assert_eq!(
            collections.data[0].collection_type,
            SubjectCollectionType::Doing.as_u32()
        );
        assert_eq!(
            SubjectCollectionType::from_u32(collections.data[0].collection_type),
            Some(SubjectCollectionType::Doing)
        );
        assert_eq!(
            SubjectCollectionType::from_u32(collections.data[1].collection_type),
            Some(SubjectCollectionType::Dropped)
        );

        // 单条收藏 / 详情 / 角色 / 关联 / 用户。
        let collection = block_on(client.get_user_collection("anilog_dev", 45678)).unwrap();
        assert_eq!(collection.subject_id, 45678);
        assert_eq!(collection.ep_status, Some(3));
        let detail = block_on(client.get_subject_detail(45678)).unwrap();
        assert_eq!(detail.infobox.len(), 7);
        let characters = block_on(client.get_subject_characters(45678)).unwrap();
        assert_eq!(characters.len(), 2);
        assert_eq!(characters[0].relation, "主角");
        let related = block_on(client.get_subject_related(45678)).unwrap();
        assert_eq!(related.len(), 2);
        let profile = block_on(client.get_user_profile()).unwrap();
        assert_eq!(profile.username, "anilog_dev");
        assert_eq!(profile.user_group, Some(10));
        let connected = block_on(client.test_connection()).unwrap();
        assert_eq!(connected.id, profile.id);

        // 写方法 204 无响应体 → Ok(())。
        let payload = json!({"type": SubjectCollectionType::Doing.as_u32()});
        assert!(block_on(client.update_collection(45678, &payload)).is_ok());
        assert!(
            block_on(client.update_episode_progress(98765, EpisodeCollectionType::Watched)).is_ok()
        );
        assert!(
            block_on(client.update_episode_progress_batch(
                45678,
                &[98765, 98766],
                EpisodeCollectionType::Watched
            ))
            .is_ok()
        );
    }

    #[test]
    fn fixture_client_stub_error_matches_http_status() {
        let unauthorized = FixtureBangumiClient::stub_error(401);
        assert!(matches!(unauthorized, BangumiApiError::Unauthorized { .. }));
        let limited = FixtureBangumiClient::stub_error(429);
        match limited {
            BangumiApiError::RateLimited { retry_after, .. } => {
                assert_eq!(retry_after, Some(StdDuration::from_secs(120)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    // -- 8. BangumiSyncSettings 序列化键集合 ------------------------------------

    #[test]
    fn bangumi_sync_settings_serializes_exactly_eight_keys() {
        let value = serde_json::to_value(BangumiSyncSettings::default()).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("settings must serialize to an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "apiBaseUrl",
                "conflictPolicy",
                "preferredBroadcastSites",
                "pullCollections",
                "pullExternalStatus",
                "pushCompletedEpisodes",
                "pushLocalChanges",
                "syncEnabled"
            ]
        );
        // 默认值语义再断言一次（schema §3.1）。
        assert_eq!(value["apiBaseUrl"], "");
        assert_eq!(value["syncEnabled"], false);
        assert_eq!(value["pullCollections"], true);
        assert_eq!(value["pushLocalChanges"], false);
        assert_eq!(value["pushCompletedEpisodes"], false);
        assert_eq!(value["pullExternalStatus"], true);
        assert_eq!(
            value["preferredBroadcastSites"],
            json!(default_preferred_broadcast_sites())
        );
    }

    // -- 端点 URL / 枚举常量 ----------------------------------------------------

    #[test]
    fn endpoint_urls_follow_official_layout() {
        let base = BangumiBaseUrls {
            root: "https://api.bgm.tv".into(),
            v0: "https://api.bgm.tv/v0".into(),
        };
        assert_eq!(calendar_url(&base), "https://api.bgm.tv/calendar");
        assert_eq!(
            season_subjects_url(&base, 2026, 7, 50, 0),
            "https://api.bgm.tv/v0/subjects?type=2&year=2026&month=7&limit=50&offset=0"
        );
        // limit 上限 50。
        assert!(season_subjects_url(&base, 2026, 7, 500, 0).contains("limit=50"));
        assert_eq!(
            subject_detail_url(&base, 45678),
            "https://api.bgm.tv/v0/subjects/45678"
        );
        // 集数是 /v0/episodes，limit 上限 200。
        assert_eq!(
            subject_episodes_url(&base, 45678, 100, 0),
            "https://api.bgm.tv/v0/episodes?subject_id=45678&limit=100&offset=0"
        );
        assert!(subject_episodes_url(&base, 45678, 1000, 0).contains("limit=200"));
        assert_eq!(
            subject_characters_url(&base, 45678),
            "https://api.bgm.tv/v0/subjects/45678/characters"
        );
        assert_eq!(
            subject_related_url(&base, 45678),
            "https://api.bgm.tv/v0/subjects/45678/subjects"
        );
        assert_eq!(me_url(&base), "https://api.bgm.tv/v0/me");
        assert_eq!(
            user_collections_url(&base, "anilog_dev", 2, 30, 0),
            "https://api.bgm.tv/v0/users/anilog_dev/collections?subject_type=2&limit=30&offset=0"
        );
        assert_eq!(
            user_collection_url(&base, "anilog_dev", 45678),
            "https://api.bgm.tv/v0/users/anilog_dev/collections/45678"
        );
        // `-` 占位 = 当前 token 用户（官方 spec）。
        assert_eq!(
            update_collection_url(&base, 45678),
            "https://api.bgm.tv/v0/users/-/collections/45678"
        );
        assert_eq!(
            episode_progress_url(&base, 98765),
            "https://api.bgm.tv/v0/users/-/collections/-/episodes/98765"
        );
        assert_eq!(
            episode_progress_batch_url(&base, 45678),
            "https://api.bgm.tv/v0/users/-/collections/45678/episodes"
        );
    }

    #[test]
    fn collection_type_constants_match_official_semantics() {
        // 条目收藏：2 是 Done，不是 Doing。
        assert_eq!(SubjectCollectionType::Wish.as_u32(), 1);
        assert_eq!(SubjectCollectionType::Done.as_u32(), 2);
        assert_eq!(SubjectCollectionType::Doing.as_u32(), 3);
        assert_eq!(SubjectCollectionType::OnHold.as_u32(), 4);
        assert_eq!(SubjectCollectionType::Dropped.as_u32(), 5);
        assert_eq!(
            SubjectCollectionType::from_u32(2),
            Some(SubjectCollectionType::Done)
        );
        assert_eq!(SubjectCollectionType::from_u32(6), None);
        // 单集进度：0 未收藏 / 1 想看 / 2 看过 / 3 抛弃。
        assert_eq!(EpisodeCollectionType::NotCollected.as_u32(), 0);
        assert_eq!(EpisodeCollectionType::Watched.as_u32(), 2);
        assert_eq!(EpisodeCollectionType::Dropped.as_u32(), 3);
        // 集数类型：0 本篇 / 1 SP / 2 OP / 3 ED / 4 预告 / 5 MAD / 6 其他。
        assert_eq!(EpType::Main.as_u32(), 0);
        assert_eq!(EpType::Preview.as_u32(), 4);
        assert_eq!(EpType::Other.as_u32(), 6);
        assert_eq!(EpType::from_u32(7), None);
    }

    // -- Token 存储 -------------------------------------------------------------

    #[test]
    fn memory_token_store_round_trip_and_validation() {
        let store = MemoryTokenStore::new();
        assert_eq!(store.load().unwrap(), None);
        store.store("  secret-token  ").unwrap();
        assert_eq!(store.load().unwrap(), Some("secret-token".into()));
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        assert!(matches!(store.store("   "), Err(TokenStoreError::Other(_))));
    }

    #[test]
    fn unsupported_token_store_reports_platform_error() {
        let store = UnsupportedTokenStore;
        for error in [
            store.load().err().unwrap(),
            store.store("token").err().unwrap(),
            store.clear().err().unwrap(),
        ] {
            assert!(matches!(error, TokenStoreError::Platform(_)));
        }
    }

    // -- 9. HttpBangumiClient（Phase 1，本地 mock HTTP server）------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// HTTP 测试用 tokio current-thread runtime（reqwest 需要 tokio reactor）。
    fn http_block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(future)
    }

    fn mock_base(url: &str) -> BangumiBaseUrls {
        BangumiBaseUrls {
            root: url.to_string(),
            v0: format!("{url}/v0"),
        }
    }

    #[test]
    fn bearer_prefixes_token_without_trimming() {
        assert_eq!(bearer("abc"), "Bearer abc");
        assert_eq!(bearer(" a b "), "Bearer  a b ");
    }

    #[test]
    fn http_client_sends_bearer_header_and_parses_me_fixture() {
        let body = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |method, target, headers, _request_body| {
                assert_eq!(method, "GET");
                assert_eq!(target, "/v0/me");
                // Authorization 头必须被发送且形如 "Bearer xxx"。
                assert_eq!(
                    headers.get("authorization").map(String::as_str),
                    Some("Bearer roundtrip-token-abc")
                );
                // UA 与 lib.rs AniList client 一致。
                assert!(
                    headers
                        .get("user-agent")
                        .map(String::as_str)
                        .unwrap_or_default()
                        .starts_with("AniLog Tauri/")
                );
                (200, vec![], body.clone())
            },
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let profile = http_block_on(client.get_user_profile("roundtrip-token-abc")).unwrap();
        assert_eq!(profile.username, "anilog_dev");
        assert_eq!(profile.nickname, "阿罗");
        assert_eq!(profile.user_group, Some(10));
        // Token 不被客户端持久化：再次请求仍需显式传入。
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn http_client_maps_401_to_unauthorized() {
        let body = include_str!("../fixtures/bangumi/error-401.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, _request_body| (401, vec![], body.clone()),
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let error = http_block_on(client.get_user_profile("stale-token")).unwrap_err();
        assert!(matches!(error, BangumiApiError::Unauthorized { .. }));
        assert!(error.to_string().contains("Unauthorized"));
    }

    #[test]
    fn http_client_maps_429_with_retry_after_to_rate_limited() {
        let body = include_str!("../fixtures/bangumi/error-429.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, _request_body| {
                (
                    429,
                    vec![("Retry-After".to_string(), "120".to_string())],
                    body.clone(),
                )
            },
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let error = http_block_on(client.get_user_profile("throttled-token")).unwrap_err();
        match error {
            BangumiApiError::RateLimited { retry_after, .. } => {
                assert_eq!(retry_after, Some(StdDuration::from_secs(120)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn http_client_parses_collections_paged_envelope() {
        let body = include_str!("../fixtures/bangumi/user-collections-page.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, target, headers, _request_body| {
                assert!(target.starts_with("/v0/users/anilog_dev/collections?"));
                assert!(target.contains("subject_type=2"));
                assert!(
                    headers
                        .get("authorization")
                        .map(String::as_str)
                        .unwrap_or_default()
                        .starts_with("Bearer ")
                );
                (200, vec![], body.clone())
            },
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let page = http_block_on(client.get_user_collections(
            "collections-token",
            "anilog_dev",
            SUBJECT_TYPE_ANIME,
            30,
            0,
        ))
        .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.limit, 30);
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[0].subject_id, 45678);
        assert_eq!(
            page.data[0].collection_type,
            SubjectCollectionType::Doing.as_u32()
        );
        assert_eq!(
            page.data[1].collection_type,
            SubjectCollectionType::Dropped.as_u32()
        );
        // camelCase 前端投影。
        let item = bangumi_collection_json(&page.data[0]);
        assert_eq!(item["subjectId"], 45678);
        assert_eq!(item["type"], 3);
        assert_eq!(item["epStatus"], 3);
        assert_eq!(item["subjectType"], 2);
    }

    #[test]
    fn http_client_parses_me_profile_to_camel_case_json() {
        let body = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, _request_body| (200, vec![], body.clone()),
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let profile = http_block_on(client.get_user_profile("profile-token")).unwrap();
        let value = bangumi_profile_json(&profile);
        assert_eq!(value["id"], 876543);
        assert_eq!(value["username"], "anilog_dev");
        assert_eq!(value["nickname"], "阿罗");
        assert_eq!(value["userGroup"], 10);
        assert!(value["avatar"]["large"].is_string());
    }

    #[test]
    fn http_client_calendar_falls_back_from_proxy_to_official() {
        // 反代 /calendar 返回 500（ServerError）→ 回落官方一次成功。
        let proxy = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, target, _headers, _request_body| {
                assert_eq!(target, "/calendar");
                (500, vec![], "proxy boom".into())
            },
        ));
        let calendar = include_str!("../fixtures/bangumi/calendar.json").to_string();
        let official = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, target, _headers, _request_body| {
                assert_eq!(target, "/calendar");
                (200, vec![], calendar.clone())
            },
        ));
        let client = HttpBangumiClient::with_fallback(
            test_http_client(),
            mock_base(&proxy.url()),
            mock_base(&official.url()),
        );
        let days = http_block_on(client.get_calendar()).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(proxy.requests().len(), 1);
        assert_eq!(official.requests().len(), 1);

        // 4xx（404）不回落，直接透传。
        let proxy = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, _target, _headers, _request_body| (404, vec![], "{}".into()),
        ));
        let official = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, _target, _headers, _request_body| (200, vec![], "[]".into()),
        ));
        let client = HttpBangumiClient::with_fallback(
            test_http_client(),
            mock_base(&proxy.url()),
            mock_base(&official.url()),
        );
        let error = http_block_on(client.get_calendar()).unwrap_err();
        assert!(matches!(error, BangumiApiError::NotFound { .. }));
        assert_eq!(proxy.requests().len(), 1);
        assert_eq!(official.requests().len(), 0);

        // 两次都失败：优先返回反代（主路径）错误。
        let proxy = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, _target, _headers, _request_body| (502, vec![], "proxy down".into()),
        ));
        let official = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, _target, _headers, _request_body| (500, vec![], "official down".into()),
        ));
        let client = HttpBangumiClient::with_fallback(
            test_http_client(),
            mock_base(&proxy.url()),
            mock_base(&official.url()),
        );
        let error = http_block_on(client.get_calendar()).unwrap_err();
        assert_eq!(error, BangumiApiError::ServerError(502));
        assert_eq!(proxy.requests().len(), 1);
        assert_eq!(official.requests().len(), 1);

        // 官方基址（base == fallback）只请求一次，无回落。
        let official = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_method, _target, _headers, _request_body| (200, vec![], "[]".into()),
        ));
        let base = mock_base(&official.url());
        let client = HttpBangumiClient::with_fallback(test_http_client(), base.clone(), base);
        assert!(http_block_on(client.get_calendar()).is_ok());
        assert_eq!(official.requests().len(), 1);
    }

    #[test]
    fn http_client_write_paths_send_official_bodies() {
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |method, target, _headers, request_body| {
                let payload: Value = serde_json::from_str(request_body).expect("json body");
                match (method, target) {
                    ("POST", "/v0/users/-/collections/45678") => {
                        assert_eq!(payload["type"], 3);
                    }
                    ("PATCH", "/v0/users/-/collections/45678") => {
                        assert_eq!(payload["rate"], 8);
                    }
                    ("PUT", "/v0/users/-/collections/-/episodes/98765") => {
                        assert_eq!(payload["type"], 2);
                    }
                    ("PATCH", "/v0/users/-/collections/45678/episodes") => {
                        assert_eq!(payload["episode_id"], serde_json::json!([98765, 98766]));
                        assert_eq!(payload["type"], 2);
                    }
                    other => panic!("unexpected request {other:?}"),
                }
                (204, vec![], String::new())
            },
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        http_block_on(async {
            client
                .update_collection("write-token", 45678, &serde_json::json!({"type": 3}), true)
                .await
                .unwrap();
            client
                .update_collection("write-token", 45678, &serde_json::json!({"rate": 8}), false)
                .await
                .unwrap();
            client
                .update_episode_progress("write-token", 98765, EpisodeCollectionType::Watched)
                .await
                .unwrap();
            client
                .update_episode_progress_batch(
                    "write-token",
                    45678,
                    &[98765, 98766],
                    EpisodeCollectionType::Watched,
                )
                .await
                .unwrap();
        });
        assert_eq!(server.requests().len(), 4);
    }

    #[test]
    fn http_client_error_paths_never_leak_token_material() {
        const TOKEN: &str = "super-secret-bangumi-token-42";
        let assert_clean = |rendered: &str| {
            assert!(!rendered.contains("Bearer"), "leaks Bearer: {rendered}");
            assert!(!rendered.contains("token"), "leaks token: {rendered}");
            assert!(
                !rendered.contains("Authorization"),
                "leaks header: {rendered}"
            );
            assert!(!rendered.contains(TOKEN), "leaks token value: {rendered}");
        };

        // 401 错误路径。
        let body = include_str!("../fixtures/bangumi/error-401.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, _request_body| (401, vec![], body.clone()),
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let error = http_block_on(client.get_user_profile(TOKEN)).unwrap_err();
        assert_clean(&error.to_string());
        assert_clean(&format!("{error:?}"));

        // 429 错误路径（含 Retry-After）。
        let body = include_str!("../fixtures/bangumi/error-429.json").to_string();
        let server = test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, _request_body| {
                (
                    429,
                    vec![("Retry-After".to_string(), "30".to_string())],
                    body.clone(),
                )
            },
        ));
        let client = HttpBangumiClient::with_base(mock_base(&server.url())).unwrap();
        let error = http_block_on(client.test_connection(TOKEN)).unwrap_err();
        assert_clean(&error.to_string());
        assert_clean(&format!("{error:?}"));

        // 网络层错误路径（连接被拒绝端口）。
        let base = mock_base("http://127.0.0.1:9");
        let client = HttpBangumiClient::with_base(base).unwrap();
        let error = http_block_on(client.get_user_profile(TOKEN)).unwrap_err();
        assert!(matches!(error, BangumiApiError::Network(_)));
        assert_clean(&error.to_string());
        assert_clean(&format!("{error:?}"));
    }

    #[test]
    fn http_client_limits_global_concurrency_to_two() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let body = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let server = test_support::MockBangumiServer::spawn({
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            std::sync::Arc::new(move |_method, _target, _headers, _request_body| {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(StdDuration::from_millis(80));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                (200, vec![], body.clone())
            })
        });
        let client = Arc::new(HttpBangumiClient::with_base(mock_base(&server.url())).unwrap());
        http_block_on(async {
            let mut handles = Vec::new();
            for _ in 0..4 {
                let client = client.clone();
                handles.push(tokio::spawn(async move {
                    client.get_user_profile("concurrent-token").await
                }));
            }
            for handle in handles {
                handle.await.unwrap().unwrap();
            }
        });
        assert_eq!(server.requests().len(), 4);
        assert!(
            max_in_flight.load(Ordering::SeqCst) <= HTTP_CLIENT_CONCURRENCY,
            "global concurrency must stay within the semaphore limit"
        );
    }
}
