package io.anilog.android;

import java.time.ZonedDateTime;
import java.time.format.DateTimeFormatter;
import java.util.Map;
import java.util.TreeMap;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

final class AniListRequestPolicy {
    static final long DAY = 86400L;
    static final long MANUAL_COOLDOWN = 60L;
    static final long PRECISE_MAX_AGE = 7L * DAY;

    private AniListRequestPolicy() {}

    static int mediaId(JSONObject follow, boolean original) {
        return follow.optInt(!original && "bangumi".equals(follow.optString("source")) ? "anilistId" : "id", 0);
    }

    static Map<Integer, Long> requests(JSONArray following, boolean original, long activeTtl, int target) {
        Map<Integer, Long> result = new TreeMap<>();
        for (int index = 0; index < following.length(); index++) {
            JSONObject follow = following.optJSONObject(index);
            if (follow == null || (target > 0 && follow.optInt("id") != target)) continue;
            int id = mediaId(follow, original);
            String status = original ? "" : follow.optString("bangumiStatus");
            if (id <= 0 || "dropped".equals(status)) continue;
            long ttl = "done".equals(status) ? 30L * DAY
                : ("wish".equals(status) || "on_hold".equals(status)) ? DAY : Math.max(MANUAL_COOLDOWN, activeTtl);
            Long old = result.get(id);
            result.put(id, old == null ? ttl : Math.min(old, ttl));
        }
        return result;
    }

    static boolean fresh(long fetchedAt, long now, long ttl) {
        return fetchedAt > 0 && fetchedAt <= now && now - fetchedAt < ttl;
    }

    static boolean needsRefresh(JSONObject media, long now, long ttl, boolean force) {
        if (media == null) return true;
        String status = media.optString("status");
        long lifetime = force ? MANUAL_COOLDOWN : media.optBoolean("_missing") ? DAY
            : ("FINISHED".equals(status) || "CANCELLED".equals(status)) ? 30L * DAY : ttl;
        return !fresh(media.optLong("_fetchedAt"), now, lifetime);
    }

    static long retryDelay(int status, String retryAfter, int failures, long now) {
        long retry = 0;
        if (retryAfter != null) {
            try { retry = Long.parseLong(retryAfter.trim()); }
            catch (NumberFormatException error) {
                try { retry = ZonedDateTime.parse(retryAfter, DateTimeFormatter.RFC_1123_DATE_TIME).toEpochSecond() - now; }
                catch (RuntimeException ignored) {}
            }
        }
        long base = status == 403 ? 900L : 60L;
        long backoff = Math.min(3600L, base * (1L << Math.min(6, Math.max(0, failures - 1))));
        return Math.max(backoff, Math.max(0, Math.min(7L * DAY, retry)));
    }

    static JSONObject nextEpisode(JSONObject media, long now) {
        JSONObject next = media.optJSONObject("nextAiringEpisode");
        if (next != null && (next.optLong("airingAt") <= now || next.optInt("episode") <= 0)) next = null;
        JSONObject schedule = media.optJSONObject("futureAiringSchedule");
        JSONArray nodes = schedule == null ? null : schedule.optJSONArray("nodes");
        if (nodes != null) {
            for (int index = 0; index < nodes.length(); index++) {
                JSONObject row = nodes.optJSONObject(index);
                if (row != null && row.optInt("episode") > 0 && row.optLong("airingAt") > now
                    && (next == null || row.optLong("airingAt") < next.optLong("airingAt"))) next = row;
            }
        }
        return next;
    }

    static JSONArray validResponse(JSONArray requested, JSONArray response) throws JSONException {
        Map<Integer, JSONObject> found = new TreeMap<>();
        for (int index = 0; index < response.length(); index++) {
            JSONObject media = response.optJSONObject(index);
            if (media != null && media.optInt("id") > 0) found.put(media.optInt("id"), media);
        }
        JSONArray result = new JSONArray();
        for (int index = 0; index < requested.length(); index++) {
            int id = requested.getInt(index);
            JSONObject media = found.get(id);
            result.put(media == null ? new JSONObject().put("id", id).put("_missing", true) : media);
        }
        return result;
    }
}
