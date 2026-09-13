package io.anilog.android;

import android.content.Context;
import android.content.SharedPreferences;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;
import java.util.Locale;

final class MobileStore {
    private static final String PREFS = "anilog_mobile";
    private static final String FOLLOWING = "following";
    private static final String EVENTS = "aired_events";
    private static final String PENDING_TASKS = "pending_tasks";
    private static final String DELIVERED = "delivered_ids";
    private static final String NOTIFICATIONS = "notifications_enabled";
    private static final String CREATE_TASKS = "create_tasks_enabled";
    private static final String LAST_SYNC = "last_sync_at";
    private static final String OPEN_TASKS = "open_tasks";
    private static final String UI_LANGUAGE = "ui_language";
    private static final String DAILY_TASK_REMINDER = "daily_task_reminder_enabled";
    private static final String DAILY_TASK_REMINDER_TIME = "daily_task_reminder_time";
    private static final String LAST_TASK_REMINDER_DATE = "last_task_reminder_date";
    // Phase 4 后台完整同步新增（全部本地-only，绝不进坚果云文档）：
    private static final String TOMBSTONES = "following_deleted_at";
    private static final String LAST_FULL_SYNC = "last_full_sync_at";
    private static final String LAST_WEBDAV_SYNC = "last_webdav_sync_at";
    private static final String LAST_BANGUMI_SYNC = "last_bangumi_sync_at";
    private static final String LAST_SCHEDULE_SYNC = "last_schedule_sync_at";
    private static final String LAST_SYNC_ERROR = "last_sync_error";
    private static final String BANGUMI_API_BASE_URL = "bangumi_api_base_url";
    private static final String BANGUMI_EPISODES_CACHE_PREFIX = "bangumi_episodes_cache_";
    private static final String ANILIST_SCHEDULE_CACHE_PREFIX = "anilist_schedule_cache_";
    private static final String PULL_COLLECTIONS = "pull_collections_enabled";
    private static final String BANGUMI_SUGGESTIONS = "pending_bangumi_suggestions";
    private static final String SYNC_INTERVAL_HOURS = "sync_interval_hours";
    private static final String SCHEDULED_INTERVAL_HOURS = "scheduled_interval_hours";
    private static final Object LOCK = new Object();

    private MobileStore() {}

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    private static JSONArray readArray(Context context, String key) {
        try {
            return new JSONArray(prefs(context).getString(key, "[]"));
        } catch (JSONException error) {
            return new JSONArray();
        }
    }

    static void configure(
        Context context,
        JSONArray following,
        JSONArray pendingTasks,
        JSONObject tombstones,
        boolean notificationsEnabled,
        boolean createTasksEnabled,
        boolean dailyTaskReminderEnabled,
        String dailyTaskReminderTime,
        String uiLanguage
    ) throws JSONException, SyncMerge.MergeException {
        synchronized (LOCK) {
            JSONArray localFollowing = following(context);
            JSONObject incoming = SyncMerge.emptyDocument()
                .put("following", following).put("tasks", pendingTasks)
                .put("followingDeletedAt", tombstones);
            SyncMerge.Result result = SyncMerge.merge(BackgroundSyncWorker.localDocument(context), incoming);
            JSONArray mergedFollowing = result.merged.getJSONArray("following");
            // Business LWW must not overwrite newer native schedules on resume.
            SyncMerge.restoreLocalSchedules(mergedFollowing, following);
            for (int index = 0; index < mergedFollowing.length(); index++) {
                JSONObject item = mergedFollowing.getJSONObject(index);
                for (int old = 0; old < localFollowing.length(); old++) {
                    JSONObject previous = localFollowing.getJSONObject(old);
                    if (previous.optInt("id") == item.optInt("id")
                        && previous.optLong("scheduleUpdatedAt") > 0
                        && previous.optLong("scheduleUpdatedAt") >= item.optLong("scheduleUpdatedAt")) {
                        for (String key : SyncMerge.LOCAL_SCHEDULE_KEYS) {
                            if (previous.has(key)) item.put(key, previous.opt(key));
                        }
                    }
                }
            }
            prefs(context).edit()
                .putString(FOLLOWING, mergedFollowing.toString())
                .putString(PENDING_TASKS, result.merged.getJSONArray("tasks").toString())
                .putString(TOMBSTONES, result.merged.getJSONObject("followingDeletedAt").toString())
                .putBoolean(NOTIFICATIONS, notificationsEnabled)
                .putBoolean(CREATE_TASKS, createTasksEnabled)
                .putBoolean(DAILY_TASK_REMINDER, dailyTaskReminderEnabled)
                .putString(DAILY_TASK_REMINDER_TIME, normalizeReminderTime(dailyTaskReminderTime))
                .putString(UI_LANGUAGE, normalizeUiLanguage(context, uiLanguage))
                .apply();
        }
    }

