export type Season = 'WINTER' | 'SPRING' | 'SUMMER' | 'FALL';
export type ViewId = 'season' | 'tasks' | 'following' | 'settings';
export type SeasonViewMode = 'weekday' | 'all';
export type TitlePreference = 'auto' | 'english' | 'romaji' | 'native';
export type UiLanguage = 'zh-CN' | 'en-US';

export interface AnimeTitle {
  native?: string | null;
  romaji?: string | null;
  english?: string | null;
}

export interface Anime {
  id: number;
  title: AnimeTitle;
  coverImage?: { extraLarge?: string; medium?: string; color?: string | null };
  bannerImage?: string | null;
  description?: string | null;
  format?: string | null;
  episodes?: number | null;
  duration?: number | null;
  status?: string | null;
  season?: Season | null;
  seasonYear?: number | null;
  startDate?: { year?: number; month?: number; day?: number };
  studios?: { nodes: Array<{ name: string }> };
  genres?: string[];
  averageScore?: number | null;
  popularity?: number;
  nextAiringEpisode?: AiringEpisode | null;
  airingSchedule?: { nodes: AiringEpisode[] };
  siteUrl?: string;
  // Phase 2（Bangumi 主数据源迁移）：Bangumi 来源条目 id=subjectId、siteUrl=https://bgm.tv/subject/{id}、native 标题优先中文名。
  source?: 'anilist' | 'bangumi';
  bangumiSubjectId?: number | null;
  anilistId?: number | null;
}

export interface AiringEpisode {
  episode: number;
  airingAt: number;
  timeUntilAiring?: number;
  airingPrecision?: 'instant' | 'date' | 'unknown';
}

// Bangumi 收藏状态（SubjectCollectionType：wish/doing/done/on_hold/dropped）。
export type BangumiCollectionStatus = 'wish' | 'doing' | 'done' | 'on_hold' | 'dropped';

// finale-completed 事件 payload：最后一话看完时由后端发出（条目已自动转为“看过”）。
export interface BangumiFinaleCompletedPayload {
  subjectId: number;
  displayTitle: string;
}

export interface FollowedAnime {
  id: number;
  title: AnimeTitle;
  displayTitle: string;
  titleSource?: 'anilist' | 'bangumi' | 'custom';
  bangumiId?: number | null;
  coverImage: string;
  format?: string | null;
  episodes?: number | null;
  nextAiringEpisode?: AiringEpisode | null;
  siteUrl?: string;
  followedAt: number;
  syncUpdatedAt?: number;
  // Phase 2 主键迁移新增（可选，向后兼容）。
  source?: 'anilist' | 'bangumi';
  anilistId?: number | null;
  mapping?: BangumiMapping | null;
  mappingPending?: boolean;
  // Phase 3 收藏/评分/进度同步新增（可选，Bangumi 拉取合并后由 Rust 侧填充）。
  bangumiStatus?: BangumiCollectionStatus | null;
  rating?: number | null;
  watchedEpisode?: number | null;
}

export interface WatchTask {
  id: string;
  animeId: number;
  animeTitle: string;
  coverImage?: string;
  episode: number;
  airingAt: number;
  airingPrecision?: 'instant' | 'date' | 'unknown';
  status: 'pending' | 'completed';
  statusSource?: 'airing' | 'local' | 'bangumi';
  createdAt: number;
  completedAt: number | null;
  syncUpdatedAt?: number;
}

export interface SyncMetadata {
  followingDeletedAt: Record<string, number>;
}

export interface Settings {
  uiLanguage: UiLanguage;
  pollIntervalMinutes: number;
  launchAtLogin: boolean;
  minimizeToTray: boolean;
  showTrayIcon: boolean;
  notifyWhenAired: boolean;
  createWatchTasks: boolean;
  dailyTaskReminderEnabled: boolean;
  dailyTaskReminderTime: string;
  bangumiApiBaseUrl: string;
  titlePreference: TitlePreference;
}

