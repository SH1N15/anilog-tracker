package io.anilog.android;

import android.content.Context;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.time.LocalDate;
import java.time.OffsetDateTime;
import java.time.LocalTime;
import java.time.ZoneOffset;
import java.util.ArrayDeque;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AniListScheduler {
    private static final String ENDPOINT = "https://graphql.anilist.co";
    private static final String QUERY = "query MobileSchedules($ids: [Int]) { Page(page: 1, perPage: 50) { media(type: ANIME, id_in: $ids) { id coverImage { medium } nextAiringEpisode { episode airingAt } } } }";
    // 与 Rust 桌面端保持一致：逐集表一天内复用，网络失败时继续使用旧快照。
    private static final long BANGUMI_EPISODES_CACHE_TTL_SECONDS = 24L * 60L * 60L;
    // 分钟级值只来自成功的 AniList 响应；上游短暂故障时最多保留一周。
    private static final long ANILIST_PRECISE_CACHE_MAX_AGE_SECONDS = 7L * 24L * 60L * 60L;
    // AniList 公开限额通常是 90 次/分钟，但负载保护时会收紧到 30。桌面与
    // Android 都保守使用 30，避免后台补偿和手动同步叠加成短时间突发。
    private static final int ANILIST_SAFE_REQUESTS_PER_MINUTE = 30;
    private static final ArrayDeque<Long> ANILIST_REQUEST_WINDOW = new ArrayDeque<>();

    private AniListScheduler() {}

    static synchronized int sync(Context context) throws IOException, JSONException {
        Context appContext = context.getApplicationContext();
        NotificationScheduler.scheduleAll(appContext);
        JSONArray following = MobileStore.following(appContext);
        if (following.length() == 0) {
            MobileStore.setLastSyncAt(appContext, System.currentTimeMillis() / 1000L);
            return 0;
        }

        int updated = 0;
        if (!BuildConfig.isOriginalEdition) {
            updated += syncBangumiEpisodeSchedules(appContext);
            // 在请求 AniList 前先套用最近一次可信分钟级快照；这样 403/断网时
            // 仍能保留提醒精度，同时继续以 Bangumi 逐集表校验集号。
            updated += applyCachedAniListSchedules(appContext, MobileStore.following(appContext));
        }
        for (int offset = 0; offset < following.length(); offset += 50) {
            JSONArray ids = new JSONArray();
            for (int index = offset; index < Math.min(offset + 50, following.length()); index += 1) {
                JSONObject follow = following.optJSONObject(index);
                if (follow == null) continue;
                int queryId = follow.optInt("id");
                if (!BuildConfig.isOriginalEdition && "bangumi".equals(follow.optString("source"))) {
                    queryId = follow.optInt("anilistId", 0);
                }
                if (queryId > 0) ids.put(queryId);
            }
            if (ids.length() == 0) continue;
            JSONArray media;
            try {
                media = request(ids);
            } catch (IOException | JSONException error) {
                if (BuildConfig.isOriginalEdition) throw error;
                // Standard 的 AniList 请求仅是非关键补充；Bangumi 已经完成
                // next/任务纠偏，不能因 AniList 暂停而让整轮同步失败。
                MobileStore.setLastSyncError(appContext, "AniList 补充不可用（" + error.getMessage() + "）");
                continue;
            }
            for (int index = 0; index < media.length(); index += 1) {
                JSONObject item = media.optJSONObject(index);
                if (item == null) continue;
                int animeId = item.optInt("id");
                JSONObject next = item.optJSONObject("nextAiringEpisode");
                JSONObject cover = item.optJSONObject("coverImage");
                String coverImage = cover == null ? null : cover.optString("medium", null);
                if (!BuildConfig.isOriginalEdition) {
                    // 只缓存原始成功响应；缓存结构不进入 WebDAV，也不记录 token。
                    MobileStore.setAnilistScheduleCache(appContext, item, System.currentTimeMillis() / 1000L);
                }
                if (!BuildConfig.isOriginalEdition) {
                    // 一个 AniList media 可能被多个 Bangumi 分季 subject 共用。
                    // 逐条处理，让每个 subject 以自己的 Bangumi `ep` 保持身份；
                    // AniList 的全集序号只通过 Bangumi `sort` 映射为本地集号。
                    boolean hasBangumiMatch = false;
                    JSONArray currentFollowing = MobileStore.following(appContext);
                    for (int followIndex = 0; followIndex < currentFollowing.length(); followIndex += 1) {
                        JSONObject followed = currentFollowing.optJSONObject(followIndex);
                        if (followed == null || !"bangumi".equals(followed.optString("source"))
                            || followed.optInt("anilistId", 0) != animeId) continue;
                        hasBangumiMatch = true;
                        updated += applyAniListPrecisionToBangumiFollow(appContext, followed, next, coverImage);
                    }
                    if (hasBangumiMatch) continue;
                }
                if (next == null) {
                    MobileStore.updateSchedule(appContext, animeId, null, null, coverImage);
                    NotificationScheduler.cancel(appContext, animeId);
                } else {
                    MobileStore.updateSchedule(
                        appContext,
                        animeId,
                        next.optInt("episode"),
                        next.optLong("airingAt"),
                        coverImage
                    );
                    JSONObject follow = MobileStore.findFollow(appContext, animeId);
                    // schedule() 内部按 Bangumi 状态过滤（非“在看”只取消不排新集闹钟），此处无需重复判断。
                    if (follow != null) NotificationScheduler.schedule(appContext, follow);
                }
                updated += 1;
            }
        }
        MobileStore.setLastSyncAt(appContext, System.currentTimeMillis() / 1000L);
        return updated;
    }

    /** Standard 的播出真值来自 Bangumi episode 表；AniList 仅作回退。 */
    static int syncBangumiEpisodeSchedules(Context context) throws IOException, JSONException {
        JSONArray following = MobileStore.following(context);
        int updated = 0;
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject follow = following.optJSONObject(index);
            if (follow == null || !"bangumi".equals(follow.optString("source"))) continue;
            int subjectId = follow.optInt("id", 0);
            if (subjectId <= 0) continue;
            String broadcastTime = follow.optString("bangumiBroadcastTime", "");
            JSONArray episodes;
            try {
                episodes = loadBangumiEpisodes(context, subjectId);
            } catch (IOException | JSONException error) {
                // 单个作品失败不能阻断其它作品；下个周期会继续尝试。
                continue;
            }
            if (episodes == null) continue;
            long now = System.currentTimeMillis() / 1000L;
            JSONArray allTasks = MobileStore.allTasks(context);
            JSONArray keptTasks = new JSONArray();
            int anilistId = follow.optInt("anilistId", 0);
            long followedAt = follow.optLong("followedAt", 0);
            for (int taskIndex = 0; taskIndex < allTasks.length(); taskIndex += 1) {
                JSONObject task = allTasks.optJSONObject(taskIndex);
                if (task == null) continue;
                int taskSubject = task.optInt("subjectId", 0);
                int taskAnimeId = task.optInt("animeId", 0);
                boolean belongs = taskSubject == subjectId || taskAnimeId == subjectId
                    || (anilistId > 0 && taskAnimeId == anilistId);
                if (!belongs || "completed".equals(task.optString("status", "pending"))) {
                    keptTasks.put(task);
                    continue;
                }
                int taskEpisode = task.optInt("episode", 0);
                JSONObject matched = findEpisode(episodes, taskEpisode);
                if (matched == null) {
                    int maxKnownEpisode = 0;
                    for (int episodeIndex = 0; episodeIndex < episodes.length(); episodeIndex += 1) {
                        JSONObject knownEpisode = episodes.optJSONObject(episodeIndex);
                        if (knownEpisode == null || knownEpisode.optInt("type", 0) != 0) continue;
                        maxKnownEpisode = Math.max(maxKnownEpisode, episodeNumber(knownEpisode));
                    }
                    if ("pending".equals(task.optString("status", "pending"))
                        && maxKnownEpisode > 0 && taskEpisode > maxKnownEpisode) {
                        continue;
                    }
                    task.put("needsScheduleReview", true);
                    task.put("scheduleReviewReason", "Bangumi episode airdate unavailable");
                    keptTasks.put(task);
                    continue;
                }
                String taskAirdate = matched.optString("airdate", "").trim();
                if (followedAt > 0 && isBeforeFollow(taskAirdate, followedAt)) {
                    continue;
                }
                if (!taskAirdate.isEmpty() && !isAired(taskAirdate, now)) {
                    continue;
                }
                task.put("episodeId", matched.optLong("id", 0));
                // Bangumi 的日期级 airdate 只有“哪一天”，不能把已有的
                // AniList 分钟级时间覆盖成 UTC 00:00。只有 RFC3339 精确值才
                // 改写已有任务；没有旧时间时才写入日期级回退。
                long correctedAt = parseAirdate(taskAirdate, broadcastTime);
                if (correctedAt > 0 && (isPreciseAirdate(taskAirdate) || task.optLong("airingAt", 0) <= 0)) {
                    task.put("airingAt", correctedAt);
                }
                task.remove("needsScheduleReview");
                task.remove("scheduleReviewReason");
                task.put("animeId", subjectId);
                task.put("subjectId", subjectId);
                task.put("episodeId", matched.optLong("id", 0));
                task.put("id", subjectId + "-" + taskEpisode);
                keptTasks.put(task);
            }
            MobileStore.setTasks(context, keptTasks);
            int nextEpisode = 0;
            long nextAt = 0;
            for (int i = 0; i < episodes.length(); i += 1) {
                JSONObject episode = episodes.optJSONObject(i);
                if (episode == null || episode.optInt("type", 0) != 0) continue;
                int number = episodeNumber(episode);
                if (number <= 0) continue;
                String airdate = episode.optString("airdate", "").trim();
                if (airdate.isEmpty()) continue;
                boolean aired = isAired(airdate, now);
                if (!aired) {
                    long at = parseAirdate(airdate, broadcastTime);
                    if (at > 0 && (nextEpisode == 0 || number < nextEpisode)) {
                        nextEpisode = number;
                        nextAt = at;
                    }
                }
            }
                if (nextEpisode > 0 && nextAt > 0) {
                MobileStore.updateSchedule(context, subjectId, nextEpisode, nextAt, null);
            } else {
                MobileStore.updateSchedule(context, subjectId, null, null, null);
            }
            // Bangumi 逐集纠偏可能刚刚替换了 next；前台 syncNow 也必须立即
            // 重排闹钟，不能等下一个 WorkManager 周期。
            NotificationScheduler.scheduleAll(context);
            updated += 1;
        }
        return updated;
    }

    /**
     * 使用最近一次成功的 AniList nextAiringEpisode 精确值覆盖日期级回退。
     * Bangumi 已确认同一 subject/episode 后才允许应用，避免分季误配。
     */
    private static int applyCachedAniListSchedules(Context context, JSONArray following) {
        if (BuildConfig.isOriginalEdition || following == null) return 0;
        long now = System.currentTimeMillis() / 1000L;
        int updated = 0;
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject follow = following.optJSONObject(index);
            if (follow == null || !"bangumi".equals(follow.optString("source"))) continue;
            int subjectId = follow.optInt("id", 0);
            int anilistId = follow.optInt("anilistId", 0);
            int expectedEpisode = expectedLocalNextEpisode(context, follow, null);
            if (subjectId <= 0 || anilistId <= 0 || expectedEpisode <= 0) continue;
            JSONObject media = MobileStore.anilistScheduleCache(
                context, anilistId, now, ANILIST_PRECISE_CACHE_MAX_AGE_SECONDS);
            if (media == null) continue;
            JSONObject next = media.optJSONObject("nextAiringEpisode");
            JSONObject cover = media.optJSONObject("coverImage");
            String coverImage = cover == null ? null : cover.optString("medium", null);
            updated += applyAniListPrecisionToBangumiFollow(context, follow, next, coverImage);
        }
        return updated;
    }

    private static JSONArray loadBangumiEpisodes(Context context, int subjectId) throws IOException, JSONException {
        long now = System.currentTimeMillis() / 1000L;
        JSONArray fresh = MobileStore.bangumiEpisodesCache(
            context, subjectId, now, BANGUMI_EPISODES_CACHE_TTL_SECONDS, true);
        if (fresh != null) return fresh;
        try {
            JSONArray episodes = requestBangumiEpisodes(context, subjectId);
            MobileStore.setBangumiEpisodesCache(context, subjectId, episodes, now);
            return episodes;
        } catch (IOException | JSONException error) {
            // 逐集表是纠偏数据；离线时宁可复用上次成功快照，也不退回
            // bangumi-data 的 begin + P7D 推算。
            JSONArray stale = MobileStore.bangumiEpisodesCache(context, subjectId, now, -1, false);
            if (stale != null) return stale;
            throw error;
        }
    }

    private static JSONArray requestBangumiEpisodes(Context context, int subjectId) throws IOException, JSONException {
        String base = MobileStore.bangumiApiBaseUrl(context);
        if (base == null || base.trim().isEmpty()) base = "https://api.bgm.tv/v0";
        base = base.trim().replaceAll("/+$", "");
        URL url = new URL(base + "/episodes?subject_id=" + subjectId + "&limit=200&offset=0");
        HttpURLConnection connection = (HttpURLConnection) url.openConnection();
        connection.setRequestMethod("GET");
        connection.setConnectTimeout(15_000);
        connection.setReadTimeout(20_000);
        connection.setRequestProperty("Accept", "application/json");
        connection.setRequestProperty("User-Agent", "AniLog-Android/" + BuildConfig.VERSION_NAME + " (https://github.com/SH1N15/anilog-tracker)");
        String token = BangumiTokenStore.load(context);
        if (token != null && !token.trim().isEmpty()) {
            connection.setRequestProperty("Authorization", "Bearer " + token.trim());
        }
        int status = connection.getResponseCode();
        InputStream stream = status >= 200 && status < 300 ? connection.getInputStream() : connection.getErrorStream();
        String response = readAll(stream);
        connection.disconnect();
        if (status < 200 || status >= 300) throw new IOException("Bangumi episode HTTP " + status);
        JSONObject root = new JSONObject(response);
        JSONArray data = root.optJSONArray("data");
        return data == null ? null : data;
    }

    private static boolean isAired(String value, long now) {
        try {
            if (value.length() > 10 && (value.contains("T") || value.endsWith("Z"))) return parseAirdate(value) <= now;
            return LocalDate.parse(value.substring(0, 10)).isBefore(Instant.ofEpochSecond(now).atZone(ZoneOffset.UTC).toLocalDate());
        } catch (RuntimeException error) { return false; }
    }

    private static long parseAirdate(String value) {
        return parseAirdate(value, "");
    }

    private static long parseAirdate(String value, String broadcastTime) {
        try {
            if (value.length() > 10 && (value.contains("T") || value.endsWith("Z"))) return OffsetDateTime.parse(value).toEpochSecond();
            LocalDate date = LocalDate.parse(value.substring(0, 10));
            LocalTime time = broadcastTime == null || broadcastTime.trim().isEmpty()
                ? LocalTime.MIDNIGHT
                : LocalTime.parse(broadcastTime.trim());
            return date.atTime(time).toEpochSecond(ZoneOffset.UTC);
        } catch (RuntimeException error) { return 0; }
    }

    private static boolean isPreciseAirdate(String value) {
        String trimmed = value == null ? "" : value.trim();
        return trimmed.length() > 10 && (trimmed.contains("T") || trimmed.endsWith("Z"));
    }

    private static JSONObject findEpisode(JSONArray episodes, int number) {
        if (number <= 0) return null;
        for (int index = 0; index < episodes.length(); index += 1) {
            JSONObject episode = episodes.optJSONObject(index);
            if (episode == null || episode.optInt("type", 0) != 0) continue;
            if (episodeNumber(episode) == number) return episode;
        }
        return null;
    }

    private static boolean isBeforeFollow(String value, long followedAt) {
        if (value == null || value.trim().isEmpty() || followedAt <= 0) return false;
        String trimmed = value.trim();
        try {
            if (trimmed.length() > 10 && (trimmed.contains("T") || trimmed.endsWith("Z"))) {
                return parseAirdate(trimmed) < followedAt;
            }
            LocalDate airDate = LocalDate.parse(trimmed.substring(0, 10));
            LocalDate followDate = Instant.ofEpochSecond(followedAt)
                .atZone(ZoneOffset.UTC).toLocalDate();
            return airDate.isBefore(followDate);
        } catch (RuntimeException error) { return false; }
    }

    private static int expectedLocalNextEpisode(Context context, JSONObject follow, JSONArray supplied) {
        int subjectId = follow.optInt("id", 0);
        JSONArray episodes = supplied != null ? supplied : MobileStore.bangumiEpisodesCache(
            context, subjectId, System.currentTimeMillis() / 1000L, -1, false);
        if (episodes == null) return 0;
        long now = System.currentTimeMillis() / 1000L;
        int next = 0;
        for (int index = 0; index < episodes.length(); index += 1) {
            JSONObject episode = episodes.optJSONObject(index);
            if (episode == null || episode.optInt("type", 0) != 0) continue;
            int number = episodeNumber(episode);
            String airdate = episode.optString("airdate", "").trim();
            if (number > 0 && !airdate.isEmpty() && !isAired(airdate, now)
                && (next == 0 || number < next)) next = number;
        }
        return next;
    }

    /** Bangumi `ep` is the local season number; `sort` is global order. */
    private static int episodeNumber(JSONObject episode) {
        double ep = episode.optDouble("ep", Double.NaN);
        if (!Double.isNaN(ep)) {
            int rounded = (int) Math.rint(ep);
            if (rounded > 0 && Math.abs(ep - rounded) < 0.25) return rounded;
        }
        double sort = episode.optDouble("sort", Double.NaN);
        if (Double.isNaN(sort)) return 0;
        int rounded = (int) Math.rint(sort);
        return rounded > 0 && Math.abs(sort - rounded) < 0.25 ? rounded : 0;
    }

    private static void removeFuturePendingTask(Context context, int subjectId, int anilistId, int episode, long airingAt) {
        if (subjectId <= 0 || episode <= 0 || airingAt <= System.currentTimeMillis() / 1000L) return;
        JSONArray allTasks = MobileStore.allTasks(context);
        JSONArray kept = new JSONArray();
        for (int index = 0; index < allTasks.length(); index += 1) {
            JSONObject task = allTasks.optJSONObject(index);
            if (task == null) continue;
            int taskSubject = task.optInt("subjectId", 0);
            int taskAnimeId = task.optInt("animeId", 0);
            if ("pending".equals(task.optString("status", "pending"))
                && (taskSubject == subjectId || taskAnimeId == subjectId || (anilistId > 0 && taskAnimeId == anilistId))
                && task.optInt("episode", 0) == episode) continue;
            kept.put(task);
        }
        MobileStore.setTasks(context, kept);
    }

    /**
     * Apply an AniList minute timestamp only after the current Bangumi subject
     * has confirmed which local episode it belongs to.  AniList usually uses
     * the same number; for split subjects it may use Bangumi's global `sort`.
     */
    private static int applyAniListPrecisionToBangumiFollow(
        Context context,
        JSONObject follow,
        JSONObject next,
        String coverImage
    ) {
        int subjectId = follow.optInt("id", 0);
        int anilistId = follow.optInt("anilistId", 0);
        int expectedEpisode = expectedLocalNextEpisode(context, follow, null);
        if (subjectId <= 0 || expectedEpisode <= 0) return 0;
        if (next == null || next.optLong("airingAt", 0) <= 0) {
            MobileStore.updateCover(context, subjectId, coverImage);
            return 0;
        }
        int aniListEpisode = next.optInt("episode", 0);
        int localEpisode = localEpisodeForAniListNumber(context, subjectId, aniListEpisode);
        if (localEpisode <= 0) {
            // AniList may number a split subject from the enclosing season
            // (e.g. ep16) while Bangumi's subject starts at local ep5.  When
            // `sort` is not the same number space, the scheduled calendar
            // date is the stable bridge between the two sources.
            localEpisode = localEpisodeForAniListDate(
                context, subjectId, next.optLong("airingAt", 0));
        }
        if (localEpisode != expectedEpisode) {
            MobileStore.updateCover(context, subjectId, coverImage);
            return 0;
        }
        long airingAt = next.optLong("airingAt", 0);
        MobileStore.updateSchedule(context, subjectId, expectedEpisode, airingAt, coverImage);
        // Bangumi 日期错误时可能已经提前生成 pending 任务；AniList 的时间
        // 是最终门禁，未来集必须撤回，待实际播出后再创建。
        removeFuturePendingTask(context, subjectId, anilistId, expectedEpisode, airingAt);
        return 1;
    }

    private static int localEpisodeForAniListNumber(Context context, int subjectId, int aniListEpisode) {
        if (aniListEpisode <= 0) return 0;
        JSONArray episodes = MobileStore.bangumiEpisodesCache(
            context, subjectId, System.currentTimeMillis() / 1000L, -1, false);
        if (episodes == null) return 0;
        for (int index = 0; index < episodes.length(); index += 1) {
            JSONObject episode = episodes.optJSONObject(index);
            if (episode == null || episode.optInt("type", 0) != 0) continue;
            int local = episodeNumber(episode);
            if (local <= 0) continue;
            if (local == aniListEpisode || episodeSortNumber(episode) == aniListEpisode) return local;
        }
        return 0;
    }

    private static int episodeSortNumber(JSONObject episode) {
        double sort = episode.optDouble("sort", Double.NaN);
        if (Double.isNaN(sort)) return 0;
        int rounded = (int) Math.rint(sort);
        return rounded > 0 && Math.abs(sort - rounded) < 0.25 ? rounded : 0;
    }

    private static int localEpisodeForAniListDate(Context context, int subjectId, long airingAt) {
        if (subjectId <= 0 || airingAt <= 0) return 0;
        JSONArray episodes = MobileStore.bangumiEpisodesCache(
            context, subjectId, System.currentTimeMillis() / 1000L, -1, false);
        if (episodes == null) return 0;
        LocalDate utcDate = Instant.ofEpochSecond(airingAt).atZone(ZoneOffset.UTC).toLocalDate();
        LocalDate eastAsiaDate = Instant.ofEpochSecond(airingAt)
            .atZone(ZoneOffset.ofHours(8)).toLocalDate();
        for (int index = 0; index < episodes.length(); index += 1) {
            JSONObject episode = episodes.optJSONObject(index);
            if (episode == null || episode.optInt("type", 0) != 0) continue;
            String airdate = episode.optString("airdate", "").trim();
            if (airdate.length() < 10) continue;
            LocalDate bangumiDate;
            try {
                bangumiDate = LocalDate.parse(airdate.substring(0, 10));
            } catch (RuntimeException error) {
                continue;
            }
            if (bangumiDate.equals(utcDate) || bangumiDate.equals(eastAsiaDate)) {
                return episodeNumber(episode);
            }
        }
        return 0;
    }

    private static JSONArray request(JSONArray ids) throws IOException, JSONException {
        awaitAniListPermit();
        JSONObject variables = new JSONObject().put("ids", ids);
        JSONObject payload = new JSONObject().put("query", QUERY).put("variables", variables);
        HttpURLConnection connection = (HttpURLConnection) new URL(ENDPOINT).openConnection();
        connection.setRequestMethod("POST");
        connection.setConnectTimeout(15_000);
        connection.setReadTimeout(20_000);
        connection.setDoOutput(true);
        connection.setRequestProperty("Content-Type", "application/json");
        connection.setRequestProperty("Accept", "application/json, multipart/mixed");
        connection.setRequestProperty("Origin", "https://anilist.co");
        connection.setRequestProperty("Referer", "https://anilist.co/");
        connection.setRequestProperty("User-Agent", "AniLog-Android/" + BuildConfig.VERSION_NAME + " (https://github.com/SH1N15/anilog-tracker)");
        byte[] body = payload.toString().getBytes(StandardCharsets.UTF_8);
        connection.setFixedLengthStreamingMode(body.length);
        try (OutputStream output = connection.getOutputStream()) {
            output.write(body);
        }

        int status = connection.getResponseCode();
        InputStream stream = status >= 200 && status < 300 ? connection.getInputStream() : connection.getErrorStream();
        String response = readAll(stream);
        connection.disconnect();
        if (status < 200 || status >= 300) throw new IOException("AniList HTTP " + status);
        JSONObject root = new JSONObject(response);
        if (root.has("errors")) throw new IOException("AniList returned GraphQL errors");
        JSONObject data = root.optJSONObject("data");
        JSONObject page = data == null ? null : data.optJSONObject("Page");
        JSONArray media = page == null ? null : page.optJSONArray("media");
        if (media == null) throw new IOException("AniList returned invalid schedule data");
        return media;
    }

    private static void awaitAniListPermit() throws IOException {
        while (true) {
            long waitMillis = 0;
            synchronized (ANILIST_REQUEST_WINDOW) {
                long now = System.currentTimeMillis();
                while (!ANILIST_REQUEST_WINDOW.isEmpty()
                    && now - ANILIST_REQUEST_WINDOW.peekFirst() >= 60_000L) {
                    ANILIST_REQUEST_WINDOW.removeFirst();
                }
                if (ANILIST_REQUEST_WINDOW.size() < ANILIST_SAFE_REQUESTS_PER_MINUTE) {
                    ANILIST_REQUEST_WINDOW.addLast(now);
                    return;
                }
                waitMillis = Math.max(1L, 60_000L - (now - ANILIST_REQUEST_WINDOW.peekFirst()));
            }
            try {
                Thread.sleep(waitMillis);
            } catch (InterruptedException error) {
                Thread.currentThread().interrupt();
                throw new IOException("AniList request throttling interrupted", error);
            }
        }
    }

    private static String readAll(InputStream stream) throws IOException {
        if (stream == null) return "";
        StringBuilder output = new StringBuilder();
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) output.append(line);
        }
        return output.toString();
    }
}