    static JSONArray following(Context context) {
        synchronized (LOCK) {
            return readArray(context, FOLLOWING);
        }
    }

    /**
     * Bangumi 追番状态（Rust configure payload following 摘要的 bangumiStatus 字段：
     * "wish"|"doing"|"done"|"on_hold"|"dropped"，缺省为空）。
     * following 以原始 JSON 原样落盘（configure/updateSchedule/setFollowing 均保留未知键），
     * 因此无需单独投影字段；此读取接口集中暴露语义，供播出通知调度做状态过滤。
     */
    static String followBangumiStatus(JSONObject follow) {
        return follow == null ? "" : follow.optString("bangumiStatus", "");
    }

    static JSONObject findFollow(Context context, int animeId) {
        synchronized (LOCK) {
            JSONArray items = readArray(context, FOLLOWING);
            for (int index = 0; index < items.length(); index += 1) {
                JSONObject item = items.optJSONObject(index);
                if (item != null && item.optInt("id") == animeId) return item;
            }
            return null;
        }
    }

    static JSONObject findFollowByAnilistId(Context context, int anilistId) {
        synchronized (LOCK) {
            JSONArray items = readArray(context, FOLLOWING);
            for (int index = 0; index < items.length(); index += 1) {
                JSONObject item = items.optJSONObject(index);
                if (item != null && item.optInt("anilistId", 0) == anilistId) return item;
            }
            return null;
        }
    }

    static void updateSchedule(Context context, int animeId, Integer episode, Long airingAt, String coverImage) {
        synchronized (LOCK) {
            JSONArray items = readArray(context, FOLLOWING);
            for (int index = 0; index < items.length(); index += 1) {
                JSONObject item = items.optJSONObject(index);
                if (item == null || item.optInt("id") != animeId) continue;
                try {
                    if (episode == null || airingAt == null) {
                        item.remove("nextEpisode");
                        item.remove("nextAiringAt");
                    } else {
                        item.put("nextEpisode", episode);
                        item.put("nextAiringAt", airingAt);
                    }
                    item.put("nextAiringPrecision", "instant");
                    item.put("scheduleUpdatedAt", System.currentTimeMillis());
                    if (coverImage != null && !coverImage.isEmpty()) item.put("coverImage", coverImage);
                } catch (JSONException ignored) {}
                break;
            }
            prefs(context).edit().putString(FOLLOWING, items.toString()).apply();
        }
    }

    static void updateCover(Context context, int animeId, String coverImage) {
        if (coverImage == null || coverImage.isEmpty()) return;
        synchronized (LOCK) {
            JSONArray items = readArray(context, FOLLOWING);
            for (int index = 0; index < items.length(); index += 1) {
                JSONObject item = items.optJSONObject(index);
                if (item == null || item.optInt("id") != animeId) continue;
                try { item.put("coverImage", coverImage); } catch (JSONException ignored) {}
                break;
            }
            prefs(context).edit().putString(FOLLOWING, items.toString()).apply();
        }
    }

    static boolean notificationsEnabled(Context context) {
        return prefs(context).getBoolean(NOTIFICATIONS, true);
    }

    static boolean createTasksEnabled(Context context) {
        return prefs(context).getBoolean(CREATE_TASKS, true);
    }

    /** 完整任务历史（pending + completed），供 WebDAV Worker 使用。 */
    static JSONArray allTasks(Context context) {
        synchronized (LOCK) {
            return readArray(context, PENDING_TASKS);
        }
    }

