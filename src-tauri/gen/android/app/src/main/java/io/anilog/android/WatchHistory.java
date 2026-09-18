package io.anilog.android;

import java.util.HashSet;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Completion review is business history, not a disposable schedule cache. */
final class WatchHistory {
    private WatchHistory() {}

    static String decision(JSONObject task) {
        JSONObject review = task.optJSONObject("completionReview");
        return review == null ? "" : review.optString("decision");
    }

    static boolean needsReview(JSONObject task) {
        return "completed".equals(task.optString("status")) && "review".equals(decision(task));
    }

    static boolean isCompleted(JSONObject task) {
        return "completed".equals(task.optString("status")) && !needsReview(task);
    }

    static boolean isReset(JSONObject task) {
        return "pending".equals(task.optString("status")) && "reset".equals(decision(task));
    }

    static boolean isPending(JSONObject task, long now) {
        return "pending".equals(task.optString("status")) && !(isReset(task) && isFuture(task, now));
    }

    static boolean isFuture(JSONObject task, long now) {
        long at = task.optLong("airingAt");
        String precision = task.optString("airingPrecision");
        return at > 0 && ("instant".equals(precision) ? at > now
            : "date".equals(precision) && at / 86400L > now / 86400L);
    }

    static boolean reviewFutureCompletion(JSONObject task, long now) throws JSONException {
        if (!"completed".equals(task.optString("status")) || "keep".equals(decision(task))
            || needsReview(task) || !"bangumi_episode".equals(task.optString("airingSource"))
            || task.optLong("episodeId") <= 0 || !isFuture(task, now)) return false;
        long revision = Math.max(now * 1000L, task.optLong("syncUpdatedAt") + 1L);
        JSONObject review = new JSONObject().put("decision", "review").put("reason", "before_airing")
            .put("detectedAt", revision).put("airingAt", task.opt("airingAt"))
            .put("airingPrecision", task.opt("airingPrecision"));
        for (String key : new String[] {"completedAt", "createdAt", "syncUpdatedAt", "lastPushedToBangumiAt"}) {
            review.put("original" + Character.toUpperCase(key.charAt(0)) + key.substring(1),
                task.has(key) ? task.opt(key) : JSONObject.NULL);
        }
        task.put("completionReview", review).put("syncUpdatedAt", revision);
        return true;
    }

    static boolean reconcile(JSONArray following, JSONArray tasks, long now) throws JSONException {
        boolean changed = false;
        for (int index = 0; index < tasks.length(); index++) {
            changed |= reviewFutureCompletion(tasks.getJSONObject(index), now);
        }
        for (int index = 0; index < following.length(); index++) {
            JSONObject entry = following.getJSONObject(index);
            if (!"bangumi".equals(entry.optString("source")) || !(entry.opt("watchedEpisode") instanceof Number)) continue;
            int subject = entry.optInt("id");
            int total = entry.optInt("episodes");
            Set<Integer> valid = new HashSet<>();
            Set<Integer> all = new HashSet<>();
            for (int taskIndex = 0; taskIndex < tasks.length(); taskIndex++) {
                JSONObject task = tasks.getJSONObject(taskIndex);
                int episode = task.optInt("episode");
                if ((task.optInt("subjectId") == subject || task.optInt("animeId") == subject)
                    && ("completed".equals(task.optString("status")) || isReset(task))
                    && episode > 0 && (total <= 0 || episode <= total)) {
                    all.add(episode);
                    if (isCompleted(task)) valid.add(episode);
                }
            }
            int previous = entry.optInt("watchedEpisode");
            if (valid.size() > previous || (valid.size() < all.size() && previous == all.size())) {
                entry.put("watchedEpisode", valid.size())
                    .put("syncUpdatedAt", Math.max(now * 1000L, entry.optLong("syncUpdatedAt") + 1L));
                changed = true;
            }
        }
        return changed;
    }
}