export interface ConnectionTestResult {
  ok: boolean;
  message: string;
  baseUrl: string;
}

export interface CacheInfo {
  bytes: number;
  sessionBytes: number;
  legacyBytes: number;
  supported: boolean;
}

export interface BangumiCandidate {
  subjectId: number;
  name: string;
  nameCn: string;
  date?: string | null;
  platform?: string | null;
  score: number;
}

export interface BangumiTitleMatch {
  animeId: number;
  status: 'matched' | 'unmatched' | 'ambiguous' | 'unavailable';
  subjectId?: number;
  subjectIds?: number[];
  name?: string;
  nameCn?: string;
  confidence?: number;
  source?: string;
  resolverVersion?: number;
  checkedAt: number;
  candidates?: BangumiCandidate[];
}

export interface AppState {
  version: number;
  following: FollowedAnime[];
  tasks: WatchTask[];
  settings: Settings;
  bangumiTitles: Record<string, BangumiTitleMatch>;
  lastSyncAt: number;
  syncMetadata: SyncMetadata;
  bangumiSyncSettings?: BangumiSyncSettings;
  bangumiSyncStatus?: BangumiSyncStatus;
  runtime?: {
    isDesktop: boolean;
    notificationsSupported: boolean;
    notificationPermissionGranted?: boolean;
    exactSchedulingGranted?: boolean;
    platform: string;
    edition: 'standard' | 'original';
  };
}

export interface WebDavConfig {
  supported: boolean;
  enabled: boolean;
  baseUrl: string;
  username: string;
  hasPassword: boolean;
  lastSyncAt: number;
  lastError: string;
}

export interface WebDavSyncResult {
  ok: boolean;
  changed: boolean;
  syncedAt: number;
  message: string;
}

export interface DesktopApi {
  getState(): Promise<AppState>;
  fetchSeason(params: { season: Season; year: number }): Promise<Anime[]>;
  toggleFollow(anime: Anime): Promise<AppState>;
  updateFollowTitle(animeId: number, displayTitle: string): Promise<AppState>;
  resolveBangumiTitle?(anime: Anime): Promise<BangumiTitleMatch>;
  testBangumiConnection?(baseUrl: string): Promise<ConnectionTestResult>;
  toggleTask(taskId: string): Promise<AppState>;
  updateSettings(settings: Partial<Settings>): Promise<AppState>;
  syncNow(): Promise<{ created: number; syncedAt: number }>;
  getCacheInfo(): Promise<CacheInfo>;
  clearCache(): Promise<CacheInfo>;
  getWebDavConfig(): Promise<WebDavConfig>;
  saveWebDavConfig(config: { enabled: boolean; baseUrl: string; username: string; password?: string }): Promise<WebDavConfig>;
  testWebDavConnection(): Promise<{ ok: boolean; message: string }>;
  syncWebDav(): Promise<WebDavSyncResult>;
  requestExactScheduling?(): Promise<void>;
  bangumiAuthStatus?(): Promise<BangumiAuthStatus>;
  bangumiSaveToken?(params: { token: string }): Promise<{ ok: boolean; message: string }>;
  bangumiDisconnect?(): Promise<{ ok: boolean; message: string }>;
  bangumiTestConnection?(params: { baseUrl?: string | null }): Promise<BangumiConnectionTestResult>;
  bangumiGetUserProfile?(): Promise<BangumiUserProfile | null>;
  bangumiGetUserCollections?(params: { offset?: number; limit?: number }): Promise<{ total: number; items: BangumiCollectionItem[] }>;
  bangumiResolveMapping?(params: { animeId: number }): Promise<BangumiMappingResolution>;
  bangumiConfirmMapping?(params: { animeId: number; subjectId: number }): Promise<AppState>;
  bangumiSkipMapping?(params: { animeId: number }): Promise<AppState>;
  bangumiGetSubjectExtras?(params: { subjectId: number }): Promise<BangumiSubjectExtras | null>;
  bangumiSyncNow?(): Promise<BangumiSyncResult>;
  bangumiUpdateSyncSettings?(params: BangumiSyncSettingsPatch): Promise<AppState>;
  bangumiSetRating?(params: { subjectId: number; rating: number | null }): Promise<{ ok: boolean; message: string }>;
  bangumiSetCollectionStatus?(params: { subjectId: number; status: BangumiCollectionStatus }): Promise<{ ok: boolean; message: string; state: AppState }>;
  openExternal(url: string): Promise<void>;
  onStateChanged(callback: (state: AppState) => void): () => void;
  // stale=true 仅 standard 版 Bangumi 主链会带：网络失败后回落过期缓存兜底（无后台刷新调度）。
  onSeasonUpdated(callback: (update: { season: Season; year: number; anime: Anime[]; fetchedAt: number; stale?: boolean }) => void): () => void;
  onOpenTasks?(callback: () => void): () => void;
  onFinaleCompleted?(callback: (payload: BangumiFinaleCompletedPayload) => void): () => void;
}