    /** 未完成任务投影，供每日提醒和界面待看计数使用。 */
    static JSONArray pendingTasks(Context context) {
        JSONArray all = allTasks(context);
        JSONArray pending = new JSONArray();
        for (int index = 0; index < all.length(); index += 1) {
            JSONObject task = all.optJSONObject(index);
            if (task != null && !"completed".equals(task.optString("status", "pending"))) {
                pending.put(task);
            }
        }
        return pending;
    }

    static boolean dailyTaskReminderEnabled(Context context) {
        return prefs(context).getBoolean(DAILY_TASK_REMINDER, false);
    }

    static String dailyTaskReminderTime(Context context) {
        return normalizeReminderTime(prefs(context).getString(DAILY_TASK_REMINDER_TIME, "20:00"));
    }

    static String lastTaskReminderDate(Context context) {
        return prefs(context).getString(LAST_TASK_REMINDER_DATE, "");
    }

    static void setLastTaskReminderDate(Context context, String value) {
        prefs(context).edit().putString(LAST_TASK_REMINDER_DATE, value).apply();
    }

    static String uiLanguage(Context context) {
        return normalizeUiLanguage(context, prefs(context).getString(UI_LANGUAGE, ""));
    }

    private static String normalizeUiLanguage(Context context, String value) {
        if (!context.getPackageName().contains(".original")) return "zh-CN";
        String language = value == null || value.isEmpty() ? Locale.getDefault().getLanguage() : value;
        return language.toLowerCase(Locale.ROOT).startsWith("zh") ? "zh-CN" : "en-US";
    }

    private static String normalizeReminderTime(String value) {
        return value != null && value.matches("(?:[01]\\d|2[0-3]):[0-5]\\d") ? value : "20:00";
    }

    static boolean addAiredEvent(Context context, JSONObject event, boolean storeEvent) {
        synchronized (LOCK) {
            String id = event.optString("id");
            if (id.isEmpty()) return false;
            JSONArray delivered = readArray(context, DELIVERED);
            boolean alreadyDelivered = false;
            for (int index = 0; index < delivered.length(); index += 1) {
                if (id.equals(delivered.optString(index))) alreadyDelivered = true;
            }
            if (!alreadyDelivered) delivered.put(id);
            JSONArray trimmed = new JSONArray();
            int start = Math.max(0, delivered.length() - 500);
            for (int index = start; index < delivered.length(); index += 1) trimmed.put(delivered.opt(index));

            SharedPreferences.Editor editor = prefs(context).edit().putString(DELIVERED, trimmed.toString());
            if (storeEvent) {
                JSONArray events = readArray(context, EVENTS);
                if (!alreadyDelivered) events.put(event);
                editor.putString(EVENTS, events.toString());

                JSONArray pendingTasks = readArray(context, PENDING_TASKS);
                boolean taskKnown = false;
                for (int index = 0; index < pendingTasks.length(); index += 1) {
                    JSONObject pendingTask = pendingTasks.optJSONObject(index);
                    if (pendingTask != null && id.equals(pendingTask.optString("id"))) {
                        taskKnown = true;
                        if ("completed".equals(pendingTask.optString("status"))) alreadyDelivered = true;
                        break;
                    }
                }
                if (!taskKnown) pendingTasks.put(event);
                editor.putString(PENDING_TASKS, pendingTasks.toString());
            }
            editor.apply();
            return !alreadyDelivered;
        }
    }

    static JSONArray consumeEvents(Context context) {
        synchronized (LOCK) {
            JSONArray events = readArray(context, EVENTS);
            prefs(context).edit().putString(EVENTS, "[]").apply();
            return events;
        }
    }

    static void setLastSyncAt(Context context, long epochSeconds) {
        prefs(context).edit().putLong(LAST_SYNC, epochSeconds).apply();
    }

    static long lastSyncAt(Context context) {
        return prefs(context).getLong(LAST_SYNC, 0);
    }

    static void requestOpenTasks(Context context) {
        prefs(context).edit().putBoolean(OPEN_TASKS, true).apply();
    }

    // ------------------------------------------------------------------
    // Phase 4 后台完整同步：本地状态读写（WebDAV 合并写回 + 同步状态 + 设置）
    // ------------------------------------------------------------------

