package io.anilog.android;

import java.time.Instant;
import java.time.LocalDate;
import java.time.OffsetDateTime;
import java.time.ZoneOffset;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Per-subject episode identity and time precision, independent of Android I/O. */
final class EpisodeSchedule {
    private static final long PRECISE_TTL = 7L * 86400L;

    private EpisodeSchedule() {}

    static int integer(double value) {
        if (!Double.isFinite(value)) return 0;
        int rounded = (int) Math.round(value);
        return rounded > 0 && Math.abs(value - rounded) < 0.25 ? rounded : 0;
    }

    static int number(JSONObject episode) {
        int local = integer(episode.optDouble("ep", Double.NaN));
        return local > 0 ? local : integer(episode.optDouble("sort", Double.NaN));
    }

    static long timestamp(String date) {
        try {
            return date.length() > 10
                ? OffsetDateTime.parse(date).toEpochSecond()
                : LocalDate.parse(date).atStartOfDay().toEpochSecond(ZoneOffset.UTC);
        } catch (RuntimeException error) {
            return 0;
        }
    }

    static boolean precise(JSONObject row) {
        return "instant".equals(row.optString("airingPrecision"));
    }

    static boolean aired(JSONObject row, long now) {
        long at = row.optLong("airingAt", 0);
        return at > 0 && (precise(row) ? at <= now : at / 86400L < now / 86400L);
    }

    static boolean future(JSONObject row, long now) {
        long at = row.optLong("airingAt", 0);
        return at > 0 && (precise(row) ? at > now : at / 86400L > now / 86400L);
    }

    private static boolean sameDate(String date, long at) {
        if (date.length() < 10 || at <= 0) return false;
        String day = date.substring(0, 10);
        return Instant.ofEpochSecond(at).atOffset(ZoneOffset.UTC).toLocalDate().toString().equals(day)
            || Instant.ofEpochSecond(at).atOffset(ZoneOffset.ofHours(8)).toLocalDate().toString().equals(day);
    }

    private static List<JSONObject> preciseNodes(JSONObject media) {
        List<JSONObject> nodes = new ArrayList<>();
        if (media == null) return nodes;
        JSONObject schedule = media.optJSONObject("airingSchedule");
        JSONArray history = schedule == null ? null : schedule.optJSONArray("nodes");
        if (history != null) {
            for (int index = 0; index < history.length(); index++) {
                JSONObject node = history.optJSONObject(index);
                if (node != null) nodes.add(node);
            }
        }
        JSONObject next = media.optJSONObject("nextAiringEpisode");
        if (next != null) {
            nodes.removeIf(node -> node.optInt("episode") == next.optInt("episode"));
            nodes.add(next);
        }
        return nodes;
    }

    static JSONArray resolve(JSONObject follow, JSONArray episodes, JSONObject media, long now) throws JSONException {
        List<JSONObject> rows = new ArrayList<>();
        List<JSONObject> nodes = preciseNodes(media);
        Set<Integer> seen = new HashSet<>();
        for (int index = 0; index < episodes.length(); index++) {
            JSONObject episode = episodes.optJSONObject(index);
            if (episode == null || episode.optInt("type", 0) != 0 || episode.optLong("id") <= 0) continue;
            int local = number(episode);
            if (local <= 0 || !seen.add(local)) continue;
            String date = episode.optString("airdate", "").trim();
            long at = timestamp(date);
            boolean instant = at > 0 && date.length() > 10;
            long preciseFetchedAt = 0;
            int global = integer(episode.optDouble("sort", Double.NaN));
            boolean split = global > 0 && global != local;
            long matched = 0;
            for (JSONObject node : nodes) {
                if (node.optInt("episode") == (split ? global : local)) {
                    matched = node.optLong("airingAt", 0);
                    break;
                }
            }
            if (matched <= 0) {
                // A calendar bridge is only safe when both sides are unique.
                int sameDayEpisodes = 0;
                for (int other = 0; other < episodes.length(); other++) {
                    JSONObject candidate = episodes.optJSONObject(other);
                    if (candidate != null && candidate.optInt("type", 0) == 0
                        && candidate.optString("airdate", "").equals(date)) sameDayEpisodes++;
                }
                Set<Long> times = new HashSet<>();
                for (JSONObject node : nodes) {
                    long time = node.optLong("airingAt", 0);
                    if (sameDate(date, time)) times.add(time);
                }
                if (sameDayEpisodes == 1 && times.size() == 1) matched = times.iterator().next();
            }
            if (matched > 0) {
                at = matched;
                instant = true;
                preciseFetchedAt = media.optLong("_fetchedAt", now);
            }
            JSONArray previous = follow.optJSONArray("episodeSchedule");
            if (previous != null) {
                for (int old = 0; old < previous.length(); old++) {
                    JSONObject row = previous.optJSONObject(old);
                    if (row == null || row.optLong("episodeId") != episode.optLong("id") || !precise(row)) continue;
                    long fetched = row.optLong("precisionFetchedAt", 0);
                    if (fetched > preciseFetchedAt && now - fetched <= PRECISE_TTL) {
                        at = row.optLong("airingAt", 0);
                        instant = true;
                        preciseFetchedAt = fetched;
                    }
                }
            }
            rows.add(new JSONObject().put("episode", local).put("episodeId", episode.optLong("id"))
                .put("airdate", date).put("airingAt", at)
                .put("airingPrecision", instant ? "instant" : "date")
                .put("precisionFetchedAt", preciseFetchedAt));
        }
        rows.sort(Comparator.comparingInt(row -> row.optInt("episode")));
        return new JSONArray(rows);
    }