declare global {
  interface Window {
    animeTracker?: DesktopApi;
  }
}

// —— Bangumi 标准版迁移（Phase 0 schema 冻结）——
export type BangumiConflictPolicy = 'latest' | 'local-first' | 'bangumi-first';
export type BangumiMappingMethod = 'local' | 'external' | 'title-year' | 'manual';
export type BangumiMappingConfidence = 'high' | 'medium' | 'low';
export type BangumiEpisodeType = 'regular' | 'special' | 'movie' | 'ova' | 'unknown';
export type BangumiLastChangedBy = 'local' | 'bangumi' | 'webdav';

export interface BangumiSyncSettings {
  apiBaseUrl: string;
  syncEnabled: boolean;          // 默认 false
  pullCollections: boolean;      // 默认 true
  pushLocalChanges: boolean;     // 默认 false
  pushCompletedEpisodes: boolean;// 默认 false
  pullExternalStatus: boolean;   // 默认 true
  conflictPolicy: BangumiConflictPolicy;
  preferredBroadcastSites: string[]; // 默认 ["bangumi","ani_one","ani_one_asia","gamer","unext"]
}

export interface BangumiMapping {
  method: BangumiMappingMethod;
  confidence: BangumiMappingConfidence;
  updatedAt: number; // 秒级时间戳（与 Rust 侧冻结单位一致；syncUpdatedAt 才用毫秒）
}

export interface BangumiAiringInfo {
  nextEpisode?: number | null;
  nextAiringAt?: number | null; // 秒级时间戳；四级来源解析后的结果
}

export interface BangumiSubjectRecord {
  subjectId: number;      // 主键
  source: 'bangumi';
  title: string;
  titleOriginal?: string | null;
  titleRomaji?: string | null;
  coverImage: string;
  format?: string | null;
  episodes?: number | null;
  airing?: BangumiAiringInfo | null;
  bangumiStatus?: string | null;  // wish/doing/done/on_hold/dropped
  rating?: number | null;         // 个人评分 0-10
  watchedEpisode?: number | null;
  anilistId?: number | null;      // 兼容字段，可空
  mapping?: BangumiMapping | null;
  mappingPending?: boolean;       // 迁移中间态
  lastPulledFromBangumiAt?: number | null;
  lastPushedToBangumiAt?: number | null;
  lastPulledPayloadHash?: string | null;
  lastPushedPayloadHash?: string | null;
  lastChangedBy?: BangumiLastChangedBy;
  syncUpdatedAt?: number;
}

export interface BangumiEpisodeRecord {
  id: string;              // "bgm-episode-{episodeId}" 或兼容旧 "{animeId}-{episode}"
  subjectId?: number | null;      // 迁移中间态可为 null（与 Rust Option<i64> 一致）
  episodeId?: number | null;      // 旧任务为 null
  episodeNumber?: number | null;
  episodeSortKey: string;         // 不假设整数
  episodeType: BangumiEpisodeType;
  title?: string | null;
  status: 'pending' | 'completed';
  completedAt?: number | null;
  createdAt?: number;
  animeId?: number | null;        // 旧任务兼容（AniList id）
  airingAt?: number | null;
  syncUpdatedAt?: number;
  lastChangedBy?: BangumiLastChangedBy;
}