    /** 直接替换 following 列表（仅后台 Worker 的坚果云合并写回使用；前台由 Rust configure 驱动）。 */
    static void setFollowing(Context context, JSONArray following) {
        synchronized (LOCK) {
            JSONArray stored = following;
            if (following != null) {
                try {
                    stored = new JSONArray(following.toString());
                    SyncMerge.restoreLocalSchedules(stored, readArray(context, FOLLOWING));
                }
                catch (JSONException ignored) {}
            }
            prefs(context).edit().putString(FOLLOWING, stored == null ? "[]" : stored.toString()).apply();
        }
    }

    static void applyEpisodeSchedule(Context context, int subjectId, JSONArray episodes, JSONObject media) throws JSONException {
        synchronized (LOCK) {
            JSONArray following = readArray(context, FOLLOWING);
            for (int index = 0; index < following.length(); index++) {
                JSONObject follow = following.getJSONObject(index);
                if (follow.optInt("id") != subjectId) continue;
                long now = System.currentTimeMillis() / 1000L;
                JSONArray rows = EpisodeSchedule.resolve(follow, episodes, media, now);
                if (rows.length() == 0) return; // Missing data is not a cancellation.
                JSONArray tasks = EpisodeSchedule.reconcileTasks(
                    follow, readArray(context, PENDING_TASKS), rows, createTasksEnabled(context), now);
                EpisodeSchedule.updateNext(follow, rows, now);
                follow.put("scheduleUpdatedAt", System.currentTimeMillis());
                JSONArray delivered = readArray(context, DELIVERED);
                JSONArray retained = new JSONArray();
                for (int delivery = 0; delivery < delivered.length(); delivery++) {
                    String id = delivered.optString(delivery);
                    boolean invalid = false;
                    for (int rowIndex = 0; rowIndex < rows.length(); rowIndex++) {
                        JSONObject row = rows.getJSONObject(rowIndex);
                        if (id.equals(subjectId + "-" + row.optInt("episode")) && EpisodeSchedule.future(row, now)) invalid = true;
                    }
                    if (!invalid) retained.put(id);
                }
                prefs(context).edit().putString(FOLLOWING, following.toString())
                    .putString(PENDING_TASKS, tasks.toString()).putString(DELIVERED, retained.toString()).apply();
                return;
            }
        }
    }

    static void advanceAiredSchedule(Context context, int subjectId) {
        synchronized (LOCK) {
            JSONArray following = readArray(context, FOLLOWING);
            for (int index = 0; index < following.length(); index++) {
                JSONObject follow = following.optJSONObject(index);
                if (follow == null || follow.optInt("id") != subjectId) continue;
                JSONArray rows = follow.optJSONArray("episodeSchedule");
                if (rows == null) {
                    updateSchedule(context, subjectId, null, null, null);
                } else {
                    try {
                        EpisodeSchedule.updateNext(follow, rows, System.currentTimeMillis() / 1000L);
                        follow.put("scheduleUpdatedAt", System.currentTimeMillis());
                        prefs(context).edit().putString(FOLLOWING, following.toString()).apply();
                    } catch (JSONException ignored) {}
                }
                return;
            }
        }
    }

    /** 直接替换完整任务历史（仅后台 Worker 的坚果云合并写回使用）。 */
    static void setTasks(Context context, JSONArray tasks) {
        synchronized (LOCK) {
            prefs(context).edit().putString(PENDING_TASKS, tasks == null ? "[]" : tasks.toString()).apply();
        }
    }

    static SyncMerge.Result mergeDocument(Context context, JSONObject remote) throws JSONException, SyncMerge.MergeException {
        synchronized (LOCK) {
            SyncMerge.Result result = SyncMerge.merge(BackgroundSyncWorker.localDocument(context), remote);
            setFollowing(context, result.merged.getJSONArray("following"));
            setTasks(context, result.merged.getJSONArray("tasks"));
            setTombstones(context, result.merged.getJSONObject("followingDeletedAt"));
            return result;
        }
    }

