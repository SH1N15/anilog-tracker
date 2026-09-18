package io.anilog.android;

import static org.junit.Assert.*;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public class WatchHistoryTest {
    private static final long NOW = 1789362000L;

    private static JSONObject follow() throws Exception {
        return new JSONObject().put("id", 638497).put("source", "bangumi")
            .put("episodes", 13).put("watchedEpisode", 12).put("syncUpdatedAt", 1000);
    }

    private static JSONObject task(int episode) throws Exception {
        return new JSONObject().put("id", "638497-" + episode).put("animeId", 638497)
            .put("subjectId", 638497).put("episode", episode).put("episodeId", 1700000 + episode)
            .put("status", "completed").put("syncUpdatedAt", 1000).put("completedAt", 100)
            .put("airingAt", episode == 13 ? 1791100800L : 1789286400L)
            .put("airingSource", "bangumi_episode").put("airingPrecision", "instant");
    }

    @Test
    public void futureCompletionDoesNotInflateElevenWatchedEpisodes() throws Exception {
        JSONArray tasks = new JSONArray();
        for (int number = 1; number <= 11; number++) tasks.put(task(number));
        tasks.put(task(13));
        JSONArray follows = new JSONArray().put(follow());
        assertTrue(WatchHistory.reconcile(follows, tasks, NOW));
        assertEquals(11, follows.getJSONObject(0).getInt("watchedEpisode"));
        assertEquals(12, tasks.length());
        assertTrue(WatchHistory.needsReview(tasks.getJSONObject(11)));
        assertEquals(100, tasks.getJSONObject(11).getLong("completedAt"));
        assertFalse(WatchHistory.reconcile(follows, tasks, NOW + 60));
    }

    @Test
    public void currentDayDateOrUnverifiedTimeDoesNotInvalidateACompletion() throws Exception {
        JSONObject task = task(13).put("airingAt", NOW / 86400L * 86400L).put("airingPrecision", "date");
        assertFalse(WatchHistory.reviewFutureCompletion(task, NOW));
        task = task(13).put("airingSource", "offline");
        assertFalse(WatchHistory.reviewFutureCompletion(task, NOW));
    }

    @Test
    public void futureEvidenceFromEpisodeTableQuarantinesWithoutRemovingHistory() throws Exception {
        JSONObject task = task(13).put("airingAt", 100).put("airingSource", "offline");
        JSONObject row = new JSONObject().put("episode", 13).put("episodeId", 1700013)
            .put("airingAt", 1791100800L).put("airingPrecision", "instant");
        JSONArray tasks = EpisodeSchedule.reconcileTasks(follow(), new JSONArray().put(task),
            new JSONArray().put(row), false, NOW);
        assertEquals(1, tasks.length());
        assertEquals("completed", tasks.getJSONObject(0).getString("status"));
        assertEquals(100, tasks.getJSONObject(0).getLong("completedAt"));
        assertTrue(WatchHistory.needsReview(tasks.getJSONObject(0)));
    }

    @Test
    public void explicitResetSurvivesOldSnapshotAndWaitsForAiring() throws Exception {
        JSONObject old = task(13);
        JSONObject reset = new JSONObject(old.toString());
        WatchHistory.reviewFutureCompletion(reset, NOW);
        reset.getJSONObject("completionReview").put("decision", "reset").put("resolvedAt", NOW * 1000L + 1);
        reset.put("status", "pending").put("completedAt", JSONObject.NULL)
            .put("statusSource", "local").put("syncUpdatedAt", NOW * 1000L + 1);
        for (boolean reverse : new boolean[] {false, true}) {
            JSONObject merged = SyncMerge.chooseRecord(reverse ? old : reset, reverse ? reset : old, "createdAt");
            assertTrue(WatchHistory.isReset(merged));
            JSONArray rows = new JSONArray().put(new JSONObject().put("episode", 13).put("episodeId", 1700013)
                .put("airingAt", 1791100800L).put("airingPrecision", "instant"));
            JSONArray tasks = EpisodeSchedule.reconcileTasks(follow(), new JSONArray().put(merged), rows, false, NOW);
            assertEquals(1, tasks.length());
            assertFalse(WatchHistory.isPending(tasks.getJSONObject(0), NOW));
            assertTrue(WatchHistory.isPending(tasks.getJSONObject(0), 1791100800L));
        }
    }

    @Test
    public void partialHistoryCannotLowerAnUnrelatedRemoteTotal() throws Exception {
        JSONArray follows = new JSONArray().put(follow().put("watchedEpisode", 8));
        JSONArray tasks = new JSONArray().put(task(13));
        WatchHistory.reconcile(follows, tasks, NOW);
        assertEquals(8, follows.getJSONObject(0).getInt("watchedEpisode"));
    }

    @Test
    public void reviewPersistsAfterAiringUnlessUserConfirms() throws Exception {
        JSONObject task = task(13);
        WatchHistory.reviewFutureCompletion(task, NOW);
        assertFalse(WatchHistory.reviewFutureCompletion(task, 1791100801L));
        assertFalse(WatchHistory.isCompleted(task));
        task.getJSONObject("completionReview").put("decision", "keep");
        assertFalse(WatchHistory.reviewFutureCompletion(task, NOW));
        assertTrue(WatchHistory.isCompleted(task));
    }
}
