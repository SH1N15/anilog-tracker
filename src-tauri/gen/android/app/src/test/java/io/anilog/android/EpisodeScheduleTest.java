package io.anilog.android;

import static org.junit.Assert.*;
import java.time.OffsetDateTime;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public class EpisodeScheduleTest {
    @Test
    public void originalRejectsBangumiBeforeAccessingStorageOrNetwork() throws Exception {
        if (BuildConfig.isOriginalEdition) {
            assertEquals(0, AniListScheduler.syncBangumiEpisodeSchedules(null));
            AniListScheduler.refreshCachedSchedules(null);
        }
    }

    private static long at(String value) { return OffsetDateTime.parse(value).toEpochSecond(); }

    private static JSONObject follow(int id) throws Exception {
        return new JSONObject().put("id", id).put("source", "bangumi").put("anilistId", 189046)
            .put("displayTitle", "Test season").put("bangumiStatus", "doing").put("episodes", 8)
            .put("followedAt", at("2026-09-09T12:00:00+08:00"));
    }

    private static JSONObject episode(int number, int sort, String date) throws Exception {
        return new JSONObject().put("id", 1000 + number).put("ep", number).put("sort", sort).put("airdate", date).put("type", 0);
    }

    private static JSONObject media(int number, String date, long fetched) throws Exception {
        return new JSONObject().put("id", 189046).put("_fetchedAt", fetched)
            .put("nextAiringEpisode", new JSONObject().put("episode", number).put("airingAt", at(date)));
    }

    private static JSONObject pending(int subject, int number, long time) throws Exception {
        return new JSONObject().put("id", subject + "-" + number).put("subjectId", subject)
            .put("animeId", subject).put("episode", number).put("airingAt", time)
            .put("status", "pending").put("createdAt", time).put("syncUpdatedAt", time * 1000);
    }

    @Test
    public void dateOnlyMorningDoesNotNotifyOrCreateTonightsTask() throws Exception {
        long now = at("2026-09-10T09:33:00+08:00");
        for (int subject : new int[] {622206, 571784, 583729}) {
            JSONObject follow = follow(subject).put("episodes", 13);
            JSONArray rows = EpisodeSchedule.resolve(follow,
                new JSONArray().put(episode(10, 10, "2026-09-10")), null, now);
            EpisodeSchedule.updateNext(follow, rows, now);
            assertFalse(EpisodeSchedule.canNotify(follow, 10, rows.getJSONObject(0).getLong("airingAt"), now));
            assertEquals(0, EpisodeSchedule.reconcileTasks(follow, new JSONArray(), rows, true, now).length());
            assertEquals("date", follow.getString("nextAiringPrecision"));
            assertEquals(10, follow.getInt("nextEpisode"));
        }
    }

    @Test
    public void splitEpisodeSurvivesLocalMidnightAndRepeatedRefresh() throws Exception {
        long now = at("2026-09-10T01:00:00+08:00");
        JSONObject follow = follow(633836).put("watchedEpisode", 4);
        JSONArray episodes = new JSONArray().put(episode(5, 82, "2026-09-09")).put(episode(6, 83, "2026-09-16"));
        JSONObject media = media(16, "2026-09-09T21:00:00+08:00", now - 14400);
        JSONArray rows = EpisodeSchedule.resolve(follow, episodes, media, now);
        assertTrue(EpisodeSchedule.aired(rows.getJSONObject(0), now));
        JSONArray tasks = EpisodeSchedule.reconcileTasks(follow, new JSONArray(), rows, true, now);
        assertEquals(1, tasks.length());
        assertEquals("633836-5", tasks.getJSONObject(0).getString("id"));
        EpisodeSchedule.updateNext(follow, rows, now);
        assertEquals(6, follow.getInt("nextEpisode"));
        JSONArray offline = EpisodeSchedule.resolve(follow, episodes, null, now + 3600);
        assertTrue(EpisodeSchedule.precise(offline.getJSONObject(0)));
        JSONArray refreshed = EpisodeSchedule.reconcileTasks(follow, tasks, offline, true, now + 3600);
        assertEquals(SyncMerge.stableRecord(tasks.getJSONObject(0)), SyncMerge.stableRecord(refreshed.getJSONObject(0)));
    }

    @Test
    public void sameDayWithoutPrecisionDoesNotEraseExistingTask() throws Exception {
        long now = at("2026-09-10T01:00:00+08:00");
        JSONObject follow = follow(633836);
        JSONArray rows = EpisodeSchedule.resolve(follow, new JSONArray().put(episode(5, 82, "2026-09-09")), null, now);
        JSONObject task = pending(633836, 5, at("2026-09-09T21:00:00+08:00"));
        assertFalse(EpisodeSchedule.future(rows.getJSONObject(0), now));
        assertFalse(EpisodeSchedule.aired(rows.getJSONObject(0), now));
        assertEquals(1, EpisodeSchedule.reconcileTasks(follow, new JSONArray().put(task), rows, false, now).length());
    }

    @Test
    public void sharedAnilistIdCannotBindAnotherPartsLocalEpisodeNumber() throws Exception {
        long now = at("2026-09-10T09:00:00+08:00");
        JSONObject media = media(5, "2026-05-06T21:00:00+08:00", now);
        JSONArray rows = EpisodeSchedule.resolve(follow(633836),
            new JSONArray().put(episode(5, 82, "2026-09-09")), media, now);
        assertFalse(EpisodeSchedule.precise(rows.getJSONObject(0)));
        assertEquals(at("2026-09-09T00:00:00Z"), rows.getJSONObject(0).getLong("airingAt"));
    }

    @Test
    public void preciseFutureRetractsFalsePendingButPreservesCompletedHistory() throws Exception {
        long morning = at("2026-09-10T09:33:00+08:00");
        long evening = at("2026-09-10T23:30:00+08:00");
        JSONObject follow = follow(622206).put("episodes", 12);
        JSONArray episodes = new JSONArray().put(episode(10, 10, "2026-09-10"));
        JSONArray rows = EpisodeSchedule.resolve(follow, episodes,
            media(10, "2026-09-10T23:30:00+08:00", morning), morning);
        JSONObject falseTask = pending(622206, 10, at("2026-09-10T00:00:00Z"));
        assertEquals(0, EpisodeSchedule.reconcileTasks(follow, new JSONArray().put(falseTask), rows, true, morning).length());
        assertEquals(1, EpisodeSchedule.reconcileTasks(follow, new JSONArray(), rows, true, evening).length());
        falseTask.put("status", "completed").put("completedAt", morning);
        assertEquals(falseTask.toString(),
            EpisodeSchedule.reconcileTasks(follow, new JSONArray().put(falseTask), rows, true, morning).getJSONObject(0).toString());
    }

    @Test
    public void disabledTaskCreationDoesNotDisableVerifiedNotifications() throws Exception {
        long now = at("2026-09-09T21:00:00+08:00");
        JSONObject follow = follow(633836);
        JSONArray rows = EpisodeSchedule.resolve(follow, new JSONArray().put(episode(5, 82, "2026-09-09")),
            media(16, "2026-09-09T21:00:00+08:00", now - 60), now);
        follow.put("episodeSchedule", rows);
        assertTrue(EpisodeSchedule.canNotify(follow, 5, now, now));
        assertEquals(0, EpisodeSchedule.reconcileTasks(follow, new JSONArray(), rows, false, now).length());
        assertFalse(EpisodeSchedule.canNotify(follow.put("bangumiStatus", "done"), 5, now, now));
    }

    @Test
    public void precisionRetentionDoesNotExtendItsOwnExpiry() throws Exception {
        long fetched = at("2026-09-09T12:00:00+08:00");
        JSONObject follow = follow(633836);
        JSONArray episodes = new JSONArray().put(episode(6, 83, "2026-09-16"));
        JSONArray rows = EpisodeSchedule.resolve(follow, episodes, media(17, "2026-09-16T21:00:00+08:00", fetched), fetched);
        EpisodeSchedule.updateNext(follow, rows, fetched);
        rows = EpisodeSchedule.resolve(follow, episodes, null, fetched + 86400);
        EpisodeSchedule.updateNext(follow, rows, fetched + 86400);
        assertFalse(EpisodeSchedule.precise(EpisodeSchedule.resolve(follow, episodes, null, fetched + 8 * 86400).getJSONObject(0)));
    }

    @Test
    public void olderSnapshotCannotOverwriteNewerPreciseTime() throws Exception {
        long now = at("2026-09-09T12:00:00+08:00");
        JSONObject follow = follow(633836);
        JSONArray episodes = new JSONArray().put(episode(5, 82, "2026-09-09"));
        JSONArray rows = EpisodeSchedule.resolve(follow, episodes,
            media(16, "2026-09-09T21:00:00+08:00", now), now);
        EpisodeSchedule.updateNext(follow, rows, now);
        JSONArray stale = EpisodeSchedule.resolve(follow, episodes,
            media(16, "2026-09-09T20:00:00+08:00", now - 60), now + 1);
        assertEquals(at("2026-09-09T21:00:00+08:00"), stale.getJSONObject(0).getLong("airingAt"));
        assertEquals(now, stale.getJSONObject(0).getLong("precisionFetchedAt"));
    }
}