    static JSONObject find(JSONArray rows, int episode) {
        if (rows == null) return null;
        for (int index = 0; index < rows.length(); index++) {
            JSONObject row = rows.optJSONObject(index);
            if (row != null && row.optInt("episode") == episode) return row;
        }
        return null;
    }

    static void updateNext(JSONObject follow, JSONArray rows, long now) throws JSONException {
        JSONObject next = null;
        for (int index = 0; index < rows.length(); index++) {
            JSONObject row = rows.getJSONObject(index);
            if (row.optLong("airingAt") > 0 && !aired(row, now)) {
                next = row;
                break;
            }
        }
        follow.put("episodeSchedule", rows);
        follow.put("nextEpisode", next == null ? 0 : next.optInt("episode"));
        follow.put("nextAiringAt", next == null ? 0 : next.optLong("airingAt"));
        follow.put("nextEpisodeId", next == null ? JSONObject.NULL : next.optLong("episodeId"));
        follow.put("nextAiringPrecision", next == null ? "unknown" : next.optString("airingPrecision"));
        follow.put("scheduleUpdatedAt", now * 1000L);
    }

    static boolean tracks(JSONObject follow) {
        String status = follow.optString("bangumiStatus", "");
        return status.isEmpty() || "doing".equals(status) || "null".equals(status);
    }

    static boolean canNotify(JSONObject follow, int episode, long at, long now) {
        if (!tracks(follow) || episode <= 0 || at <= 0 || at > now) return false;
        if (!"bangumi".equals(follow.optString("source"))) return true;
        JSONObject row = find(follow.optJSONArray("episodeSchedule"), episode);
        return row != null && precise(row) && row.optLong("airingAt") == at && aired(row, now);
    }

    static JSONArray reconcileTasks(JSONObject follow, JSONArray all, JSONArray rows, boolean create, long now) throws JSONException {
        int subject = follow.optInt("id");
        JSONArray kept = new JSONArray();
        Set<Integer> known = new HashSet<>();
        for (int index = 0; index < all.length(); index++) {
            JSONObject task = all.getJSONObject(index);
            boolean belongs = task.optInt("subjectId") == subject || task.optInt("animeId") == subject;
            int episode = task.optInt("episode");
            if (!belongs || "completed".equals(task.optString("status"))) {
                kept.put(task);
                if (belongs) known.add(episode);
                continue;
            }
            JSONObject row = find(rows, episode);
            if (row != null && future(row, now)) continue;
            if (follow.optInt("episodes") > 0 && episode > follow.optInt("episodes")) continue;
            JSONObject normalized = new JSONObject(task.toString());
            if (row != null) {
                normalized.put("episodeId", row.optLong("episodeId"));
                normalized.put("subjectId", subject).put("animeId", subject).put("id", subject + "-" + episode);
                if (precise(row) || normalized.optLong("airingAt") <= 0) {
                    normalized.put("airingAt", row.optLong("airingAt"));
                    normalized.put("airingPrecision", row.optString("airingPrecision"));
                }
                normalized.remove("needsScheduleReview");
                normalized.remove("scheduleReviewReason");
            }
            if (!SyncMerge.stableRecord(task).equals(SyncMerge.stableRecord(normalized))) {
                normalized.put("syncUpdatedAt", now * 1000L);
            }
            kept.put(normalized);
            known.add(episode);
        }
        if (create && tracks(follow)) {
            for (int index = 0; index < rows.length(); index++) {
                JSONObject row = rows.getJSONObject(index);
                int episode = row.optInt("episode");
                long followedAt = follow.optLong("followedAt", 0);
                long at = row.optLong("airingAt");
                if (!aired(row, now) || known.contains(episode) || episode <= follow.optInt("watchedEpisode", 0)
                    || (followedAt > 0 && at / 86400L < followedAt / 86400L)) continue;
                kept.put(new JSONObject().put("id", subject + "-" + episode)
                    .put("animeId", subject).put("subjectId", subject).put("episodeId", row.optLong("episodeId"))
                    .put("episode", episode).put("episodeType", "regular").put("episodeSortKey", String.valueOf(episode))
                    .put("animeTitle", follow.optString("displayTitle")).put("coverImage", follow.optString("coverImage"))
                    .put("airingAt", at).put("airingPrecision", row.optString("airingPrecision"))
                    .put("airingSource", "bangumi_episode").put("status", "pending").put("statusSource", "airing")
                    .put("createdAt", now).put("completedAt", JSONObject.NULL).put("syncUpdatedAt", now * 1000L));
                known.add(episode);
            }
        }
        return kept;
    }
}
