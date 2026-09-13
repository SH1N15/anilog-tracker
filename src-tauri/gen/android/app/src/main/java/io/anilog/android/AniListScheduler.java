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
import java.util.ArrayDeque;
import java.util.HashSet;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AniListScheduler {
    private static final String ENDPOINT = "https://graphql.anilist.co";
    private static final String QUERY = "query MobileSchedules($ids: [Int]) { Page(page: 1, perPage: 50) { media(type: ANIME, id_in: $ids) { id coverImage { medium } nextAiringEpisode { episode airingAt } airingSchedule(notYetAired: false, perPage: 50) { nodes { episode airingAt } } } } }";
    private static final long BANGUMI_EPISODES_CACHE_TTL_SECONDS = 24L * 60L * 60L;
    private static final long ANILIST_PRECISE_CACHE_MAX_AGE_SECONDS = 7L * 24L * 60L * 60L;
    private static final int ANILIST_SAFE_REQUESTS_PER_MINUTE = 30;
    private static final ArrayDeque<Long> ANILIST_REQUEST_WINDOW = new ArrayDeque<>();

    private AniListScheduler() {}

    static synchronized int sync(Context context) throws IOException, JSONException {
        Context app = context.getApplicationContext();
        int updated = BuildConfig.isOriginalEdition ? 0 : syncBangumiEpisodeSchedules(app);
        JSONArray following = MobileStore.following(app);
        Set<Integer> requested = new HashSet<>();
        for (int offset = 0; offset < following.length(); offset += 50) {
            JSONArray ids = new JSONArray();
            for (int index = offset; index < Math.min(offset + 50, following.length()); index++) {
                JSONObject follow = following.optJSONObject(index);
                if (follow == null) continue;
                int id = !BuildConfig.isOriginalEdition && "bangumi".equals(follow.optString("source"))
                    ? follow.optInt("anilistId", 0) : follow.optInt("id");
                if (id > 0 && requested.add(id)) ids.put(id);
            }
            if (ids.length() == 0) continue;
            JSONArray media;
            try {
                media = request(ids);
            } catch (IOException | JSONException error) {
                if (BuildConfig.isOriginalEdition) throw error;
                MobileStore.setLastSyncError(app, "AniList supplement unavailable");
                continue;
            }
            for (int index = 0; index < media.length(); index++) {
                JSONObject item = media.optJSONObject(index);
                if (item == null) continue;
                int id = item.optInt("id");
                JSONObject next = item.optJSONObject("nextAiringEpisode");
                JSONObject cover = item.optJSONObject("coverImage");
                String image = cover == null ? null : cover.optString("medium", null);
                if (!BuildConfig.isOriginalEdition) {
                    MobileStore.setAnilistScheduleCache(app, item, System.currentTimeMillis() / 1000L);
                    JSONArray current = MobileStore.following(app);
                    boolean matched = false;
                    for (int f = 0; f < current.length(); f++) {
                        JSONObject follow = current.getJSONObject(f);
                        if ("bangumi".equals(follow.optString("source")) && follow.optInt("anilistId") == id) {
                            matched = true;
                            MobileStore.updateCover(app, follow.optInt("id"), image);
                            refreshCachedSubject(app, follow);
                        }
                    }
                    if (matched) {
                        updated++;
                        continue;
                    }
                }
                MobileStore.updateSchedule(app, id,
                    next == null ? null : next.optInt("episode"),
                    next == null ? null : next.optLong("airingAt"), image);
                updated++;
            }
        }
        // No alarms are scheduled against an intermediate date-only snapshot.
        NotificationScheduler.scheduleAll(app);
        MobileStore.setLastSyncAt(app, System.currentTimeMillis() / 1000L);
        return updated;
    }

    static synchronized int syncBangumiEpisodeSchedules(Context context) throws IOException, JSONException {
        if (BuildConfig.isOriginalEdition) return 0;
        JSONArray following = MobileStore.following(context);
        int updated = 0;
        for (int index = 0; index < following.length(); index++) {
            JSONObject follow = following.optJSONObject(index);
            if (follow == null || !"bangumi".equals(follow.optString("source"))) continue;
            int subject = follow.optInt("id");
            if (subject <= 0) continue;
            JSONArray episodes;
            try {
                episodes = loadBangumiEpisodes(context, subject);
            } catch (IOException | JSONException error) {
                continue;
            }
            if (episodes == null) continue;
            MobileStore.applyEpisodeSchedule(context, subject, episodes, preciseCache(context, follow));
            updated++;
        }
        return updated;
    }

    static void refreshCachedSchedules(Context context) throws JSONException {
        if (BuildConfig.isOriginalEdition) return;
        JSONArray following = MobileStore.following(context);
        for (int index = 0; index < following.length(); index++) {
            JSONObject follow = following.optJSONObject(index);
            if (follow != null && "bangumi".equals(follow.optString("source"))) refreshCachedSubject(context, follow);
        }
    }

    private static void refreshCachedSubject(Context context, JSONObject follow) throws JSONException {
        JSONArray episodes = MobileStore.bangumiEpisodesCache(
            context, follow.optInt("id"), System.currentTimeMillis() / 1000L, -1, false);
        if (episodes != null) {
            MobileStore.applyEpisodeSchedule(context, follow.optInt("id"), episodes, preciseCache(context, follow));
        }
    }

    private static JSONObject preciseCache(Context context, JSONObject follow) {
        return MobileStore.anilistScheduleCache(context, follow.optInt("anilistId"),
            System.currentTimeMillis() / 1000L, ANILIST_PRECISE_CACHE_MAX_AGE_SECONDS);
    }

    private static JSONArray loadBangumiEpisodes(Context context, int subject) throws IOException, JSONException {
        long now = System.currentTimeMillis() / 1000L;
        JSONArray fresh = MobileStore.bangumiEpisodesCache(
            context, subject, now, BANGUMI_EPISODES_CACHE_TTL_SECONDS, true);
        if (fresh != null) return fresh;
        try {
            JSONArray episodes = requestBangumiEpisodes(context, subject);
            MobileStore.setBangumiEpisodesCache(context, subject, episodes, now);
            return episodes;
        } catch (IOException | JSONException error) {
            JSONArray stale = MobileStore.bangumiEpisodesCache(context, subject, now, -1, false);
            if (stale != null) return stale;
            throw error;
        }
    }

    private static JSONArray requestBangumiEpisodes(Context context, int subject) throws IOException, JSONException {
        if (BuildConfig.isOriginalEdition) throw new IOException("Bangumi is disabled");
        String base = MobileStore.bangumiApiBaseUrl(context);
        if (base == null || base.trim().isEmpty()) base = "https://api.bgm.tv/v0";
        base = base.trim().replaceAll("/+$", "");
        if (!base.endsWith("/v0")) base += "/v0";
        JSONArray episodes = new JSONArray();
        for (int offset = 0; offset < 2000; offset += 200) {
            HttpURLConnection connection = (HttpURLConnection) new URL(
                base + "/episodes?subject_id=" + subject + "&limit=200&offset=" + offset).openConnection();
            connection.setRequestMethod("GET");
            connection.setConnectTimeout(15_000);
            connection.setReadTimeout(20_000);
            connection.setRequestProperty("Accept", "application/json");
            connection.setRequestProperty("User-Agent", userAgent());
            String token = BangumiTokenStore.load(context);
            if (token != null && !token.trim().isEmpty()) {
                connection.setRequestProperty("Authorization", "Bearer " + token.trim());
            }
            try {
                int status = connection.getResponseCode();
                if (status < 200 || status >= 300) throw new IOException("Bangumi episode HTTP " + status);
                JSONObject root = new JSONObject(readAll(connection.getInputStream()));
                JSONArray page = root.optJSONArray("data");
                if (page == null) throw new IOException("Invalid episode response");
                for (int index = 0; index < page.length(); index++) episodes.put(page.get(index));
                if (page.length() < 200 || episodes.length() >= root.optInt("total", Integer.MAX_VALUE)) return episodes;
            } finally {
                connection.disconnect();
            }
        }
        throw new IOException("Episode list pagination limit");
    }

    private static JSONArray request(JSONArray ids) throws IOException, JSONException {
        awaitAniListPermit();
        JSONObject payload = new JSONObject().put("query", QUERY).put("variables", new JSONObject().put("ids", ids));
        HttpURLConnection connection = (HttpURLConnection) new URL(ENDPOINT).openConnection();
        connection.setRequestMethod("POST");
        connection.setConnectTimeout(15_000);
        connection.setReadTimeout(20_000);
        connection.setDoOutput(true);
        connection.setRequestProperty("Content-Type", "application/json");
        connection.setRequestProperty("Accept", "application/json, multipart/mixed");
        connection.setRequestProperty("Origin", "https://anilist.co");
        connection.setRequestProperty("Referer", "https://anilist.co/");
        connection.setRequestProperty("User-Agent", userAgent());
        try {
            byte[] body = payload.toString().getBytes(StandardCharsets.UTF_8);
            connection.setFixedLengthStreamingMode(body.length);
            try (OutputStream output = connection.getOutputStream()) { output.write(body); }
            int status = connection.getResponseCode();
            if (status < 200 || status >= 300) throw new IOException("AniList HTTP " + status);
            JSONObject root = new JSONObject(readAll(connection.getInputStream()));
            if (root.has("errors")) throw new IOException("AniList returned GraphQL errors");
            JSONObject data = root.optJSONObject("data");
            JSONObject page = data == null ? null : data.optJSONObject("Page");
            JSONArray media = page == null ? null : page.optJSONArray("media");
            if (media == null) throw new IOException("AniList returned invalid schedule data");
            return media;
        } finally {
            connection.disconnect();
        }
    }

    private static String userAgent() {
        return "AniLog-Android/" + BuildConfig.VERSION_NAME + " (https://github.com/SH1N15/anilog-tracker)";
    }

    private static void awaitAniListPermit() throws IOException {
        while (true) {
            long waitMillis;
            synchronized (ANILIST_REQUEST_WINDOW) {
                long now = System.currentTimeMillis();
                while (!ANILIST_REQUEST_WINDOW.isEmpty() && now - ANILIST_REQUEST_WINDOW.peekFirst() >= 60_000L) {
                    ANILIST_REQUEST_WINDOW.removeFirst();
                }
                if (ANILIST_REQUEST_WINDOW.size() < ANILIST_SAFE_REQUESTS_PER_MINUTE) {
                    ANILIST_REQUEST_WINDOW.addLast(now);
                    return;
                }
                waitMillis = Math.max(1L, 60_000L - (now - ANILIST_REQUEST_WINDOW.peekFirst()));
            }
            try { Thread.sleep(waitMillis); }
            catch (InterruptedException error) {
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