    /** 旧调用名保留给回退代码；新 Worker 必须使用 setTasks，避免丢历史。 */
    static void setPendingTasks(Context context, JSONArray tasks) {
        setTasks(context, tasks);
    }

    /** 取消追番墓碑（{animeId: 毫秒时间戳}），与 Rust syncMetadata.followingDeletedAt 同语义。 */
    static JSONObject tombstones(Context context) {
        synchronized (LOCK) {
            try {
                return new JSONObject(prefs(context).getString(TOMBSTONES, "{}"));
            } catch (JSONException error) {
                return new JSONObject();
            }
        }
    }

    static void setTombstones(Context context, JSONObject tombstones) {
        prefs(context).edit().putString(TOMBSTONES, tombstones == null ? "{}" : tombstones.toString()).apply();
    }

    /** 同步状态五字段（秒级时间戳；与 Rust BangumiSyncStatus 单位一致，仅本地展示）。 */
    static long lastFullSyncAt(Context context) { return prefs(context).getLong(LAST_FULL_SYNC, 0); }

    static long lastWebDavSyncAt(Context context) { return prefs(context).getLong(LAST_WEBDAV_SYNC, 0); }

    static long lastBangumiSyncAt(Context context) { return prefs(context).getLong(LAST_BANGUMI_SYNC, 0); }

    static long lastScheduleSyncAt(Context context) { return prefs(context).getLong(LAST_SCHEDULE_SYNC, 0); }

    static void setLastFullSyncAt(Context context, long epochSeconds) {
        prefs(context).edit().putLong(LAST_FULL_SYNC, epochSeconds).apply();
    }

    static void setLastWebDavSyncAt(Context context, long epochSeconds) {
        prefs(context).edit().putLong(LAST_WEBDAV_SYNC, epochSeconds).apply();
    }

    static void setLastBangumiSyncAt(Context context, long epochSeconds) {
        prefs(context).edit().putLong(LAST_BANGUMI_SYNC, epochSeconds).apply();
    }

    static void setLastScheduleSyncAt(Context context, long epochSeconds) {
        prefs(context).edit().putLong(LAST_SCHEDULE_SYNC, epochSeconds).apply();
    }

    /** 最近一次同步错误摘要；绝不写入 token/凭据（写入前由调用方净化）。 */
    static String lastSyncError(Context context) {
        return prefs(context).getString(LAST_SYNC_ERROR, "");
    }

    static void setLastSyncError(Context context, String error) {
        String value = error == null ? "" : error;
        if (value.length() > 300) value = value.substring(0, 300);
        prefs(context).edit().putString(LAST_SYNC_ERROR, value).apply();
    }

    /** Bangumi 反代基址（settings.bangumiApiBaseUrl 同源；空 = 官方 api.bgm.tv）。 */
    static String bangumiApiBaseUrl(Context context) {
        return prefs(context).getString(BANGUMI_API_BASE_URL, "");
    }

    static void setBangumiApiBaseUrl(Context context, String url) {
        prefs(context).edit().putString(BANGUMI_API_BASE_URL, url == null ? "" : url.trim()).apply();
    }

    /**
     * 读取 Bangumi 逐集缓存。缓存只保存在本机 SharedPreferences，不进入 WebDAV
     * 文档；freshOnly=true 时按 TTL 命中，false 时允许网络失败后的旧缓存回退。
     */
    static JSONArray bangumiEpisodesCache(Context context, int subjectId, long nowSeconds, long maxAgeSeconds, boolean freshOnly) {
        if (subjectId <= 0) return null;
        String raw = prefs(context).getString(BANGUMI_EPISODES_CACHE_PREFIX + subjectId, null);
        if (raw == null || raw.isEmpty()) return null;
        try {
            JSONObject envelope = new JSONObject(raw);
            long fetchedAt = envelope.optLong("fetchedAt", 0);
            JSONArray data = envelope.optJSONArray("data");
            if (fetchedAt <= 0 || data == null) return null;
            if (freshOnly && maxAgeSeconds >= 0 && nowSeconds - fetchedAt > maxAgeSeconds) return null;
            return new JSONArray(data.toString());
        } catch (JSONException ignored) {
            return null;
        }
    }

