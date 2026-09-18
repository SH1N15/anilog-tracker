import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Bell,
  AlertTriangle,
  BellRing,
  CalendarDays,
  CalendarRange,
  Check,
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Circle,
  Clock3,
  Cloud,
  ExternalLink,
  Filter,
  HardDrive,
  Inbox,
  Languages,
  LayoutGrid,
  Link2,
  ListChecks,
  LoaderCircle,
  Minus,
  MonitorDot,
  Network,
  RefreshCw,
  Save,
  ScrollText,
  Pencil,
  Search,
  Settings,
  SlidersHorizontal,
  Sparkles,
  Star,
  Tag,
  Trash2,
  User,
  Users,
  Undo2,
  X,
} from 'lucide-react';
import { api } from './api';
import type { Anime, AppState, BangumiAuthStatus, BangumiCollectionStatus, BangumiConflictPolicy, BangumiFinaleCompletedPayload, BangumiMappingResolution, BangumiSubjectExtras, BangumiSyncReport, BangumiSyncSettingsPatch, BangumiTitleMatch, BangumiUserProfile, FollowedAnime, Season, SeasonViewMode, Settings as AppSettings, UiLanguage, ViewId, WatchTask, WebDavConfig } from './types';
import { IS_ORIGINAL_EDITION, productName, titleForPreference } from './edition';
import { localizeMessage, normalizeUiLanguage, tr } from './i18n';
import { createStateRefreshController, type StateRefreshController } from './state-refresh';
import { isCompletedHistory, isPendingHistory, isScheduledHistoryReset, needsHistoryReview } from './task-history';
import { IS_TAURI_APP } from './platform/tauri';
import {
  currentSeason,
  formatAiring,
  formatLabel,
  localAiringWeekday,
  relativeTime,
  reminderTitleOf,
  SEASONS,
  seasonLabel,
  seasonMonths,
  seasonName,
  secondaryTitle,
  stripDescription,
  titleOf,
} from './utils';

const EMPTY_STATE: AppState = {
  version: 2,
  following: [],
  tasks: [],
  bangumiTitles: {},
  settings: { uiLanguage: 'zh-CN', pollIntervalMinutes: 5, launchAtLogin: false, minimizeToTray: true, showTrayIcon: true, notifyWhenAired: true, createWatchTasks: true, dailyTaskReminderEnabled: false, dailyTaskReminderTime: '20:00', bangumiApiBaseUrl: IS_ORIGINAL_EDITION ? '' : 'https://sh1n.cc.cd/v0', titlePreference: 'auto' },
  lastSyncAt: 0,
  syncMetadata: { followingDeletedAt: {} },
};

const NAV_ITEMS: Array<{ id: ViewId; label: [string, string]; icon: typeof CalendarDays }> = [
  { id: 'season', label: ['季度新番', 'Seasonal Anime'], icon: CalendarDays },
  { id: 'tasks', label: ['观看任务', 'Watch Tasks'], icon: ListChecks },
  { id: 'following', label: ['我的追番', 'Following'], icon: Bell },
  { id: 'settings', label: ['偏好设置', 'Settings'], icon: Settings },
];

const UI_STATE_KEY = IS_ORIGINAL_EDITION ? 'anilog-original-ui-state' : 'anilog-ui-state';

// Phase 3：Bangumi 收藏状态徽章（doing=追番中即当前默认态，不展示徽章）。
const BANGUMI_STATUS_LABELS: Record<string, [string, string]> = {
  done: ['已看完', 'Completed'],
  dropped: ['已弃番', 'Dropped'],
  on_hold: ['搁置', 'On hold'],
  wish: ['想看', 'Wish to watch'],
};
// 状态驱动追踪：行内状态下拉的选项（dropped 会触发抛弃追番确认并写回 Bangumi）。
const BANGUMI_STATUS_OPTIONS: Array<{ value: BangumiCollectionStatus; label: [string, string] }> = [
  { value: 'doing', label: ['在看', 'Watching'] },
  { value: 'wish', label: ['想看', 'Plan to watch'] },
  { value: 'on_hold', label: ['搁置', 'On hold'] },
  { value: 'done', label: ['看过', 'Watched'] },
  { value: 'dropped', label: ['抛弃追番', 'Drop'] },
];
// 状态分组顺序：在看（含未标记 bangumiStatus 的旧条目）→ 想看 → 搁置 → 看完；dropped 条目已被取消追番，不展示。
const FOLLOWING_GROUP_ORDER: Array<{ key: 'doing' | 'wish' | 'on_hold' | 'done'; label: [string, string] }> = [
  { key: 'doing', label: ['在看', 'Watching'] },
  { key: 'wish', label: ['想看', 'Plan to watch'] },
  { key: 'on_hold', label: ['搁置', 'On hold'] },
  { key: 'done', label: ['看完', 'Finished'] },
];

function followingGroupOf(item: FollowedAnime): 'doing' | 'wish' | 'on_hold' | 'done' {
  if (item.bangumiStatus === 'wish') return 'wish';
  if (item.bangumiStatus === 'on_hold') return 'on_hold';
  if (item.bangumiStatus === 'done') return 'done';
  return 'doing';
}
const BANGUMI_RATING_OPTIONS = Array.from({ length: 11 }, (_, value) => value);
// 问题 4：状态分组折叠记忆（独立 key，按 edition 区分，沿用 anilog-ui-state 的读写模式）。
const FOLLOW_GROUPS_KEY = IS_ORIGINAL_EDITION ? 'anilog-original-follow-groups' : 'anilog-follow-groups';
type FollowingGroupKey = 'doing' | 'wish' | 'on_hold' | 'done';
// 默认：在看展开，想看/搁置/看完收起。
const FOLLOW_GROUPS_DEFAULT_COLLAPSED: Record<FollowingGroupKey, boolean> = { doing: false, wish: true, on_hold: true, done: true };

function loadCollapsedGroups(): Record<FollowingGroupKey, boolean> {
  try {
    const saved = JSON.parse(localStorage.getItem(FOLLOW_GROUPS_KEY) || '{}');
    const next = { ...FOLLOW_GROUPS_DEFAULT_COLLAPSED };
    (Object.keys(FOLLOW_GROUPS_DEFAULT_COLLAPSED) as FollowingGroupKey[]).forEach((key) => {
      if (typeof saved[key] === 'boolean') next[key] = saved[key];
    });
    return next;
  } catch {
    return { ...FOLLOW_GROUPS_DEFAULT_COLLAPSED };
  }
}
// Bangumi SubjectCollectionType（1 wish / 2 done / 3 doing / 4 on_hold / 5 dropped）→ 建议列表展示名。
const BANGUMI_SUGGESTION_TYPE_LABELS: Record<number, string> = { 1: '想看', 2: '看过', 3: '在看', 4: '搁置', 5: '弃番' };

function loadUiState(fallback: { season: Season; year: number }): { view: ViewId; season: Season; year: number; seasonView: SeasonViewMode } {
  try {
    const saved = JSON.parse(localStorage.getItem(UI_STATE_KEY) || '{}');
    const views: ViewId[] = ['season', 'tasks', 'following', 'settings'];
    return {
      view: views.includes(saved.view) ? saved.view : 'season',
      season: SEASONS.some((item) => item.value === saved.season) ? saved.season : fallback.season,
      year: Number.isInteger(saved.year) && saved.year >= 2000 && saved.year <= 2100 ? saved.year : fallback.year,
      seasonView: saved.seasonView === 'all' ? 'all' : 'weekday',
    };
  } catch {
    return { view: 'season', ...fallback, seasonView: 'weekday' };
  }
}