export interface BangumiSyncStatus { // 本地-only，绝不进坚果云文档
  lastFullSyncAt?: number | null;
  lastWebDavSyncAt?: number | null;
  lastBangumiSyncAt?: number | null;
  lastScheduleSyncAt?: number | null;
  lastSyncError?: string | null;
}

export interface BangumiUserSummary {
  id: number;
  username: string;
  nickname: string;
  avatarUrl?: string | null;
}

// —— Phase 1（Token + 连接 + 只读，Rust 命令镜像，camelCase）——
export interface BangumiAuthStatus {
  supported: boolean;
  hasToken: boolean;
  apiBaseUrl: string;
}

export interface BangumiUserProfile {
  id: number;
  username: string;
  nickname: string;
  avatar?: { large?: string | null; medium?: string | null; small?: string | null } | null;
  sign?: string | null;
  userGroup?: number | null;
}

export interface BangumiCollectionItem {
  subjectId: number;
  subjectType: number;
  rate?: number | null;
  collectionType: number;
  tags: string[];
  epStatus?: number | null;
  volStatus?: number | null;
  updatedAt?: string | null;
  private?: boolean | null;
  comment?: string | null;
}

export interface BangumiConnectionTestResult {
  ok: boolean;
  message: string;
  username?: string | null;
  nickname?: string | null;
}

// Phase 2 已落地：FollowedAnime 新增可选字段 source/anilistId/mapping/mappingPending（见上方接口）。
//   WatchTask 的可选字段 episodeId/episodeSortKey/episodeType/subjectId 待任务层迁移接入后再补。

// —— Phase 2（季度主链 + 主键迁移 + 卡片增强，Rust 命令镜像，camelCase）——
export interface BangumiMappingCandidate {
  subjectId: number;
  name: string;
  nameCn: string | null;
  date: string | null;
  begin?: string | null;
  score?: number;
}

export interface BangumiMappingResolution {
  status: 'mapped' | 'pending' | 'unavailable';
  subjectId: number | null;
  candidates: BangumiMappingCandidate[];
  anime: {
    id: number;
    displayTitle: string;
    seasonYear: number | null;
    format: string | null;
    coverImage: string;
  };
}

export interface BangumiSubjectExtras {
  fetchedAt: number;
  rating?: { score: number | null; total: number | null; rank: number | null } | null;
  tags: Array<{ name: string; count: number }>;
  characters: Array<{ id: number; name: string; nameCn: string | null; relation: string; imageUrl?: string | null }>;
  related: Array<{ id: number; name: string; nameCn: string | null; relation: string; imageUrl?: string | null }>;
  staff: Array<{ key: string; value: string }>;
  siteUrl: string;
}

// —— Phase 3（收藏/评分/进度，Rust 命令镜像，camelCase）——
export interface BangumiSyncSuggestion {
  subjectId: number;
  nameCn: string | null;
  type: number; // Bangumi SubjectCollectionType：1 wish / 2 done / 3 doing / 4 on_hold / 5 dropped
}

export interface BangumiSyncReport {
  pulled: number;
  followed: number;
  unfollowed: number;
  completedTasks: number;
  suggestions: BangumiSyncSuggestion[];
  conflicts: number;
  pushed: number;
  errors: string[];
}

export interface BangumiSyncResult {
  ok: boolean;
  message: string;
  report: BangumiSyncReport;
}

export interface BangumiSyncSettingsPatch {
  syncEnabled?: boolean;
  pullCollections?: boolean;
  pushLocalChanges?: boolean;
  pushCompletedEpisodes?: boolean;
  pullExternalStatus?: boolean;
  conflictPolicy?: BangumiConflictPolicy;
}