    static void setBangumiEpisodesCache(Context context, int subjectId, JSONArray episodes, long fetchedAtSeconds) {
        if (subjectId <= 0 || episodes == null || fetchedAtSeconds <= 0) return;
        try {
            JSONObject envelope = new JSONObject();
            envelope.put("fetchedAt", fetchedAtSeconds);
            envelope.put("data", new JSONArray(episodes.toString()));
            prefs(context).edit()
                .putString(BANGUMI_EPISODES_CACHE_PREFIX + subjectId, envelope.toString())
                .apply();
        } catch (JSONException ignored) {}
    }

    /** AniList 分钟级播出快照；只用于 Standard 在上游短暂不可用时保留提醒精度。 */
    static JSONObject anilistScheduleCache(Context context, int anilistId, long nowSeconds, long maxAgeSeconds) {
        if (anilistId <= 0) return null;
        String raw = prefs(context).getString(ANILIST_SCHEDULE_CACHE_PREFIX + anilistId, null);
        if (raw == null || raw.isEmpty()) return null;
        try {
            JSONObject envelope = new JSONObject(raw);
            long fetchedAt = envelope.optLong("fetchedAt", 0);
            JSONObject media = envelope.optJSONObject("media");
            if (fetchedAt <= 0 || media == null || nowSeconds - fetchedAt > maxAgeSeconds) return null;
            return new JSONObject(media.toString()).put("_fetchedAt", fetchedAt);
        } catch (JSONException ignored) {
            return null;
        }
    }

    static void setAnilistScheduleCache(Context context, JSONObject media, long fetchedAtSeconds) {
        if (media == null || fetchedAtSeconds <= 0) return;
        int anilistId = media.optInt("id", 0);
        if (anilistId <= 0) return;
        try {
            JSONObject envelope = new JSONObject();
            envelope.put("fetchedAt", fetchedAtSeconds);
            envelope.put("media", new JSONObject(media.toString()));
            prefs(context).edit()
                .putString(ANILIST_SCHEDULE_CACHE_PREFIX + anilistId, envelope.toString())
                .apply();
        } catch (JSONException ignored) {}
    }

    /** 是否拉取 Bangumi 收藏（默认开；original 永不拉取，与设置无关）。 */
    static boolean pullCollectionsEnabled(Context context) {
        return prefs(context).getBoolean(PULL_COLLECTIONS, true);
    }

    static void setPullCollectionsEnabled(Context context, boolean enabled) {
        prefs(context).edit().putBoolean(PULL_COLLECTIONS, enabled).apply();
    }

    /** Bangumi 收藏拉取的“建议”列表（后台只标记不破坏；由前台 run_full_sync 细化合并）。 */
    static JSONArray bangumiSuggestions(Context context) {
        synchronized (LOCK) {
            return readArray(context, BANGUMI_SUGGESTIONS);
        }
    }

    static void setBangumiSuggestions(Context context, JSONArray suggestions) {
        synchronized (LOCK) {
            prefs(context).edit().putString(BANGUMI_SUGGESTIONS, suggestions == null ? "[]" : suggestions.toString()).apply();
        }
    }

    /** 周期同步间隔（小时，默认 6；与 LOCAL_MIGRATION_PROGRESS 硬不变量 5 一致）。 */
    static int syncIntervalHours(Context context) {
        int value = prefs(context).getInt(SYNC_INTERVAL_HOURS, 6);
        return value >= 1 ? value : 6;
    }

    static void setSyncIntervalHours(Context context, int hours) {
        prefs(context).edit().putInt(SYNC_INTERVAL_HOURS, Math.max(1, hours)).apply();
    }

    static int scheduledIntervalHours(Context context) {
        return prefs(context).getInt(SCHEDULED_INTERVAL_HOURS, 0);
    }

    static void setScheduledIntervalHours(Context context, int hours) {
        prefs(context).edit().putInt(SCHEDULED_INTERVAL_HOURS, hours).apply();
    }

    static boolean consumeOpenTasks(Context context) {
        synchronized (LOCK) {
            boolean requested = prefs(context).getBoolean(OPEN_TASKS, false);
            if (requested) prefs(context).edit().putBoolean(OPEN_TASKS, false).apply();
            return requested;
        }
    }
}