function App() {
  const nowSeason = currentSeason();
  const initialUi = useMemo(() => loadUiState(nowSeason), [nowSeason.season, nowSeason.year]);
  const [view, setView] = useState<ViewId>(initialUi.view);
  const [state, setState] = useState<AppState>(EMPTY_STATE);
  const [stateReady, setStateReady] = useState(false);
  const [stateLoadError, setStateLoadError] = useState('');
  const stateRefresh = useRef<StateRefreshController | null>(null);
  const [season, setSeason] = useState<Season>(initialUi.season);
  const [year, setYear] = useState(initialUi.year);
  const [seasonView, setSeasonView] = useState<SeasonViewMode>(initialUi.seasonView);
  const [anime, setAnime] = useState<Anime[]>([]);
  const [loading, setLoading] = useState(true);
  // 问题 1：standard 版 season-updated 事件可能带 stale=true（Bangumi 网络失败后回落过期缓存）。
  const [seasonStale, setSeasonStale] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [error, setError] = useState('');
  const [lastSyncMessage, setLastSyncMessage] = useState('');
  const seasonRequest = useRef(0);
  const language = normalizeUiLanguage(state.settings.uiLanguage);
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const navItems = NAV_ITEMS.map((item) => ({ ...item, text: t(...item.label) }));

  useEffect(() => {
    document.title = productName(language);
    document.documentElement.lang = language;
  }, [language]);

  useEffect(() => {
    const openTasks = () => setView('tasks');
    window.addEventListener('anilog:open-tasks', openTasks);
    const unsubscribeDesktop = api.onOpenTasks?.(openTasks);
    return () => {
      window.removeEventListener('anilog:open-tasks', openTasks);
      unsubscribeDesktop?.();
    };
  }, []);

  // 状态驱动追踪：最后一话看完后弹出完结评分横幅；多部完结时显示最新一条即可。
  const [finaleBanner, setFinaleBanner] = useState<BangumiFinaleCompletedPayload | null>(null);
  const [finaleRatingBusy, setFinaleRatingBusy] = useState(false);
  useEffect(() => {
    if (!IS_TAURI_APP) return;
    return api.onFinaleCompleted?.((payload) => setFinaleBanner(payload));
  }, []);

  const rateFinale = async (rating: number) => {
    const current = finaleBanner;
    if (!current || !api.bangumiSetRating) return;
    setFinaleRatingBusy(true);
    try {
      const result = await api.bangumiSetRating({ subjectId: current.subjectId, rating });
      setState(await api.getState());
      setLastSyncMessage(localizeMessage(result.message, language));
      setFinaleBanner(null);
    } catch (reason) {
      setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('评分同步失败', 'Rating sync failed'));
    } finally {
      setFinaleRatingBusy(false);
    }
  };

  useEffect(() => {
    const controller = createStateRefreshController({
      getState: api.getState,
      subscribe: api.onStateChanged,
      applyState: (nextState) => {
        setState(nextState);
        setStateReady(true);
        setStateLoadError('');
      },
      onError: (reason) => {
        const message = reason instanceof Error ? localizeMessage(reason.message, language) : t('无法读取本地状态', 'Could not read local data');
        setStateLoadError(message);
        setError(message);
      },
    });
    stateRefresh.current = controller;

    const refreshWhenVisible = () => {
      if (document.visibilityState === 'visible') void controller.refresh();
    };
    const refreshWhenFocused = () => { void controller.refresh(); };

    void controller.refresh(true);
    document.addEventListener('visibilitychange', refreshWhenVisible);
    window.addEventListener('focus', refreshWhenFocused);
    return () => {
      controller.dispose();
      if (stateRefresh.current === controller) stateRefresh.current = null;
      document.removeEventListener('visibilitychange', refreshWhenVisible);
      window.removeEventListener('focus', refreshWhenFocused);
    };
  }, [language]);

  useEffect(() => {
    localStorage.setItem(UI_STATE_KEY, JSON.stringify({ view, season, year, seasonView }));
  }, [view, season, year, seasonView]);

  const loadSeason = useCallback(async (force = false) => {
    const requestId = ++seasonRequest.current;
    setLoading(true);
    setSeasonStale(false);
    setError('');
    try {
      const nextAnime = await api.fetchSeason({ season, year, force });
      if (requestId === seasonRequest.current) setAnime(nextAnime);
    } catch (reason) {
      if (requestId === seasonRequest.current) setError(reason instanceof Error ? localizeMessage(reason.message, language) : t('无法读取本季番剧', 'Could not load this season'));
    } finally {
      if (requestId === seasonRequest.current) setLoading(false);
    }
  }, [season, year, language]);

  useEffect(() => {
    if (stateReady && view === 'season') void loadSeason();
  }, [loadSeason, view, stateReady]);

  useEffect(() => api.onSeasonUpdated((update) => {
    if (update.season === season && update.year === year) {
      setAnime(update.anime);
      setError('');
      setLoading(false);
      setSeasonStale(update.stale === true);
    }
  }), [season, year]);

  const syncNow = async () => {
    setSyncing(true);
    setLastSyncMessage('');
    try {
      if (IS_TAURI_APP && !IS_ORIGINAL_EDITION && state.bangumiSyncSettings?.syncEnabled && api.bangumiSyncNow) {
        const result = await api.bangumiSyncNow();
        setState(await api.getState());
        setLastSyncMessage(localizeMessage(result.message, language));
        if (view === 'season') await loadSeason(true);
        return;
      }
      const result = await api.syncNow();
      setState(await api.getState());
      setLastSyncMessage(result.warning ? localizeMessage(result.warning, language)
        : result.created ? t(`新增 ${result.created} 个观看任务`, `${result.created} watch task${result.created === 1 ? '' : 's'} added`) : t('已是最新状态', 'Already up to date'));
      if (view === 'season') await loadSeason(true);
    } catch (reason) {
      setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('同步失败', 'Sync failed'));
    } finally {
      setSyncing(false);
    }
  };

  // 问题 2/4：回调稳定化 + 跨键追番索引（id / anilistId / bangumiId 任一命中即视为已追）。
  // 问题 2：卡片开关的即时反馈（顶栏同步消息）与请求期间防连点；成功/失败都会落到 lastSyncMessage。
  const [followBusyKeys, setFollowBusyKeys] = useState<ReadonlySet<number>>(new Set());
  const toggleFollowAnime = useCallback(async (item: Anime) => {
    const busyKeys = new Set<number>([item.id]);
    if (typeof item.bangumiSubjectId === 'number') busyKeys.add(item.bangumiSubjectId);
    if (typeof item.anilistId === 'number') busyKeys.add(item.anilistId);
    setFollowBusyKeys((prev) => new Set([...prev, ...busyKeys]));
    try {
      const next = await api.toggleFollow(item);
      setState(next);
      const title = titleOf(item.title, language);
      const followedNow = next.following.some((entry) => entry.id === item.id
        || entry.anilistId === item.id
        || (typeof item.bangumiSubjectId === 'number' && entry.bangumiId === item.bangumiSubjectId)
        || (typeof item.anilistId === 'number' && entry.anilistId === item.anilistId));
      setLastSyncMessage(followedNow
        ? t(`已加入追番《${title}》`, `Following “${title}”`)
        : t(`已取消追番《${title}》`, `Unfollowed “${title}”`));
    } catch (reason) {
      setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('追番操作失败', 'Follow action failed'));
    } finally {
      setFollowBusyKeys((prev) => {
        const rest = new Set(prev);
        busyKeys.forEach((key) => rest.delete(key));
        return rest;
      });
    }
  }, [language]);
  const followedKeys = useMemo(() => {
    const keys = new Set<number>();
    state.following.forEach((item) => {
      if (Number.isFinite(item.id)) keys.add(item.id);
      if (typeof item.anilistId === 'number') keys.add(item.anilistId);
      if (typeof item.bangumiId === 'number') keys.add(item.bangumiId);
    });
    return keys;
  }, [state.following]);

  const pendingCount = state.tasks.filter(isPendingHistory).length;
  const isAndroid = state.runtime?.platform === 'android';

  if (!stateReady) {
    return (
      <main className="app-bootstrap" role={stateLoadError ? 'alert' : 'status'} aria-busy={!stateLoadError}>
        <strong>AniLog</strong>
        {stateLoadError ? (
          <>
            <p>{t('无法读取本地数据', 'Could not read local data')}</p>
            <span>{stateLoadError}</span>
            <button className="icon-button" title={t('重试', 'Retry')} aria-label={t('重试', 'Retry')} onClick={() => {
              setStateLoadError('');
              void stateRefresh.current?.refresh(true);
            }}><RefreshCw size={20} /></button>
          </>
        ) : (
          <p><LoaderCircle className="spin" size={20} />{t('正在读取本地数据', 'Loading local data')}</p>
        )}
      </main>
    );
  }

  return (
    <div className={`app-shell ${isAndroid ? 'android-app' : ''}`}>
      <aside className="sidebar">
        <button className="brand" onClick={() => setView('season')} aria-label={t('返回季度新番', 'Back to seasonal anime')}>
          <span className="brand-mark">A</span>
          <span>
            <strong>AniLog</strong>
            <small>追番日程</small>
          </span>
        </button>

        <nav className="main-nav" aria-label={t('主导航', 'Main navigation')}>
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.id}
                className={view === item.id ? 'active' : ''}
                onClick={() => setView(item.id)}
                aria-label={item.text}
                title={item.text}
              >
                <Icon size={19} strokeWidth={1.8} />
                <span>{item.text}</span>
                {item.id === 'tasks' && pendingCount > 0 && <span className="nav-count">{pendingCount}</span>}
              </button>
            );
          })}
        </nav>

        <div className="sidebar-status">
          <span className={`status-dot ${state.runtime?.isDesktop || isAndroid ? 'online' : ''}`} />
          <div>
            <strong>{state.runtime?.isDesktop ? t('后台提醒已就绪', 'Background alerts ready') : isAndroid ? t('Android 后台同步已就绪', 'Android background sync ready') : t('浏览器预览模式', 'Browser preview mode')}</strong>
            <small>{t(`${state.following.length} 部追番 · ${pendingCount} 项待看`, `${state.following.length} following · ${pendingCount} to watch`)}</small>
          </div>
        </div>
      </aside>

      <main className="main-content">
        <header className="topbar">
          <div>
            <p>{view === 'season' ? t('发现与安排', 'Discover and plan') : view === 'tasks' ? t('本地观看清单', 'Local watch list') : view === 'following' ? t('追番管理', 'Manage following') : t('应用设置', 'App preferences')}</p>
            <h1>{navItems.find((item) => item.id === view)?.text}</h1>
          </div>
          <div className="topbar-actions">
            {lastSyncMessage && <span className="sync-message">{lastSyncMessage}</span>}
            <button className="icon-button" title={t('立即同步更新', 'Sync now')} onClick={syncNow} disabled={syncing}>
              <RefreshCw size={18} className={syncing ? 'spin' : ''} />
            </button>
            <button className="inbox-button" onClick={() => setView('tasks')} aria-label={t(`${pendingCount} 项待看`, `${pendingCount} to watch`)}>
              <Inbox size={18} />
              <span>{t(`${pendingCount} 项待看`, `${pendingCount} to watch`)}</span>
            </button>
          </div>
        </header>

        <div className="view-container">
          {lastSyncMessage && <p className="mobile-sync-message" role="status">{lastSyncMessage}</p>}
          {finaleBanner && (
            <div className="finale-banner" role="status">
              <div className="finale-copy">
                <strong>{t(`《${finaleBanner.displayTitle}》已看完，去评分吧`, `“${finaleBanner.displayTitle}” finished — rate it now`)}</strong>
                <small>{t('已自动标记为看过', 'Automatically marked as watched')}</small>
              </div>
              <div className="finale-rating" aria-label={t('评分', 'Rating')}>
                <Star size={15} />
                {Array.from({ length: 10 }, (_, index) => index + 1).map((value) => (
                  <button key={value} disabled={finaleRatingBusy} onClick={() => void rateFinale(value)}>
                    {value}
                  </button>
                ))}
              </div>
              <button className="icon-button" title={t('关闭', 'Close')} aria-label={t('关闭评分提醒', 'Dismiss rating prompt')} onClick={() => setFinaleBanner(null)}>
                <X size={16} />
              </button>
            </div>
          )}
          {view === 'season' && (
            <SeasonView
              anime={anime}
              loading={loading}
              seasonStale={seasonStale}
              error={error}
              season={season}
              year={year}
              seasonView={seasonView}
              followedKeys={followedKeys}
              followBusyKeys={followBusyKeys}
              titleMatches={state.bangumiTitles}
              titlePreference={state.settings.titlePreference}
              language={language}
              onSeasonChange={setSeason}
              onYearChange={setYear}
              onSeasonViewChange={setSeasonView}
              onRetry={() => void loadSeason(true)}
              onToggleFollow={toggleFollowAnime}
            />
          )}
          {view === 'tasks' && <TasksView tasks={state.tasks} language={language}
            onToggle={async (id) => setState(await api.toggleTask(id))}
            onResolveReview={api.resolveTaskReview ? async (id, keep) => {
              try {
                setState(await api.resolveTaskReview!(id, keep));
                setLastSyncMessage(keep ? t('已确认观看记录', 'Watch record confirmed') : t('已撤销完成，原记录已留存', 'Completion reset; original record retained'));
              } catch (reason) {
                setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('核对失败', 'Review failed'));
              }
            } : undefined} />}
          {view === 'following' && (
            <FollowingView
              items={state.following}
              language={language}
              onOpenTasks={() => setView('tasks')}
              onRename={async (id, displayTitle) => setState(await api.updateFollowTitle(id, displayTitle))}
              onUnfollow={async (id) => {
                const source = anime.find((item) => item.id === id);
                const followed = state.following.find((item) => item.id === id);
                if (!followed) return;
                const pendingTaskCount = state.tasks.filter((task) => task.animeId === id && task.status === 'pending').length;
                const taskNotice = pendingTaskCount > 0
                  ? t(`取消追番后将移除 ${pendingTaskCount} 个待看任务，已完成记录会保留。`, `Unfollowing will remove ${pendingTaskCount} pending task${pendingTaskCount === 1 ? '' : 's'}. Completed history will be kept.`)
                  : t('取消追番后，已完成记录会保留。', 'Completed history will be kept after unfollowing.');
                if (!window.confirm(t(`确认取消追番《${followed.displayTitle}》吗？\n\n${taskNotice}`, `Unfollow “${followed.displayTitle}”?\n\n${taskNotice}`))) return;
                // 问题 2：失败也要落到顶栏消息（未处理 rejection 会让卡片停留在陈旧状态）。
                try {
                  if (api.unfollow) setState(await api.unfollow(id));
                  else if (source) setState(await api.toggleFollow(source));
                  // Bangumi 条目可能不在当前季度列表里；fabricate 的对象需带全标识字段（id 可为 subjectId）。
                  else setState(await api.toggleFollow({
                    ...followed,
                    coverImage: { medium: followed.coverImage },
                    source: followed.source || 'anilist',
                    bangumiSubjectId: followed.bangumiId ?? (followed.source === 'bangumi' ? followed.id : null),
                    anilistId: followed.anilistId ?? null,
                  }));
                  setLastSyncMessage(t(`已取消追番《${followed.displayTitle}》`, `Unfollowed “${followed.displayTitle}”`));
                } catch (reason) {
                  setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('取消追番失败', 'Unfollow failed'));
                }
              }}
              onConfirmMapping={async (animeId, subjectId) => {
                if (api.bangumiConfirmMapping) setState(await api.bangumiConfirmMapping({ animeId, subjectId }));
              }}
              onSkipMapping={async (animeId) => {
                if (api.bangumiSkipMapping) setState(await api.bangumiSkipMapping({ animeId }));
              }}
              onSetRating={async (subjectId, rating) => {
                if (!api.bangumiSetRating) return;
                try {
                  const result = await api.bangumiSetRating({ subjectId, rating });
                  setState(await api.getState());
                  setLastSyncMessage(localizeMessage(result.message, language));
                } catch (reason) {
                  setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('评分同步失败', 'Rating sync failed'));
                }
              }}
              onSetCollectionStatus={async (subjectId, status) => {
                if (status === 'dropped') {
                  const confirmed = window.confirm(t(
                    '抛弃后将取消追番：未完成任务删除，观看历史保留。确认？',
                    'Dropping will unfollow this title: pending tasks are deleted and watch history is kept. Confirm?',
                  ));
                  if (!confirmed) return;
                }
                if (!api.bangumiSetCollectionStatus) return;
                try {
                  const result = await api.bangumiSetCollectionStatus({ subjectId, status });
                  setState(result.state || await api.getState());
                  setLastSyncMessage(localizeMessage(result.message, language));
                } catch (reason) {
                  setLastSyncMessage(reason instanceof Error ? localizeMessage(reason.message, language) : t('状态同步失败', 'Status sync failed'));
                }
              }}
            />
          )}
          {view === 'settings' && (
            <SettingsView
              state={state}
              language={language}
              onChange={async (patch) => setState(await api.updateSettings(patch))}
              onApplyState={(next) => setState(next)}
            />
          )}
        </div>
      </main>
    </div>
  );
}

function SeasonView({
  anime,
  loading,
  seasonStale,
  error,
  season,
  year,
  seasonView,
  followedKeys,
  followBusyKeys,
  titleMatches,
  titlePreference,
  language,
  onSeasonChange,
  onYearChange,
  onSeasonViewChange,
  onRetry,
  onToggleFollow,
}: {
  anime: Anime[];
  loading: boolean;
  seasonStale: boolean;
  error: string;
  season: Season;
  year: number;
  seasonView: SeasonViewMode;
  followedKeys: Set<number>;
  followBusyKeys: ReadonlySet<number>;
  titleMatches: Record<string, BangumiTitleMatch>;
  titlePreference: AppSettings['titlePreference'];
  language: UiLanguage;
  onSeasonChange: (season: Season) => void;
  onYearChange: (year: number) => void;
  onSeasonViewChange: (view: SeasonViewMode) => void;
  onRetry: () => void;
  onToggleFollow: (anime: Anime) => Promise<void>;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const [query, setQuery] = useState('');
  const [format, setFormat] = useState('ALL');
  const [onlyFollowing, setOnlyFollowing] = useState(false);
  const [selected, setSelected] = useState<Anime | null>(null);
  const requestedTitles = useRef(new Set<number>());

  useEffect(() => {
    requestedTitles.current.clear();
  }, [season, year]);

  const requestChineseTitle = useCallback((item: Anime) => {
    if (IS_ORIGINAL_EDITION || !api.resolveBangumiTitle) return;
    // Bangumi 来源条目自带中文名，不需要标题 resolver（避免逐卡 N+1 搜索）。
    if (item.source === 'bangumi') return;
    if (requestedTitles.current.has(item.id)) return;
    requestedTitles.current.add(item.id);
    void api.resolveBangumiTitle(item).catch(() => {});
  }, []);

  // 问题 4：追番判重跨键——同一部番可能以旧 anilist 键或新 subjectId 键存在。
  // 验收第 5 轮修正：bangumi 来源卡片（bangumiSubjectId 或 source==='bangumi'，
  // anime.id 即 subjectId）只按 subject 精确匹配——anilistId 可能被共享同一
  // AniList 条目的其他季撞车（如丧失篇/夺还篇共用 189046），不能作为判定依据。
  const isFollowed = useCallback((item: Anime) => {
    if (item.source === 'bangumi' || typeof item.bangumiSubjectId === 'number') {
      const subjectId = typeof item.bangumiSubjectId === 'number' ? item.bangumiSubjectId : item.id;
      return followedKeys.has(subjectId) || followedKeys.has(item.id);
    }
    return followedKeys.has(item.id)
      || (typeof item.anilistId === 'number' && followedKeys.has(item.anilistId));
  }, [followedKeys]);

  const visible = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return anime.filter((item) => {
      const matchesQuery = !normalized || [IS_ORIGINAL_EDITION ? undefined : titleMatches[String(item.id)]?.nameCn, item.title.native, item.title.english, item.title.romaji, item.studios?.nodes[0]?.name]
        .some((value) => value?.toLowerCase().includes(normalized));
      const matchesFormat = format === 'ALL' || item.format === format;
      const matchesFollowing = !onlyFollowing || isFollowed(item);
      return matchesQuery && matchesFormat && matchesFollowing;
    });
  }, [anime, query, format, onlyFollowing, isFollowed, titleMatches]);

  const weekdayGroups = useMemo(() => {
    const groups = Array.from({ length: 8 }, () => [] as Anime[]);
    visible.forEach((item) => groups[localAiringWeekday(item)].push(item));
    return groups;
  }, [visible]);

  // 问题 2：稳定回调，配合 memo 化的 AnimeCard 避免滚动时整列表重渲染。
  const openAnime = useCallback((item: Anime) => setSelected(item), []);
  const toggleFollowCard = useCallback((item: Anime) => { void onToggleFollow(item); }, [onToggleFollow]);

  const weekdayLabels: Array<[string, string]> = [
    ['周一', 'Monday'], ['周二', 'Tuesday'], ['周三', 'Wednesday'], ['周四', 'Thursday'],
    ['周五', 'Friday'], ['周六', 'Saturday'], ['周日', 'Sunday'], ['播出日待定', 'TBA'],
  ];

  const renderAnimeCard = (item: Anime) => (
    <AnimeCard
      key={item.id}
      anime={item}
      titleMatch={titleMatches[String(item.id)]}
      titlePreference={titlePreference}
      language={language}
      followed={isFollowed(item)}
      followBusy={followBusyKeys.has(item.id)}
      onVisible={requestChineseTitle}
      onOpen={openAnime}
      onToggle={toggleFollowCard}
    />
  );

  const shiftYear = (delta: number) => onYearChange(Math.max(2000, Math.min(2100, year + delta)));

  return (
    <>
      <section className="season-toolbar" aria-label={t('季度选择', 'Season selection')}>
        <div className="year-stepper">
          <button title={t('上一年', 'Previous year')} onClick={() => shiftYear(-1)}><ChevronLeft size={18} /></button>
          <strong>{year}</strong>
          <button title={t('下一年', 'Next year')} onClick={() => shiftYear(1)}><ChevronRight size={18} /></button>
        </div>
        <div className="segmented-control">
          {SEASONS.map((item) => (
            <button key={item.value} className={season === item.value ? 'selected' : ''} onClick={() => onSeasonChange(item.value)}>
              {language === 'en-US' ? seasonName(item.value, language) : `${seasonName(item.value, language)}季`} <small>{seasonMonths(item.value, language)}</small>
            </button>
          ))}
        </div>
      </section>

      <section className="section-heading">
        <div>
          <div className="eyebrow"><Sparkles size={14} /> {seasonLabel(season, year, language)}</div>
          <h2>{t('新番更新时间表', 'Seasonal release schedule')}</h2>
          <p>{loading ? (IS_ORIGINAL_EDITION ? t('正在读取 AniList…', 'Loading AniList…') : t('正在读取新番数据…', 'Loading seasonal data…')) : t(`${anime.length} 部作品 · 时间按本机时区显示`, `${anime.length} titles · Times shown in your local time zone`)}
            {!loading && seasonStale && <span style={{ display: 'block', fontSize: 12, opacity: 0.75 }}>{t('网络暂时不可用，已显示缓存数据', 'Network unavailable — showing cached data')}</span>}
          </p>
        </div>
        <div className="filter-row">
          <label className="search-field">
            <Search size={17} />
            <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t('搜索番剧或制作公司', 'Search anime or studio')} />
            {query && <button title={t('清空搜索', 'Clear search')} onClick={() => setQuery('')}><X size={15} /></button>}
          </label>
          <label className="select-field">
            <Filter size={16} />
            <select value={format} onChange={(event) => setFormat(event.target.value)}>
              <option value="ALL">{t('全部类型', 'All formats')}</option>
              <option value="TV">{t('TV 动画', 'TV')}</option>
              <option value="ONA">{t('网络动画', 'ONA')}</option>
              <option value="MOVIE">{t('电影', 'Movie')}</option>
              <option value="OVA">OVA</option>
              <option value="SPECIAL">{t('特别篇', 'Special')}</option>
            </select>
          </label>
          <label className="check-filter">
            <input type="checkbox" checked={onlyFollowing} onChange={(event) => setOnlyFollowing(event.target.checked)} />
            {t('只看已追', 'Following only')}
          </label>
          <div className="segmented-control view-mode-control" role="group" aria-label={t('新番排列方式', 'Season layout')}>
            <button className={seasonView === 'weekday' ? 'selected' : ''} aria-pressed={seasonView === 'weekday'} onClick={() => onSeasonViewChange('weekday')}>
              <CalendarRange size={14} /> <span>{t('星期', 'Weekday')}</span>
            </button>
            <button className={seasonView === 'all' ? 'selected' : ''} aria-pressed={seasonView === 'all'} onClick={() => onSeasonViewChange('all')}>
              <LayoutGrid size={14} /> <span>{t('全部', 'All')}</span>
            </button>
          </div>
        </div>
      </section>

      {error ? (
        <EmptyState icon={MonitorDot} title={t('暂时无法读取新番', 'Could not load seasonal anime')} body={error} action={t('重新载入', 'Reload')} onAction={onRetry} />
      ) : loading ? (
        <div className="anime-grid" aria-label={t('正在载入', 'Loading')}>
          {Array.from({ length: 10 }, (_, index) => <div className="anime-skeleton" key={index}><span /><i /><i /></div>)}
        </div>
      ) : visible.length === 0 ? (
        <EmptyState icon={Search} title={t('没有符合条件的番剧', 'No matching anime')} body={t('调整搜索或筛选条件后再试。', 'Try changing the search or filters.')} />
      ) : (
        seasonView === 'all' ? (
          <div className="anime-grid">{visible.map(renderAnimeCard)}</div>
        ) : (
          <div className="weekday-sections">
            {weekdayGroups.map((items, index) => items.length > 0 && (
              <section className="weekday-section" key={index} aria-labelledby={`weekday-${index}`}>
                <header className="weekday-heading">
                  <h3 id={`weekday-${index}`}>{t(...weekdayLabels[index])}</h3>
                  <span>{t(`${items.length} 部`, `${items.length} title${items.length === 1 ? '' : 's'}`)}</span>
                </header>
                <div className="anime-grid">{items.map(renderAnimeCard)}</div>
              </section>
            ))}
          </div>
        )
      )}

      {selected && (
        <AnimeDetail
          anime={selected}
          titleMatch={titleMatches[String(selected.id)]}
          titlePreference={titlePreference}
          language={language}
          followed={isFollowed(selected)}
          onClose={() => setSelected(null)}
          onToggle={() => onToggleFollow(selected)}
        />
      )}
    </>
  );
}

function localizedTitle(anime: Anime, match?: BangumiTitleMatch, preference: AppSettings['titlePreference'] = 'auto', language: UiLanguage = 'zh-CN'): string {
  if (IS_ORIGINAL_EDITION) return titleForPreference(anime.title, preference, language);
  // Bangumi 来源条目：native 即中文名（displayTitle 优先中文名），无需走标题 resolver。
  if (anime.source === 'bangumi') return titleOf(anime.title, language);
  return match?.status === 'matched' && match.nameCn ? match.nameCn : reminderTitleOf(anime.title);
}

// 问题 2：memo 化卡片。父层所有 props 均为稳定引用（回调走 useCallback、数据来自
// useMemo 的列表/索引），滚动与工具栏输入变化时未受影响的卡片不会重渲染。
// 问题 3：Bangumi 评分为 0-10（一位小数），不能用 AniList 的百分比样式。
const AnimeCard = memo(function AnimeCard({
  anime,
  titleMatch,
  titlePreference,
  language,
  followed,
  followBusy,
  onOpen,
  onToggle,
  onVisible,
}: {
  anime: Anime;
  titleMatch?: BangumiTitleMatch;
  titlePreference: AppSettings['titlePreference'];
  language: UiLanguage;
  followed: boolean;
  followBusy: boolean;
  onOpen: (anime: Anime) => void;
  onToggle: (anime: Anime) => void;
  onVisible: (anime: Anime) => void;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const next = anime.nextAiringEpisode;
  const cardRef = useRef<HTMLElement>(null);
  const displayTitle = localizedTitle(anime, titleMatch, titlePreference, language);
  const originalTitle = titleOf(anime.title, language);
  // Bangumi 条目评分 0-10；无 source 的旧数据用 averageScore<=10 且带 subjectId 兜底判断。
  const bangumiScore = anime.source === 'bangumi'
    || (!anime.source && typeof anime.bangumiSubjectId === 'number' && (anime.averageScore ?? 0) <= 10);
  const scoreText = anime.averageScore
    ? (bangumiScore ? `★ ${anime.averageScore.toFixed(1)}` : `${anime.averageScore}%`)
    : 'NEW';

  useEffect(() => {
    if (IS_ORIGINAL_EDITION || titleMatch || !cardRef.current) return;
    if (!('IntersectionObserver' in window)) {
      onVisible(anime);
      return;
    }
    const root = document.querySelector('.view-container');
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        onVisible(anime);
        observer.disconnect();
      }
    }, { root, rootMargin: '700px 0px' });
    observer.observe(cardRef.current);
    return () => observer.disconnect();
  }, [anime.id, titleMatch, onVisible]);

  return (
    <article className="anime-card" ref={cardRef}>
      <button className="poster-button" onClick={() => onOpen(anime)} aria-label={t(`查看 ${displayTitle} 详情`, `View details for ${displayTitle}`)}>
        {/* 问题 2：卡片封面固定用 medium（extraLarge 只给详情弹窗）；宽高比由 .poster-button 的 aspect-ratio 固定。 */}
        <img src={anime.coverImage?.medium || anime.coverImage?.extraLarge} alt="" loading="lazy" decoding="async" />
        <span className="score">{scoreText}</span>
      </button>
      <div className="anime-card-body">
        <div className="anime-meta"><span>{formatLabel(anime.format, language)}</span><span>{anime.episodes ? t(`${anime.episodes} 集`, `${anime.episodes} episodes`) : t('集数待定', 'Episodes TBA')}</span></div>
        <button className="anime-title" onClick={() => onOpen(anime)}>{displayTitle}</button>
        <p className="anime-subtitle">{originalTitle !== displayTitle ? originalTitle : secondaryTitle(anime.title, language) || anime.studios?.nodes[0]?.name || t('制作信息待定', 'Studio TBA')}</p>
        <div className="airing-line">
          <Clock3 size={15} />
          <span>{next ? t(`第 ${next.episode} 集 · ${formatAiring(next.airingAt, true, language, next.airingPrecision)}`, `Episode ${next.episode} · ${formatAiring(next.airingAt, true, language, next.airingPrecision)}`) : anime.status === 'FINISHED' ? t('本季已完结', 'Finished') : t('更新时间待定', 'Schedule TBA')}</span>
        </div>
        <button className={`follow-button ${followed ? 'followed' : ''}`} disabled={followBusy} onClick={() => onToggle(anime)}>
          {followed ? <Check size={17} /> : <Bell size={17} />}
          {followed ? t('已加入追番', 'Following') : t('加入追番', 'Follow')}
        </button>
      </div>
    </article>
  );
});

function AnimeDetail({ anime, titleMatch, titlePreference, language, followed, onClose, onToggle }: { anime: Anime; titleMatch?: BangumiTitleMatch; titlePreference: AppSettings['titlePreference']; language: UiLanguage; followed: boolean; onClose: () => void; onToggle: () => void }) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const displayTitle = localizedTitle(anime, titleMatch, titlePreference, language);
  const originalTitle = titleOf(anime.title, language);
  // Phase 2：standard 下 Bangumi 来源条目（source='bangumi' 或带 bangumiSubjectId）惰性加载条目增强数据；失败静默折叠。
  const bangumiSubjectId = !IS_ORIGINAL_EDITION && (anime.source === 'bangumi' || typeof anime.bangumiSubjectId === 'number')
    ? (typeof anime.bangumiSubjectId === 'number' ? anime.bangumiSubjectId : anime.id)
    : null;
  const [extras, setExtras] = useState<BangumiSubjectExtras | null>(null);
  const [extrasReady, setExtrasReady] = useState(false);
  const [extrasLoading, setExtrasLoading] = useState(false);

  useEffect(() => {
    if (bangumiSubjectId == null || !api.bangumiGetSubjectExtras) {
      setExtras(null);
      setExtrasReady(false);
      setExtrasLoading(false);
      return;
    }
    let active = true;
    setExtras(null);
    setExtrasReady(false);
    setExtrasLoading(true);
    api.bangumiGetSubjectExtras({ subjectId: bangumiSubjectId })
      .then((result) => {
        if (!active) return;
        setExtras(result);
        setExtrasReady(result != null);
      })
      .catch(() => { if (active) setExtrasReady(false); })
      .finally(() => { if (active) setExtrasLoading(false); });
    return () => { active = false; };
  }, [bangumiSubjectId]);

  const tags = extras ? [...extras.tags].filter((tag) => tag.count > 0).sort((a, b) => b.count - a.count).slice(0, 8) : [];
  const staff = extras ? extras.staff.slice(0, 6) : [];
  const characters = extras ? extras.characters.slice(0, 8) : [];
  const related = extras ? extras.related.slice(0, 6) : [];
  const rating = extras?.rating || null;
  const openBangumiSubject = (subjectId: number) => api.openExternal(`https://bgm.tv/subject/${subjectId}`);
  const showExtras = extrasReady && extras != null && (rating?.score != null || tags.length > 0 || staff.length > 0 || characters.length > 0 || related.length > 0);

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="detail-panel" onMouseDown={(event) => event.stopPropagation()} aria-modal="true" role="dialog">
        <button className="close-button" onClick={onClose} title={t('关闭', 'Close')}><X size={20} /></button>
        <div className="detail-banner" style={anime.bannerImage ? { backgroundImage: `url(${anime.bannerImage})` } : undefined} />
        <div className="detail-content">
          <img className="detail-cover" src={anime.coverImage?.extraLarge || anime.coverImage?.medium} alt="" />
          <div className="detail-main">
            <div className="eyebrow">{formatLabel(anime.format, language)} · {anime.episodes ? t(`${anime.episodes} 集`, `${anime.episodes} episodes`) : t('集数待定', 'Episodes TBA')}</div>
            <h2>{displayTitle}</h2>
            {originalTitle !== displayTitle && <p className="detail-alt-title">{originalTitle}</p>}
            <div className="detail-stats">
              <span><strong>{anime.averageScore || '—'}</strong> {t('评分', 'score')}</span>
              <span><strong>{anime.duration || '—'}</strong> {t('分钟', 'min')}</span>
              <span><strong>{anime.studios?.nodes[0]?.name || t('待定', 'TBA')}</strong> {t('制作', 'studio')}</span>
            </div>
            <p className="description">{stripDescription(anime.description, language)}</p>
            <div className="genre-list">{anime.genres?.map((genre) => <span key={genre}>{genre}</span>)}</div>
            {showExtras && extras && (
              <div className="bangumi-extras" style={{ display: 'grid', gap: 12, margin: '14px 0', borderTop: '1px solid rgba(128,128,128,0.25)', paddingTop: 14 }}>
                {rating?.score != null && (
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <Star size={16} />
                    <span>
                      <strong>{rating.score}</strong>/10
                      {rating.total != null ? t(` · ${rating.total} 人评分`, ` · ${rating.total} ratings`) : ''}
                      {rating.rank != null ? t(` · 排名 #${rating.rank}`, ` · rank #${rating.rank}`) : ''}
                    </span>
                  </div>
                )}
                {tags.length > 0 && (
                  <div className="genre-list" aria-label={t('Bangumi 标签', 'Bangumi tags')}>
                    {tags.map((tag) => <span key={tag.name}>{tag.name}<small style={{ opacity: 0.6 }}> {tag.count}</small></span>)}
                  </div>
                )}
                {staff.length > 0 && (
                  <div>
                    <div style={{ fontSize: 13, fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6, marginBottom: 6 }}><ScrollText size={14} /> {t('制作人员', 'Staff')}</div>
                    {staff.map((row) => (
                      <div key={`${row.key}-${row.value}`} style={{ display: 'flex', gap: 8, fontSize: 13 }}>
                        <span style={{ opacity: 0.6, flexShrink: 0 }}>{row.key}</span>
                        <span>{row.value}</span>
                      </div>
                    ))}
                  </div>
                )}
                {characters.length > 0 && (
                  <div>
                    <div style={{ fontSize: 13, fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6, marginBottom: 6 }}><Users size={14} /> {t('角色', 'Characters')}</div>
                    <div style={{ display: 'flex', gap: 12, overflowX: 'auto', paddingBottom: 4 }}>
                      {characters.map((character) => (
                        <div key={character.id} style={{ flexShrink: 0, width: 88, textAlign: 'center' }}>
                          {character.imageUrl
                            ? <img src={character.imageUrl} alt="" loading="lazy" className="bangumi-character" style={{ width: 64, height: 64, borderRadius: '50%' }} />
                            : <span style={{ display: 'inline-block', width: 64, height: 64, borderRadius: '50%', background: 'rgba(128,128,128,0.25)' }} />}
                          <div style={{ fontSize: 12, marginTop: 4 }}>{character.nameCn || character.name}</div>
                          <div style={{ fontSize: 11, opacity: 0.6 }}>{character.relation}</div>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
                {related.length > 0 && (
                  <div>
                    <div style={{ fontSize: 13, fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6, marginBottom: 6 }}><Link2 size={14} /> {t('关联条目', 'Related subjects')}</div>
                    {related.map((entry) => (
                      <button
                        key={entry.id}
                        onClick={() => openBangumiSubject(entry.id)}
                        title={t(`在 Bangumi 打开《${entry.nameCn || entry.name}》`, `Open “${entry.nameCn || entry.name}” on Bangumi`)}
                        style={{ display: 'flex', gap: 8, alignItems: 'baseline', fontSize: 13, background: 'transparent', border: 'none', color: 'inherit', cursor: 'pointer', padding: '2px 0', textAlign: 'left' }}
                      >
                        <span>{entry.nameCn || entry.name}</span>
                        <span style={{ opacity: 0.6, fontSize: 12 }}>{entry.relation}</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}
            {extrasLoading && <p style={{ fontSize: 12, opacity: 0.55 }}>{t('正在读取 Bangumi 条目信息…', 'Loading Bangumi subject info…')}</p>}
            {anime.nextAiringEpisode && (
              <div className="next-airing">
                <Clock3 size={19} />
                <div><strong>{t(`第 ${anime.nextAiringEpisode.episode} 集`, `Episode ${anime.nextAiringEpisode.episode}`)}</strong><span>{formatAiring(anime.nextAiringEpisode.airingAt, true, language, anime.nextAiringEpisode.airingPrecision)} · {relativeTime(anime.nextAiringEpisode.airingAt, language, anime.nextAiringEpisode.airingPrecision)}</span></div>
              </div>
            )}
            <div className="detail-actions">
              <button className={`primary-button ${followed ? 'subtle' : ''}`} onClick={onToggle}>
                {followed ? <Check size={18} /> : <Bell size={18} />}{followed ? t('已加入追番', 'Following') : t('加入追番', 'Follow')}
              </button>
              {(anime.siteUrl || bangumiSubjectId != null) && (
                <button className="secondary-button" onClick={() => (anime.siteUrl ? api.openExternal(anime.siteUrl) : openBangumiSubject(bangumiSubjectId!))}>
                  <ExternalLink size={17} /> {bangumiSubjectId != null ? t('在 Bangumi 打开', 'Open on Bangumi') : t('AniList 页面', 'Open on AniList')}
                </button>
              )}
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

function TasksView({ tasks, language, onToggle, onResolveReview }: {
  tasks: WatchTask[]; language: UiLanguage; onToggle: (id: string) => Promise<void>;
  onResolveReview?: (id: string, keepCompleted: boolean) => Promise<void>;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const [filter, setFilter] = useState<'pending' | 'completed' | 'review' | 'all'>('pending');
  const reviews = tasks.filter(needsHistoryReview).length;
  const visible = tasks.filter((task) => filter === 'all' || (filter === 'review' ? needsHistoryReview(task)
    : filter === 'completed' ? isCompletedHistory(task) : isPendingHistory(task)));
  const pending = tasks.filter(isPendingHistory).length;
  const completed = tasks.filter(isCompletedHistory).length;

  return (
    <>
      <section className="task-summary">
        <div><span>{t('待观看', 'To watch')}</span><strong>{pending}</strong><small>{t('播出后自动加入', 'Added after airing')}</small></div>
        <div><span>{t('已看完', 'Completed')}</span><strong>{completed}</strong><small>{t('保留观看记录', 'Watch history kept')}</small></div>
        <div><span>{t('完成率', 'Completion')}</span><strong>{pending + completed ? Math.round((completed / (pending + completed)) * 100) : 0}%</strong><small>{t('当前任务清单', 'Current task list')}</small></div>
      </section>
      <section className="section-heading compact">
        <div><div className="eyebrow"><ListChecks size={14} /> {t('每集任务', 'Episode tasks')}</div><h2>{t('观看清单', 'Watch list')}</h2><p>{t('勾选一集，任务即归档到已完成。', 'Check off an episode to archive it as completed.')}</p></div>
        <div className="segmented-control task-tabs">
          <button className={filter === 'pending' ? 'selected' : ''} onClick={() => setFilter('pending')}>{t('待看', 'Pending')} {pending}</button>
          <button className={filter === 'completed' ? 'selected' : ''} onClick={() => setFilter('completed')}>{t('已看', 'Completed')} {completed}</button>
          {(reviews > 0 || filter === 'review') && <button className={filter === 'review' ? 'selected' : ''} onClick={() => setFilter('review')}>{t('待核对', 'Review')} {reviews}</button>}
          <button className={filter === 'all' ? 'selected' : ''} onClick={() => setFilter('all')}>{t('全部', 'All')}</button>
        </div>
      </section>
      {visible.length === 0 ? (
        <EmptyState icon={CheckCircle2} title={filter === 'pending' ? t('待看清单已清空', 'No pending tasks') : t('这里还没有观看记录', 'No watch history yet')} body={filter === 'pending' ? t('追番更新后，每集会自动出现在这里。', 'New episodes will appear here after they air.') : t('看完一集并勾选后会保存在这里。', 'Completed episodes will be kept here.')} />
      ) : (
        <div className="task-list">
          {visible.map((task) => <TaskRow key={task.id} task={task} language={language}
            onToggle={() => onToggle(task.id)}
            onResolveReview={onResolveReview ? (keep) => onResolveReview(task.id, keep) : undefined} />)}
        </div>
      )}
    </>
  );
}

function TaskRow({ task, language, onToggle, onResolveReview }: {
  task: WatchTask; language: UiLanguage; onToggle: () => void;
  onResolveReview?: (keepCompleted: boolean) => Promise<void>;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const review = needsHistoryReview(task);
  const scheduled = isScheduledHistoryReset(task);
  const completed = isCompletedHistory(task);
  const [busy, setBusy] = useState(false);
  const resolve = async (keep: boolean) => {
    if (!onResolveReview || busy) return;
    const message = keep
      ? t('确认确实已看完这集？确认后计入进度，启用观看进度同步时会写回账户。', 'Confirm that you watched this episode? It will count toward progress and sync when progress upload is enabled.')
      : t('撤销这条完成记录？原记录留存，实际播出后进入待看；启用进度同步时会撤销账户中对应集的已看状态。', 'Reset this completion? The original record is retained and the episode becomes pending after airing. Its watched status will be reset when progress upload is enabled.');
    if (!window.confirm(message)) return;
    setBusy(true);
    try { await onResolveReview(keep); } finally { setBusy(false); }
  };
  return (
    <article className={`task-row ${completed ? 'completed' : ''} ${review ? 'history-review' : ''}`}>
      <button className="task-check" disabled={review || scheduled || busy}
        title={review ? t('完成记录待核对', 'Completion needs review') : scheduled ? t('尚未播出', 'Not aired yet') : completed ? t('恢复为待看', 'Restore as pending') : t('标记为已看', 'Mark as watched')} onClick={onToggle}>
        {review ? <AlertTriangle size={23} /> : scheduled ? <Clock3 size={23} /> : completed ? <CheckCircle2 size={23} /> : <Circle size={23} />}
      </button>
      {task.coverImage ? <img src={task.coverImage} alt="" /> : <span className="cover-placeholder" />}
      <div className="task-copy"><strong>{task.animeTitle}</strong><span>{t(`第 ${task.episode} 集`, `Episode ${task.episode}`)}</span>
        {review && <small>{t('完成记录早于播出，未计入进度', 'Completion predates airing; excluded from progress')}</small>}
      </div>
      <div className="task-time"><Clock3 size={15} /><span>{formatAiring(task.airingAt, true, language, task.airingPrecision)}</span></div>
      {review && onResolveReview ? (
        <div className="history-review-actions">
          <button className="icon-button" disabled={busy} title={t('确认已看', 'Confirm watched')} aria-label={t('确认已看', 'Confirm watched')} onClick={() => void resolve(true)}><Check size={17} /></button>
          <button className="icon-button" disabled={busy} title={t('撤销异常完成', 'Reset completion')} aria-label={t('撤销异常完成', 'Reset completion')} onClick={() => void resolve(false)}><Undo2 size={17} /></button>
        </div>
      ) : <span className="task-state">{review ? t('待核对', 'Needs review') : scheduled ? t('未播出', 'Unaired') : completed ? t('已看完', 'Completed') : t('待观看', 'To watch')}</span>}
    </article>
  );
}

function FollowingView({
  items,
  language,
  onUnfollow,
  onOpenTasks,
  onRename,
  onConfirmMapping,
  onSkipMapping,
  onSetRating,
  onSetCollectionStatus,
}: {
  items: AppState['following'];
  language: UiLanguage;
  onUnfollow: (id: number) => void;
  onOpenTasks: () => void;
  onRename: (id: number, displayTitle: string) => Promise<void>;
  onConfirmMapping: (animeId: number, subjectId: number) => Promise<void>;
  onSkipMapping: (animeId: number) => Promise<void>;
  onSetRating: (subjectId: number, rating: number | null) => Promise<void>;
  onSetCollectionStatus: (subjectId: number, status: BangumiCollectionStatus) => Promise<void>;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [draftTitle, setDraftTitle] = useState('');
  const [mappingDialogFor, setMappingDialogFor] = useState<number | null>(null);
  const [mappingBusy, setMappingBusy] = useState(false);
  const [ratingBusyId, setRatingBusyId] = useState<number | null>(null);
  const [statusBusyId, setStatusBusyId] = useState<number | null>(null);
  const [resolutions, setResolutions] = useState<Record<number, BangumiMappingResolution>>({});
  const requestedMappings = useRef(new Set<number>());
  // 问题 4：状态分组折叠（在看默认展开，其余默认收起；跨会话记忆在 localStorage）。
  const [collapsedGroups, setCollapsedGroups] = useState<Record<FollowingGroupKey, boolean>>(loadCollapsedGroups);
  const pendingItems = items.filter((item) => item.mappingPending === true);

  useEffect(() => {
    try {
      localStorage.setItem(FOLLOW_GROUPS_KEY, JSON.stringify(collapsedGroups));
    } catch { /* 存储不可用时忽略，仅影响跨会话记忆 */ }
  }, [collapsedGroups]);

  const toggleGroupCollapse = (key: FollowingGroupKey) => {
    setCollapsedGroups((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  // Phase 3：Bangumi 条目的行内评分变更；subjectId 取 bangumiId，Bangumi 来源条目回落主键 id。
  const handleSetRating = async (item: FollowedAnime, subjectId: number, value: string) => {
    setRatingBusyId(item.id);
    try {
      await onSetRating(subjectId, value === '' ? null : Number(value));
    } finally {
      setRatingBusyId((current) => (current === item.id ? null : current));
    }
  };

  // 状态驱动追踪：行内状态变更；dropped（抛弃追番）的确认与后端调用由 App 层的 onSetCollectionStatus 处理。
  const handleSetStatus = async (item: FollowedAnime, subjectId: number, status: BangumiCollectionStatus) => {
    setStatusBusyId(item.id);
    try {
      await onSetCollectionStatus(subjectId, status);
    } finally {
      setStatusBusyId((current) => (current === item.id ? null : current));
    }
  };

  // 挂载时对每个待确认条目惰性解析候选；结果缓存在组件 state，确认/跳过后清除。
  useEffect(() => {
    if (IS_ORIGINAL_EDITION || !api.bangumiResolveMapping) return;
    pendingItems.forEach((item) => {
      if (requestedMappings.current.has(item.id) || resolutions[item.id]) return;
      requestedMappings.current.add(item.id);
      api.bangumiResolveMapping!({ animeId: item.id })
        .then((resolution) => setResolutions((prev) => ({ ...prev, [item.id]: resolution })))
        .catch(() => setResolutions((prev) => ({
          ...prev,
          [item.id]: { status: 'unavailable', subjectId: null, candidates: [], anime: { id: item.id, displayTitle: item.displayTitle, seasonYear: null, format: item.format ?? null, coverImage: item.coverImage } },
        })));
    });
  }, [pendingItems, resolutions]);

  const clearMapping = (animeId: number) => {
    setResolutions((prev) => {
      const next = { ...prev };
      delete next[animeId];
      return next;
    });
    requestedMappings.current.delete(animeId);
    setMappingDialogFor(null);
  };

  const handleConfirmMapping = async (animeId: number, subjectId: number) => {
    setMappingBusy(true);
    try {
      await onConfirmMapping(animeId, subjectId);
      clearMapping(animeId);
    } finally {
      setMappingBusy(false);
    }
  };

  const handleSkipMapping = async (animeId: number) => {
    setMappingBusy(true);
    try {
      await onSkipMapping(animeId);
      clearMapping(animeId);
    } finally {
      setMappingBusy(false);
    }
  };

  const dialogItem = mappingDialogFor != null ? items.find((item) => item.id === mappingDialogFor) || null : null;
  const dialogResolution = mappingDialogFor != null ? resolutions[mappingDialogFor] || null : null;

  // 状态分组：在看（含 bangumiStatus 为空的旧条目）→ 想看 → 搁置 → 看完；空组不显示。
  const groups = FOLLOWING_GROUP_ORDER
    .map(({ key, label }) => ({
      key,
      label,
      items: items.filter((item) => followingGroupOf(item) === key)
        .sort((a, b) => (a.nextAiringEpisode?.airingAt || Infinity) - (b.nextAiringEpisode?.airingAt || Infinity)),
    }))
    .filter((group) => group.items.length > 0);

  // 问题 1：追踪中 = bangumiStatus 为空或 doing（wish/on_hold/done 不自动建任务；dropped 本就不在列表）。
  const trackingCount = items.filter((item) => followingGroupOf(item) === 'doing').length;

  const renderFollowingRow = (item: FollowedAnime) => {
    // Phase 3：standard 下 Bangumi 条目（source='bangumi' 或带 bangumiId）提供行内评分、状态徽章与状态下拉。
    const bangumiSubjectId = !IS_ORIGINAL_EDITION && (item.source === 'bangumi' || typeof item.bangumiId === 'number')
      ? (typeof item.bangumiId === 'number' ? item.bangumiId : item.id)
      : null;
    const statusBadge = item.bangumiStatus ? BANGUMI_STATUS_LABELS[item.bangumiStatus] : undefined;
    return (
    <article className="following-row" key={item.id}>
      <img src={item.coverImage} alt="" />
      <div className="following-copy">
        <span>
          {formatLabel(item.format, language)} · {t('通知与任务标题', 'Notification and task title')}
          {item.mappingPending === true && (
            <button
              className="mapping-badge"
              style={{ marginLeft: 8, border: '1px solid currentColor', borderRadius: 999, padding: '1px 9px', fontSize: 12, background: 'transparent', color: 'inherit', cursor: 'pointer' }}
              title={t('确认 Bangumi 条目映射', 'Confirm the Bangumi subject mapping')}
              onClick={() => setMappingDialogFor(item.id)}
            >
              {t('待确认映射', 'Mapping pending')}
            </button>
          )}
          {statusBadge && (
            <span
              style={{ marginLeft: 8, border: '1px solid currentColor', borderRadius: 999, padding: '1px 9px', fontSize: 12, opacity: 0.75 }}
              title={t('Bangumi 收藏状态', 'Bangumi collection status')}
            >
              {t(...statusBadge)}
            </span>
          )}
        </span>
        {editingId === item.id ? (
          <div className="title-editor">
            <input
              value={draftTitle}
              onChange={(event) => setDraftTitle(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Escape') setEditingId(null);
                if (event.key === 'Enter' && draftTitle.trim()) {
                  void onRename(item.id, draftTitle).then(() => setEditingId(null));
                }
              }}
              aria-label={t(`${item.displayTitle} 的提醒标题`, `Alert title for ${item.displayTitle}`)}
              placeholder={t('输入提醒标题', 'Enter alert title')}
              autoFocus
            />
            <button
              title={t('保存提醒名', 'Save alert title')}
              disabled={!draftTitle.trim()}
              onClick={() => void onRename(item.id, draftTitle).then(() => setEditingId(null))}
            ><Check size={16} /></button>
            <button title={t('取消修改', 'Cancel editing')} onClick={() => setEditingId(null)}><X size={16} /></button>
          </div>
        ) : (
          <div className="following-name">
            <strong>{item.displayTitle}</strong>
            <button
              title={t('修改提醒标题', 'Edit alert title')}
              onClick={() => { setEditingId(item.id); setDraftTitle(item.displayTitle); }}
            ><Pencil size={14} /></button>
          </div>
        )}
        <small>{titleOf(item.title, language) !== item.displayTitle ? `${titleOf(item.title, language)} · ` : ''}{item.episodes ? t(`全 ${item.episodes} 集`, `${item.episodes} episodes`) : t('总集数待定', 'Episode count TBA')}</small>
        {bangumiSubjectId != null && (
          <div className="following-controls">
            <label>
              <select
                className="entry-select status-select"
                value={item.bangumiStatus || 'doing'}
                disabled={statusBusyId === item.id}
                aria-label={t(`调整 ${item.displayTitle} 的状态`, `Set status for ${item.displayTitle}`)}
                onChange={(event) => void handleSetStatus(item, bangumiSubjectId, event.target.value as BangumiCollectionStatus)}
              >
                {BANGUMI_STATUS_OPTIONS.map((option) => <option key={option.value} value={option.value}>{t(...option.label)}</option>)}
              </select>
            </label>
            <label>
              <Star size={13} />
              <select
                className="entry-select rating-select"
                value={item.rating == null ? '' : String(item.rating)}
                disabled={ratingBusyId === item.id}
                aria-label={t(`评分 ${item.displayTitle}`, `Rate ${item.displayTitle}`)}
                onChange={(event) => void handleSetRating(item, bangumiSubjectId, event.target.value)}
              >
                <option value="">{t('未评分', 'Unrated')}</option>
                {BANGUMI_RATING_OPTIONS.map((value) => <option key={value} value={value}>{value}</option>)}
              </select>
            </label>
            {item.watchedEpisode != null && (
              <span className="following-progress">
                {t(`进度 ${item.watchedEpisode}${item.episodes ? ` / ${item.episodes}` : ''}`, `Progress ${item.watchedEpisode}${item.episodes ? ` / ${item.episodes}` : ''}`)}
              </span>
            )}
          </div>
        )}
      </div>
      <div className="following-next">
        <small>{t('下次更新', 'Next episode')}</small>
        <strong>{item.nextAiringEpisode ? t(`第 ${item.nextAiringEpisode.episode} 集`, `Episode ${item.nextAiringEpisode.episode}`) : t('暂无日程', 'No schedule')}</strong>
        <span>{item.nextAiringEpisode ? `${formatAiring(item.nextAiringEpisode.airingAt, true, language, item.nextAiringEpisode.airingPrecision)} · ${relativeTime(item.nextAiringEpisode.airingAt, language, item.nextAiringEpisode.airingPrecision)}` : item.source === 'bangumi' ? t('等待 Bangumi 日程', 'Waiting for Bangumi') : t('等待 AniList 公布', 'Waiting for AniList')}</span>
      </div>
      <button className="icon-button danger" title={t('取消追番', 'Unfollow')} aria-label={t(`取消追番 ${item.displayTitle}`, `Unfollow ${item.displayTitle}`)} onClick={() => onUnfollow(item.id)}><Minus size={19} /></button>
    </article>
    );
  };

  return (
    <>
      <section className="section-heading compact">
        {/* 问题 1：头部计数只算「追踪中」条目（bangumiStatus 为空或 doing；wish/on_hold/done 不自动建任务）。 */}
        <div><div className="eyebrow"><BellRing size={14} /> {t('自动跟踪', 'Automatic tracking')}</div><h2>{t('正在追的番剧', 'Currently following')}</h2><p>{t(`${trackingCount} 部作品会在播出后自动创建观看任务。`, `${trackingCount} actively tracked title${trackingCount === 1 ? '' : 's'} will create watch tasks after airing.`)}</p></div>
        <button className="secondary-button" onClick={onOpenTasks}><ListChecks size={17} /> {t('查看任务', 'View tasks')}</button>
      </section>
      {items.length === 0 || groups.length === 0 ? (
        <EmptyState icon={Bell} title={t('还没有添加追番', 'Nothing followed yet')} body={t('到季度新番中选择作品，更新提醒会自动开启。', 'Choose a title from Seasonal Anime to enable update alerts.')} />
      ) : (
        groups.map((group) => {
          const collapsed = collapsedGroups[group.key];
          return (
            <section key={group.key} className="following-group" aria-label={t(...group.label)}>
              <header className="following-group-heading">
                <button
                  type="button"
                  className="following-group-toggle"
                  aria-expanded={!collapsed}
                  aria-label={t(`折叠或展开 ${t(...group.label)} 分组`, `Collapse or expand ${t(...group.label)} group`)}
                  onClick={() => toggleGroupCollapse(group.key)}
                >
                  <ChevronDown size={15} className={collapsed ? 'following-group-chevron collapsed' : 'following-group-chevron'} />
                  <span className="following-group-title">{t(...group.label)}</span>
                </button>
                <span className="following-group-count">{t(`${group.items.length} 部`, `${group.items.length} title${group.items.length === 1 ? '' : 's'}`)}</span>
              </header>
              {!collapsed && (
                <div className="following-list">
                  {group.items.map(renderFollowingRow)}
                </div>
              )}
            </section>
          );
        })
      )}
      {dialogItem && (
        <MappingDialog
          item={dialogItem}
          resolution={dialogResolution}
          language={language}
          busy={mappingBusy}
          onClose={() => setMappingDialogFor(null)}
          onConfirm={(subjectId) => handleConfirmMapping(dialogItem.id, subjectId)}
          onSkip={() => handleSkipMapping(dialogItem.id)}
        />
      )}
    </>
  );
}

function MappingDialog({
  item,
  resolution,
  language,
  busy,
  onClose,
  onConfirm,
  onSkip,
}: {
  item: FollowedAnime;
  resolution: BangumiMappingResolution | null;
  language: UiLanguage;
  busy: boolean;
  onClose: () => void;
  onConfirm: (subjectId: number) => void;
  onSkip: () => void;
}) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  // 优先使用解析结果里的旧作品信息；失败时回落到追番条目本身。
  const info = resolution?.anime || { id: item.id, displayTitle: item.displayTitle, seasonYear: null as number | null, format: item.format ?? null, coverImage: item.coverImage };
  const candidates = resolution?.candidates || [];
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="detail-panel" onMouseDown={(event) => event.stopPropagation()} aria-modal="true" role="dialog">
        <button className="close-button" onClick={onClose} title={t('关闭', 'Close')}><X size={20} /></button>
        <div className="detail-content">
          <div className="detail-main">
            <div className="eyebrow"><BellRing size={14} /> {t('映射确认', 'Mapping confirmation')}</div>
            <h2>{t(`将《${info.displayTitle}》关联到 Bangumi 条目`, `Link “${info.displayTitle}” to a Bangumi subject`)}</h2>
            <div className="mapping-source" style={{ display: 'flex', gap: 12, alignItems: 'center', margin: '12px 0' }}>
              <img src={info.coverImage} alt="" style={{ width: 56, height: 56, borderRadius: 8, objectFit: 'cover' }} />
              <div>
                <strong>{info.displayTitle}</strong>
                <div style={{ fontSize: 13, opacity: 0.75 }}>{formatLabel(info.format, language)} · {info.seasonYear || t('年份待定', 'Year TBA')}</div>
              </div>
            </div>
            {resolution?.status === 'unavailable' ? (
              <p style={{ opacity: 0.75 }}>{t('当前无法读取 Bangumi 候选条目，可稍后重试。', 'Bangumi candidates are unavailable right now. Try again later.')}</p>
            ) : candidates.length > 0 ? (
              <div className="mapping-candidates" style={{ display: 'grid', gap: 10 }}>
                {candidates.map((candidate) => (
                  <div key={candidate.subjectId} className="mapping-candidate" style={{ display: 'flex', gap: 12, alignItems: 'center', justifyContent: 'space-between', border: '1px solid rgba(128,128,128,0.35)', borderRadius: 10, padding: '10px 12px' }}>
                    <div>
                      <strong>{candidate.nameCn || candidate.name}</strong>
                      <div style={{ fontSize: 12, opacity: 0.75 }}>
                        {candidate.nameCn && candidate.nameCn !== candidate.name ? `${candidate.name} · ` : ''}
                        {candidate.date || t('日期待定', 'Date TBA')}
                        {typeof candidate.score === 'number' && Number.isFinite(candidate.score) ? ` · ${t(`匹配度 ${candidate.score}`, `match ${candidate.score}`)}` : ''}
                      </div>
                    </div>
                    <button className="primary-button" disabled={busy} onClick={() => onConfirm(candidate.subjectId)}>
                      <Check size={16} /> {t('确认映射', 'Confirm mapping')}
                    </button>
                  </div>
                ))}
              </div>
            ) : (
              <p style={{ opacity: 0.75 }}>{resolution ? t('暂无候选 Bangumi 条目，可以稍后再试。', 'No Bangumi candidates yet. You can decide later.') : t('正在读取候选条目…', 'Loading candidates…')}</p>
            )}
            <div className="detail-actions">
              <button className="secondary-button" disabled={busy || !resolution} onClick={onSkip}>
                <Clock3 size={16} /> {t('这些都不对，以后再说', 'None of these — decide later')}
              </button>
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

function SettingsView({ state, language, onChange, onApplyState }: { state: AppState; language: UiLanguage; onChange: (patch: Partial<AppSettings>) => Promise<void>; onApplyState: (state: AppState) => void }) {
  const t = (chinese: string, english: string) => tr(language, chinese, english);
  const message = (reason: unknown, chinese: string, english: string) => reason instanceof Error ? localizeMessage(reason.message, language) : t(chinese, english);
  const isAndroid = state.runtime?.platform === 'android';
  const isTauriDesktop = IS_TAURI_APP && state.runtime?.isDesktop === true && !isAndroid;
  const [proxyUrl, setProxyUrl] = useState(state.settings.bangumiApiBaseUrl);
  const [proxyStatus, setProxyStatus] = useState('');
  const [testingProxy, setTestingProxy] = useState(false);
  const [cacheBytes, setCacheBytes] = useState<number | null>(null);
  const [cacheSupported, setCacheSupported] = useState<boolean | null>(null);
  const [cacheStatus, setCacheStatus] = useState('');
  const [clearingCache, setClearingCache] = useState(false);
  const [webDavConfig, setWebDavConfig] = useState<WebDavConfig | null>(null);
  const [webDavUrl, setWebDavUrl] = useState('');
  const [webDavUsername, setWebDavUsername] = useState('');
  const [webDavPassword, setWebDavPassword] = useState('');
  const [webDavStatus, setWebDavStatus] = useState('');
  const [webDavBusy, setWebDavBusy] = useState(false);
  const [bangumiAuth, setBangumiAuth] = useState<BangumiAuthStatus | null>(null);
  const [bangumiProfile, setBangumiProfile] = useState<BangumiUserProfile | null>(null);
  const [bangumiToken, setBangumiToken] = useState('');
  const [bangumiApiUrl, setBangumiApiUrl] = useState(state.bangumiSyncSettings?.apiBaseUrl || state.settings.bangumiApiBaseUrl);
  const [bangumiStatus, setBangumiStatus] = useState('');
  const [bangumiBusy, setBangumiBusy] = useState(false);
  const [syncBusy, setSyncBusy] = useState(false);
  const [syncReport, setSyncReport] = useState<BangumiSyncReport | null>(null);
  const [showSuggestions, setShowSuggestions] = useState(false);
  const bangumiSyncSettings = state.bangumiSyncSettings;
  // 问题 5：standard 版状态行追加「Bangumi 上次同步」（秒级时间戳，过去向语义用 relativePastTime）。
  const lastBangumiSyncAt = state.bangumiSyncStatus?.lastBangumiSyncAt ?? null;

  useEffect(() => setProxyUrl(state.settings.bangumiApiBaseUrl), [state.settings.bangumiApiBaseUrl]);
  useEffect(() => setBangumiApiUrl(state.bangumiSyncSettings?.apiBaseUrl || state.settings.bangumiApiBaseUrl), [state.bangumiSyncSettings?.apiBaseUrl, state.settings.bangumiApiBaseUrl]);

  useEffect(() => {
    let active = true;
    api.getCacheInfo()
      .then((info) => {
        if (!active) return;
        setCacheSupported(info.supported);
        setCacheBytes(info.supported ? info.bytes : null);
      })
      .catch((reason) => { if (active) setCacheStatus(message(reason, '无法读取缓存大小', 'Could not calculate cache size')); });
    return () => { active = false; };
  }, [language]);

  useEffect(() => {
    let active = true;
    api.getWebDavConfig()
      .then((config) => {
        if (!active) return;
        setWebDavConfig(config);
        setWebDavUrl(config.baseUrl);
        setWebDavUsername(config.username);
        setWebDavStatus(config.lastError ? localizeMessage(config.lastError, language) : '');
      })
      .catch((reason) => { if (active) setWebDavStatus(message(reason, '无法读取 WebDAV 设置', 'Could not read WebDAV settings')); });
    return () => { active = false; };
  }, [language]);

  const loadBangumiStatus = async () => {
    if (!api.bangumiAuthStatus) return;
    const status = await api.bangumiAuthStatus();
    setBangumiAuth(status);
    setBangumiProfile(status.hasToken && api.bangumiGetUserProfile ? await api.bangumiGetUserProfile() : null);
  };

  useEffect(() => {
    if (!api.bangumiAuthStatus) return;
    let active = true;
    loadBangumiStatus().catch((reason) => { if (active) setBangumiStatus(message(reason, '无法读取 Bangumi 连接状态', 'Could not read Bangumi connection status')); });
    return () => { active = false; };
  }, [language]);

  const saveBangumiToken = async () => {
    if (!api.bangumiSaveToken || !api.bangumiTestConnection) return;
    setBangumiBusy(true);
    setBangumiStatus(t('正在保存…', 'Saving…'));
    try {
      const saved = await api.bangumiSaveToken({ token: bangumiToken.trim() });
      if (!saved.ok) {
        setBangumiStatus(localizeMessage(saved.message, language));
        return;
      }
      setBangumiToken('');
      const result = await api.bangumiTestConnection({ baseUrl: bangumiApiUrl.trim() || null });
      setBangumiStatus(localizeMessage(result.message, language));
      await loadBangumiStatus();
    } catch (reason) {
      setBangumiStatus(message(reason, 'Bangumi 连接失败', 'Bangumi connection failed'));
    } finally {
      setBangumiBusy(false);
    }
  };

  const saveBangumiApiUrl = async () => {
    const next = bangumiApiUrl.trim();
    const current = state.bangumiSyncSettings?.apiBaseUrl || state.settings.bangumiApiBaseUrl || '';
    if (next === current.trim()) return;
    try {
      await onChange({ bangumiApiBaseUrl: next });
      setBangumiStatus(next ? t('API 地址已保存', 'API address saved') : t('已恢复使用官方 API', 'Restored to the official API'));
    } catch (reason) {
      setBangumiStatus(message(reason, 'API 地址无效', 'Invalid API address'));
    }
  };

  const disconnectBangumi = async () => {
    if (!api.bangumiDisconnect) return;
    if (!window.confirm(t('确认断开 Bangumi 账户连接吗？\n\n本地观看记录与追番清单会保留。', 'Disconnect the Bangumi account?\n\nLocal watch history and the following list are kept.'))) return;
    setBangumiBusy(true);
    setBangumiStatus(t('正在断开…', 'Disconnecting…'));
    try {
      const result = await api.bangumiDisconnect();
      setBangumiStatus(localizeMessage(result.message, language));
      await loadBangumiStatus();
    } catch (reason) {
      setBangumiStatus(message(reason, 'Bangumi 断开失败', 'Could not disconnect Bangumi'));
    } finally {
      setBangumiBusy(false);
    }
  };

  // Phase 3：Bangumi 收藏/评分/进度同步设置；任一切换即调用后端并用返回的 AppState 刷新。
  const updateBangumiSyncSettings = async (patch: BangumiSyncSettingsPatch) => {
    if (patch.pushLocalChanges === true && !window.confirm(t(
      '开启后，本地追番/取消追番/评分变化将写入你的 Bangumi 账户。确认开启吗？',
      'Once enabled, local follow/unfollow/rating changes will be written to your Bangumi account. Enable it?',
    ))) return;
    if (!api.bangumiUpdateSyncSettings) return;
    setBangumiBusy(true);
    try {
      onApplyState(await api.bangumiUpdateSyncSettings(patch));
      setBangumiStatus(t('同步设置已保存', 'Sync settings saved'));
    } catch (reason) {
      setBangumiStatus(message(reason, 'Bangumi 同步设置保存失败', 'Could not save Bangumi sync settings'));
    } finally {
      setBangumiBusy(false);
    }
  };

  const runBangumiSync = async () => {
    if (!api.bangumiSyncNow) return;
    setSyncBusy(true);
    setBangumiStatus(t('正在同步 Bangumi…', 'Syncing Bangumi…'));
    try {
      const result = await api.bangumiSyncNow();
      onApplyState(await api.getState());
      setSyncReport(result.report);
      setBangumiStatus(localizeMessage(result.message, language));
    } catch (reason) {
      setBangumiStatus(message(reason, 'Bangumi 同步失败', 'Bangumi sync failed'));
    } finally {
      setSyncBusy(false);
    }
  };

  const webDavPayload = (enabled: boolean) => ({    enabled,
    baseUrl: webDavUrl,
    username: webDavUsername,
    ...(webDavPassword ? { password: webDavPassword } : {}),
  });

  const saveWebDav = async (enabled = webDavConfig?.enabled || false) => {
    setWebDavBusy(true);
    setWebDavStatus(t('正在保存…', 'Saving…'));
    try {
      const saved = await api.saveWebDavConfig(webDavPayload(enabled));
      setWebDavConfig(saved);
      setWebDavPassword('');
      setWebDavStatus(t('WebDAV 设置已保存', 'WebDAV settings saved'));
      return saved;
    } catch (reason) {
      setWebDavStatus(message(reason, 'WebDAV 设置保存失败', 'Could not save WebDAV settings'));
      return null;
    } finally {
      setWebDavBusy(false);
    }
  };

  const testWebDav = async () => {
    setWebDavBusy(true);
    setWebDavStatus(t('正在测试连接…', 'Testing connection…'));
    try {
      const saved = await api.saveWebDavConfig(webDavPayload(webDavConfig?.enabled || false));
      setWebDavConfig(saved);
      setWebDavPassword('');
      const result = await api.testWebDavConnection();
      setWebDavStatus(localizeMessage(result.message, language));
    } catch (reason) {
      setWebDavStatus(message(reason, 'WebDAV 连接失败', 'WebDAV connection failed'));
    } finally {
      setWebDavBusy(false);
    }
  };

  const syncWebDav = async () => {
    setWebDavBusy(true);
    setWebDavStatus(t('正在同步…', 'Syncing…'));
    try {
      const result = await api.syncWebDav();
      setWebDavConfig(await api.getWebDavConfig());
      setWebDavStatus(localizeMessage(result.message, language));
    } catch (reason) {
      setWebDavStatus(message(reason, 'WebDAV 同步失败', 'WebDAV sync failed'));
    } finally {
      setWebDavBusy(false);
    }
  };

  const clearCache = async () => {
    setClearingCache(true);
    setCacheStatus('');
    try {
      const before = cacheBytes || 0;
      const info = await api.clearCache();
      setCacheSupported(info.supported);
      setCacheBytes(info.supported ? info.bytes : null);
      setCacheStatus(before > info.bytes ? t(`已清理 ${formatStorageSize(before - info.bytes)}`, `Cleared ${formatStorageSize(before - info.bytes)}`) : t('当前没有可清理缓存', 'No cache to clear'));
    } catch (reason) {
      setCacheStatus(message(reason, '清理缓存失败', 'Could not clear cache'));
    } finally {
      setClearingCache(false);
    }
  };

  const saveAndTestProxy = async () => {
    setTestingProxy(true);
    setProxyStatus('正在测试…');
    try {
      if (!proxyUrl.trim()) {
        await onChange({ bangumiApiBaseUrl: '' });
        setProxyStatus('已恢复使用官方 API');
        return;
      }
      if (!api.testBangumiConnection) throw new Error('当前版本不提供 Bangumi 网络功能');
      const result = await api.testBangumiConnection(proxyUrl);
      setProxyStatus(result.message);
      if (result.ok) {
        await onChange({ bangumiApiBaseUrl: proxyUrl });
      }
    } catch (reason) {
      setProxyStatus(reason instanceof Error ? reason.message : '地址无效');
    } finally {
      setTestingProxy(false);
    }
  };

  return (
    <div className="settings-layout">
      <section className="settings-section">
        <div className="settings-title"><BellRing size={20} /><div><h2>{t('更新提醒', 'Update alerts')}</h2><p>{t('新一集播出时发送系统通知。', 'Send a system notification when a new episode airs.')}</p></div></div>
        <SettingRow title={t('播出通知', 'Episode notifications')} description={state.runtime?.notificationsSupported === false ? t('当前系统不支持通知', 'Notifications are not supported on this system') : isAndroid ? state.runtime?.notificationPermissionGranted === false ? t('需要在系统设置中允许 AniLog 通知', 'Allow AniLog notifications in system settings') : t('使用 Android 系统通知显示更新', 'Show updates using Android notifications') : t('使用 Windows 通知中心显示更新', 'Show updates in Windows Notification Center')}>
          <Toggle checked={state.settings.notifyWhenAired} disabled={state.runtime?.notificationsSupported === false} onChange={(value) => onChange({ notifyWhenAired: value })} />
        </SettingRow>
        {isAndroid && <SettingRow title={t('自动创建待看任务', 'Create watch tasks automatically')} description={t('关闭后只发送通知，不再新增手机端任务', 'When off, updates only send notifications and do not add mobile tasks')}>
          <Toggle checked={state.settings.createWatchTasks} onChange={(value) => onChange({ createWatchTasks: value })} />
        </SettingRow>}
        <SettingRow title={t('每日待看提醒', 'Daily watch reminder')} description={t('仅在存在待看任务时发送一次汇总通知', 'Send one summary notification only when tasks are pending')}>
          <Toggle checked={state.settings.dailyTaskReminderEnabled} disabled={state.runtime?.notificationsSupported === false} onChange={(value) => onChange({ dailyTaskReminderEnabled: value })} />
        </SettingRow>
        <SettingRow title={t('提醒时间', 'Reminder time')} description={t('错过时间后，将在设备或应用下次启动时补发', 'If missed, it will be delivered after the device or app next starts')}>
          <input
            className="time-input"
            type="time"
            value={state.settings.dailyTaskReminderTime}
            disabled={!state.settings.dailyTaskReminderEnabled}
            aria-label={t('每日待看提醒时间', 'Daily watch reminder time')}
            onChange={(event) => onChange({ dailyTaskReminderTime: event.target.value })}
          />
        </SettingRow>
        <SettingRow title={t('同步间隔', 'Sync interval')} description={IS_ORIGINAL_EDITION ? t('AniList 数据的后台检查频率', 'How often AniList is checked in the background') : t('播出数据与坚果云的后台检查频率', 'How often airing data and WebDAV sync run in the background')}>
          {isAndroid ? <span className="fixed-setting-value">{t('约每 6 小时', 'About every 6 hours')}</span> : <label className="number-select"><select value={state.settings.pollIntervalMinutes} onChange={(event) => onChange({ pollIntervalMinutes: Number(event.target.value) })}><option value={1}>{t('每 1 分钟', 'Every minute')}</option><option value={5}>{t('每 5 分钟', 'Every 5 minutes')}</option><option value={10}>{t('每 10 分钟', 'Every 10 minutes')}</option><option value={15}>{t('每 15 分钟', 'Every 15 minutes')}</option></select></label>}
        </SettingRow>
        {isAndroid && <SettingRow title={t('准时通知', 'On-time notifications')} description={state.runtime?.exactSchedulingGranted ? t('已允许按播出时间准时发送通知', 'Notifications can be sent at the scheduled airing time') : t('未授权时，系统可能延迟发送通知', 'Without permission, the system may delay notifications')}>
          {state.runtime?.exactSchedulingGranted
            ? <span className="fixed-setting-value">{t('已授权', 'Allowed')}</span>
            : <button className="secondary-button" onClick={() => void api.requestExactScheduling?.()}>{t('去授权', 'Open settings')}</button>}
        </SettingRow>}
      </section>
      <section className="settings-section">
        <div className="settings-title"><Cloud size={20} /><div><h2>{t('跨设备同步', 'Cross-device sync')}</h2><p>{t('使用你自己的 WebDAV 账户同步追番和观看任务。', 'Sync followed anime and watch tasks through your own WebDAV account.')}</p></div></div>
        <SettingRow title={t('启用 WebDAV', 'Enable WebDAV')} description={webDavConfig?.supported === false ? t('浏览器预览模式不支持此功能', 'Not available in browser preview mode') : t('设备设置、缓存和通知开关不会同步', 'Device settings, cache, and notification preferences are not synced')}>
          <Toggle
            checked={Boolean(webDavConfig?.enabled)}
            disabled={webDavBusy || !webDavConfig?.supported}
            onChange={(enabled) => { void saveWebDav(enabled); }}
          />
        </SettingRow>
        <div className="webdav-setting">
          <div className="webdav-fields">
            <label><span>{t('服务器地址', 'Server address')}</span><input type="url" value={webDavUrl} disabled={!webDavConfig?.supported} onChange={(event) => { setWebDavUrl(event.target.value); setWebDavStatus(''); }} placeholder="https://dav.example.com/" /></label>
            <label><span>{t('用户名', 'Username')}</span><input value={webDavUsername} disabled={!webDavConfig?.supported} onChange={(event) => { setWebDavUsername(event.target.value); setWebDavStatus(''); }} autoComplete="username" /></label>
            <label><span>{t('应用密码', 'App password')}</span><input type="password" value={webDavPassword} disabled={!webDavConfig?.supported} onChange={(event) => { setWebDavPassword(event.target.value); setWebDavStatus(''); }} placeholder={webDavConfig?.hasPassword ? t('已保存，留空则不修改', 'Saved; leave blank to keep it') : t('输入 WebDAV 应用密码', 'Enter WebDAV app password')} autoComplete="new-password" /></label>
          </div>
          <div className="webdav-actions">
            <button className="secondary-button" disabled={webDavBusy || !webDavConfig?.supported} onClick={() => void saveWebDav()}><Save size={15} /><span>{t('保存', 'Save')}</span></button>
            <button className="secondary-button" disabled={webDavBusy || !webDavConfig?.supported} onClick={testWebDav}><Network size={15} /><span>{t('测试连接', 'Test connection')}</span></button>
            <button className="secondary-button" disabled={webDavBusy || !webDavConfig?.enabled} onClick={syncWebDav}>{webDavBusy ? <LoaderCircle size={15} className="spin" /> : <RefreshCw size={15} />}<span>{t('立即同步', 'Sync now')}</span></button>
          </div>
          <p className={/成功|已同步|已保存|已合并|succeeded|saved|in sync|merged/i.test(webDavStatus) ? 'proxy-status success' : 'proxy-status'}>
            {webDavStatus || (webDavConfig?.lastSyncAt ? t(`上次同步：${formatAiring(webDavConfig.lastSyncAt, true, language)}`, `Last synced: ${formatAiring(webDavConfig.lastSyncAt, true, language)}`) : t('密码保存在系统安全存储中，不会写入追番数据文件。', 'The password is kept in secure system storage and is not written to anime data files.'))}
          </p>
        </div>
      </section>
      {IS_ORIGINAL_EDITION ? (
        <section className="settings-section">
          <div className="settings-title"><Languages size={20} /><div><h2>{t('语言与番名', 'Language and titles')}</h2><p>{t('界面和番剧标题均可选择显示语言。', 'Choose the interface language and preferred AniList title.')}</p></div></div>
          <SettingRow title={t('界面语言', 'Interface language')} description={t('切换后立即生效，并保存在当前设备', 'Takes effect immediately and is saved on this device')}>
            <label className="number-select">
              <select value={state.settings.uiLanguage} onChange={(event) => onChange({ uiLanguage: event.target.value as UiLanguage })}>
                <option value="zh-CN">简体中文</option>
                <option value="en-US">English</option>
              </select>
            </label>
          </SettingRow>
          <SettingRow title={t('首选标题', 'Preferred title')} description={t('首选语言缺失时会自动使用其他可用标题', 'Falls back to another available title when needed')}>
            <label className="number-select">
              <select value={state.settings.titlePreference} onChange={(event) => onChange({ titlePreference: event.target.value as AppSettings['titlePreference'] })}>
                <option value="auto">{t('自动（英文优先）', 'Automatic (English first)')}</option>
                <option value="english">{t('英文', 'English')}</option>
                <option value="romaji">{t('罗马字', 'Romaji')}</option>
                <option value="native">日本語</option>
              </select>
            </label>
          </SettingRow>
        </section>
      ) : (
        <>
        <section className="settings-section">
          <div className="settings-title"><Network size={20} /><div><h2>中文标题网络</h2><p>默认使用公共反代，也可改为自建地址；清空则使用官方 API。</p></div></div>
          <div className="proxy-setting">
            <label htmlFor="bangumi-proxy">Bangumi API 反代地址</label>
            <div className="proxy-controls">
              <input id="bangumi-proxy" type="url" value={proxyUrl} onChange={(event) => { setProxyUrl(event.target.value); setProxyStatus(''); }} placeholder="https://api.example.com" />
              <button className="secondary-button" onClick={saveAndTestProxy} disabled={testingProxy}>{testingProxy ? <LoaderCircle size={15} className="spin" /> : <Network size={15} />}<span>测试并保存</span></button>
            </div>
            <p className={proxyStatus === '连接成功' ? 'proxy-status success' : 'proxy-status'}>{proxyStatus || '反代服务可见你的网络地址和搜索标题，但不会收到追番清单或账号信息。'}</p>
          </div>
        </section>
        <section className="settings-section">
          <div className="settings-title"><User size={20} /><div><h2>Bangumi 账户</h2><p>连接 Bangumi 账户以读取资料与收藏，并按下方设置双向同步。</p></div></div>
          <SettingRow title="连接状态" description="Token 保存在系统安全存储中，不会写入追番数据或同步文件。">
            {bangumiAuth?.hasToken ? (
              <span className="fixed-setting-value">
                {(() => {
                  const avatar = bangumiProfile?.avatar?.medium || bangumiProfile?.avatar?.small;
                  return avatar ? <img src={avatar} alt="" style={{ width: 20, height: 20, borderRadius: '50%', objectFit: 'cover' }} /> : null;
                })()}
                {bangumiProfile ? `${bangumiProfile.username} · ${bangumiProfile.nickname}` : '已连接'}
              </span>
            ) : (
              <span className="fixed-setting-value">{bangumiAuth?.supported === false ? '当前平台不支持' : '未连接'}</span>
            )}
          </SettingRow>
          {bangumiAuth?.hasToken && (
            <SettingRow title="账户资料" description="只读信息，来自 Bangumi 授权账户">
              <span className="fixed-setting-value">{bangumiProfile?.username || '—'}{bangumiProfile?.nickname ? ` · ${bangumiProfile.nickname}` : ''}</span>
            </SettingRow>
          )}
          <div className="webdav-setting">
            <div className="webdav-fields">
              <label><span>Access Token</span><input type="password" value={bangumiToken} placeholder="粘贴 Bangumi Access Token" autoComplete="new-password" onChange={(event) => { setBangumiToken(event.target.value); setBangumiStatus(''); }} /></label>
              <label><span>API 地址</span><input type="url" value={bangumiApiUrl} placeholder="留空使用官方 API" onChange={(event) => { setBangumiApiUrl(event.target.value); setBangumiStatus(''); }} onBlur={() => void saveBangumiApiUrl()} /></label>
            </div>
            <div className="webdav-actions">
              <button className="secondary-button" disabled={bangumiBusy} onClick={() => void saveBangumiToken()}><Save size={15} /><span>保存并测试</span></button>
              <button className="secondary-button" disabled={bangumiBusy || !bangumiAuth?.hasToken} onClick={() => void disconnectBangumi()}><X size={15} /><span>断开连接</span></button>
            </div>
            <p className={/成功|已连接|已保存|已恢复|succeeded|saved|connected|restored/i.test(bangumiStatus) ? 'proxy-status success' : 'proxy-status'}>
              {bangumiStatus || '留空 API 地址时使用官方接口；此地址与上方中文标题反代共用同一设置。'}
            </p>
          </div>
          {bangumiAuth?.hasToken && (
            <>
              <SettingRow title="启用 Bangumi 同步" description="关闭时，Bangumi 拉取与写回全部暂停（坚果云与播出数据不受影响）">
                <Toggle checked={bangumiSyncSettings?.syncEnabled ?? false} disabled={bangumiBusy} onChange={(value) => void updateBangumiSyncSettings({ syncEnabled: value })} />
              </SettingRow>
              <div className={bangumiSyncSettings?.syncEnabled ? 'bangumi-sync-subtoggles' : 'bangumi-sync-subtoggles bangumi-sync-dimmed'}>
              <SettingRow title="从 Bangumi 读取收藏" description="把 Bangumi 收藏（追番/看完/弃番等）拉取合并到本地追番清单">
                <Toggle checked={bangumiSyncSettings?.pullCollections ?? true} disabled={bangumiBusy} onChange={(value) => void updateBangumiSyncSettings({ pullCollections: value })} />
              </SettingRow>
              <SettingRow title="将本地追番变化写回 Bangumi" description="本地追番/取消/评分变化将写入你的 Bangumi 账户，默认关闭（操作后约 1 分钟内自动同步）">
                <Toggle checked={bangumiSyncSettings?.pushLocalChanges ?? false} disabled={bangumiBusy} onChange={(value) => void updateBangumiSyncSettings({ pushLocalChanges: value })} />
              </SettingRow>
              <SettingRow title="完成任务时同步观看进度" description="勾选看完一集后，把观看进度写回 Bangumi">
                <Toggle checked={bangumiSyncSettings?.pushCompletedEpisodes ?? false} disabled={bangumiBusy} onChange={(value) => void updateBangumiSyncSettings({ pushCompletedEpisodes: value })} />
              </SettingRow>
              <SettingRow title="读取 Bangumi 外部状态" description="在 Bangumi 网页或其他客户端产生的变化会拉取回本地">
                <Toggle checked={bangumiSyncSettings?.pullExternalStatus ?? true} disabled={bangumiBusy} onChange={(value) => void updateBangumiSyncSettings({ pullExternalStatus: value })} />
              </SettingRow>
              </div>
              <SettingRow title="冲突策略" description="两端同时变化时按所选策略合并">
                <label className="number-select">
                  <select
                    value={bangumiSyncSettings?.conflictPolicy ?? 'latest'}
                    disabled={bangumiBusy}
                    aria-label="冲突策略"
                    onChange={(event) => void updateBangumiSyncSettings({ conflictPolicy: event.target.value as BangumiConflictPolicy })}
                  >
                    <option value="latest">自动（保留本地修改）</option>
                    <option value="local-first">本地优先</option>
                    <option value="bangumi-first">Bangumi 优先</option>
                  </select>
                </label>
              </SettingRow>
              <div className="webdav-actions">
                <button className="secondary-button" disabled={syncBusy} onClick={() => void runBangumiSync()}>
                  {syncBusy ? <LoaderCircle size={15} className="spin" /> : <RefreshCw size={15} />}
                  <span>立即同步 Bangumi</span>
                </button>
              </div>
              <p className="proxy-status">
                {`上次 Bangumi 同步：${relativePastTime(state.bangumiSyncStatus?.lastBangumiSyncAt, language)} · 上次坚果云同步：${relativePastTime(state.bangumiSyncStatus?.lastWebDavSyncAt, language)}`}
              </p>
              {state.bangumiSyncStatus?.lastSyncError && (
                <p className="proxy-status" style={{ color: '#e5484d' }}>{state.bangumiSyncStatus.lastSyncError}</p>
              )}
              {syncReport && (
                <div style={{ fontSize: 13, borderTop: '1px solid rgba(128,128,128,0.25)', paddingTop: 8 }}>
                  <p style={{ margin: '4px 0' }}>
                    {`拉取 ${syncReport.pulled} · 追番 +${syncReport.followed} · 取消 -${syncReport.unfollowed} · 补完成 ${syncReport.completedTasks} · 写回 ${syncReport.pushed}`}
                    {syncReport.conflicts > 0 ? ` · 冲突 ${syncReport.conflicts}` : ''}
                  </p>
                  {syncReport.suggestions.length > 0 && (
                    <div style={{ margin: '4px 0' }}>
                      <button
                        className="mapping-badge"
                        style={{ border: '1px solid currentColor', borderRadius: 999, padding: '1px 9px', fontSize: 12, background: 'transparent', color: 'inherit', cursor: 'pointer' }}
                        onClick={() => setShowSuggestions((value) => !value)}
                      >
                        {showSuggestions ? '收起建议' : `建议 ${syncReport.suggestions.length} 条（想看/搁置等，不自动改追番）`}
                      </button>
                      {showSuggestions && (
                        <ul style={{ margin: '6px 0', paddingLeft: 20 }}>
                          {syncReport.suggestions.map((suggestion) => (
                            <li key={suggestion.subjectId}>
                              {`${BANGUMI_SUGGESTION_TYPE_LABELS[suggestion.type] || `类型 ${suggestion.type}`}：${suggestion.nameCn || `条目 ${suggestion.subjectId}`}`}
                            </li>
                          ))}
                        </ul>
                      )}
                    </div>
                  )}
                  {syncReport.errors.length > 0 && (
                    <ul style={{ color: '#e5484d', margin: '6px 0', paddingLeft: 20 }}>
                      {syncReport.errors.map((item, index) => <li key={index}>{item}</li>)}
                    </ul>
                  )}
                </div>
              )}
            </>
          )}
        </section>
        </>
      )}
      {isAndroid ? (
        <section className="settings-section">
          <div className="settings-title"><MonitorDot size={20} /><div><h2>{t('Android 后台', 'Android background')}</h2><p>{t('系统定期校正日程，播出时发送普通通知。', 'The system refreshes schedules periodically and sends notifications at airtime.')}</p></div></div>
          <SettingRow title={t('后台方式', 'Background mode')} description={t('不常驻进程，不创建可见的系统闹钟条目', 'Uses system scheduling without keeping a process alive or creating visible alarms')}>
            <span className="fixed-setting-value">{t('系统调度', 'System scheduler')}</span>
          </SettingRow>
        </section>
      ) : (
        <section className="settings-section">
          <div className="settings-title"><MonitorDot size={20} /><div><h2>{t('桌面行为', 'Desktop behavior')}</h2><p>{t('控制应用启动与后台驻留方式。', 'Control startup and background behavior.')}</p></div></div>
          <SettingRow title={t('开机后启动', 'Start at login')} description={state.runtime?.isDesktop ? t('登录 Windows 后自动运行 AniLog', 'Run AniLog automatically after signing in to Windows') : t('仅桌面应用支持此设置', 'Available in the desktop app only')}>
            <Toggle checked={state.settings.launchAtLogin} disabled={!state.runtime?.isDesktop} onChange={(value) => onChange({ launchAtLogin: value })} />
          </SettingRow>
          <SettingRow title={t('关闭时驻留托盘', 'Keep running in tray')} description={t('继续在后台同步并发送更新提醒', 'Continue syncing and sending alerts in the background')}>
            <Toggle checked={state.settings.minimizeToTray} disabled={!state.runtime?.isDesktop} onChange={(value) => onChange({ minimizeToTray: value })} />
          </SettingRow>
          {isTauriDesktop && <SettingRow title={t('显示托盘图标', 'Show tray icon')} description={t('隐藏图标时继续在后台同步并发送提醒', 'Keep syncing and sending alerts while the icon is hidden')}>
            <Toggle label={t('显示托盘图标', 'Show tray icon')} checked={state.settings.showTrayIcon} onChange={(value) => onChange({ showTrayIcon: value })} />
          </SettingRow>}
        </section>
      )}
      {!isAndroid && <section className="settings-section">
        <div className="settings-title"><HardDrive size={20} /><div><h2>{t('缓存空间', 'Cache storage')}</h2><p>{t('封面与网络数据可按需重新下载，不包含追番记录和观看任务。', 'Covers and network data can be downloaded again. Following and tasks are not included.')}</p></div></div>
        <SettingRow title={t('图片与网络缓存', 'Images and network cache')} description={cacheStatus || (IS_ORIGINAL_EDITION ? t('季度列表和本地记录会保留', 'Season lists and local records are kept') : '季度列表、中文标题和本地记录会保留')}>
          <div className="cache-actions">
            <strong>{cacheSupported === false ? t('仅桌面端', 'Desktop only') : cacheBytes === null ? t('正在计算', 'Calculating') : formatStorageSize(cacheBytes)}</strong>
            <button className="secondary-button" disabled={clearingCache || cacheBytes === null || cacheSupported === false} onClick={clearCache}>
              {clearingCache ? <LoaderCircle size={15} className="spin" /> : <Trash2 size={15} />}
              <span>{t('清理缓存', 'Clear cache')}</span>
            </button>
          </div>
        </SettingRow>
      </section>}
      <section className="settings-section source-note">
        <SlidersHorizontal size={20} />
        <div>
          <h2>{t('数据与隐私', 'Data and privacy')}</h2>
          <p>{IS_ORIGINAL_EDITION ? t('番剧、标题与播出日程均来自 AniList，不连接 Bangumi 或第三方 Bangumi 反代。默认仅保存在本机；启用 WebDAV 后，只向你配置的服务器同步追番和观看任务。', 'Anime, titles, and schedules come from AniList. This edition never connects to Bangumi or a third-party Bangumi proxy. Data stays on this device by default; WebDAV only syncs following and watch tasks to your configured server.') : t(
            '番剧与播出日程来自 Bangumi（迁移期部分补充数据来自 AniList）；中文标题来自 Bangumi。追番与任务默认仅保存在本机；启用 WebDAV 后，只向你配置的服务器同步追番和观看任务。Bangumi 账户数据仅存于本机与你的 Bangumi 账户，Token 保存在系统安全存储。',
            'Anime and airing schedules come from Bangumi (some supplemental data still comes from AniList during migration); Chinese titles come from Bangumi. Following and tasks stay on this device by default; when WebDAV is enabled, only your following list and watch tasks are synced to the server you configured. Bangumi account data stays on this device and in your Bangumi account, with the token kept in secure system storage.',
          )}</p>
          <small>
            {IS_ORIGINAL_EDITION
              ? <>{t('AniList 上次同步：', 'Last AniList sync: ')}{state.lastSyncAt ? formatAiring(state.lastSyncAt, true, language) : t('尚未同步', 'Never')}</>
              : <>
                {t('播出数据上次同步：', 'Last airing-data sync: ')}{state.lastSyncAt ? formatAiring(state.lastSyncAt, true, language) : t('尚未同步', 'Never')}
                {lastBangumiSyncAt != null && lastBangumiSyncAt > 0 && <> · {t('Bangumi 上次同步：', 'Last Bangumi sync: ')}{relativePastTime(lastBangumiSyncAt, language)}</>}
              </>}
          </small>
        </div>
      </section>
    </div>
  );
}

function formatStorageSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

// Phase 3：同步状态行的“多久之前”展示（秒级时间戳）。utils 的 relativeTime 面向未来播出时间，
// 过去的时间戳会固定返回“已播出”，因此这里提供过去向的相对时间。
function relativePastTime(timestamp: number | null | undefined, language: UiLanguage): string {
  if (!timestamp) return tr(language, '从未', 'never');
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - timestamp);
  const days = Math.floor(seconds / 86400);
  if (days > 0) return tr(language, `${days} 天前`, `${days} day${days === 1 ? '' : 's'} ago`);
  const hours = Math.floor(seconds / 3600);
  if (hours > 0) return tr(language, `${hours} 小时前`, `${hours} hour${hours === 1 ? '' : 's'} ago`);
  const minutes = Math.max(1, Math.floor(seconds / 60));
  return tr(language, `${minutes} 分钟前`, `${minutes} minute${minutes === 1 ? '' : 's'} ago`);
}

function SettingRow({ title, description, children }: { title: string; description: string; children: React.ReactNode }) {
  return <div className="setting-row"><div><strong>{title}</strong><span>{description}</span></div>{children}</div>;
}

function Toggle({ checked, disabled, label, onChange }: { checked: boolean; disabled?: boolean; label?: string; onChange: (checked: boolean) => void }) {
  return <button className={`toggle ${checked ? 'on' : ''}`} disabled={disabled} role="switch" aria-checked={checked} aria-label={label} onClick={() => onChange(!checked)}><span /></button>;
}

function EmptyState({ icon: Icon, title, body, action, onAction }: { icon: typeof Search; title: string; body: string; action?: string; onAction?: () => void }) {
  return (
    <div className="empty-state"><span><Icon size={27} /></span><h3>{title}</h3><p>{body}</p>{action && <button className="secondary-button" onClick={onAction}>{action}</button>}</div>
  );
}

export default App;
