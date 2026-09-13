package io.anilog.android;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

/**
 * SyncMerge golden 测试（Phase 4 任务 2/5）。
 *
 * 用例与 Rust 权威实现（src-tauri/src/lib.rs merge_document_into_state 及其测试）
 * 对齐：远端较新覆盖、本地较新保留、墓碑删除、复活存活、孤儿 pending 清理、
 * completed 保留；外加 5 MB 拒绝与 version 拒绝。
 */
public class SyncMergeTest {
    @Test
    public void schedulesNeverCrossWebDavButAreRestoredLocally() throws Exception {
        JSONObject original = followed(1, 1000).put("nextEpisode", 6).put("nextAiringAt", 5000)
            .put("nextAiringPrecision", "instant").put("scheduleUpdatedAt", 3000)
            .put("episodeSchedule", new JSONArray().put(new JSONObject().put("episode", 6)));
        JSONObject local = doc(array(original), array(task("1-1", 1, "completed", 4000)), null);
        JSONObject remote = doc(array(followed(1, 2000).put("nextEpisode", 99).put("nextAiringAt", 1)),
            array(task("1-1", 1, "pending", 2000)), null);
        SyncMerge.Result result = SyncMerge.merge(local, remote);
        JSONObject merged = result.merged.getJSONArray("following").getJSONObject(0);
        for (String key : SyncMerge.LOCAL_SCHEDULE_KEYS) assertFalse(merged.has(key));
        assertEquals("completed", result.merged.getJSONArray("tasks").getJSONObject(0).getString("status"));
        SyncMerge.restoreLocalSchedules(result.merged.getJSONArray("following"), local.getJSONArray("following"));
        assertEquals(6, merged.getInt("nextEpisode"));
        assertEquals(3000, merged.getLong("scheduleUpdatedAt"));
        assertEquals(6, original.getInt("nextEpisode"));
        assertFalse(SyncMerge.normalizeDocument(result.merged).getJSONArray("following").getJSONObject(0).has("episodeSchedule"));
    }

    @Test
    public void regeneratedPendingNeverUndoesAWatchButManualUndoStillWins() throws Exception {
        JSONObject completed = task("1-1", 1, "completed", 1000);
        JSONObject regenerated = task("1-1", 1, "pending", 9000).put("statusSource", "airing");
        assertSame(completed, SyncMerge.chooseRecord(completed, regenerated, "createdAt"));
        assertSame(completed, SyncMerge.chooseRecord(regenerated, completed, "createdAt"));
        regenerated.put("statusSource", "local");
        assertSame(regenerated, SyncMerge.chooseRecord(completed, regenerated, "createdAt"));
    }

    private static JSONObject followed(long id, long syncUpdatedAt) throws Exception {
        JSONObject item = new JSONObject();
        item.put("id", id);
        item.put("displayTitle", "Anime " + id);
        item.put("title", new JSONObject().put("native", "Native " + id));
        item.put("followedAt", 1L);
        item.put("syncUpdatedAt", syncUpdatedAt);
        return item;
    }

    private static JSONObject followedWithTitle(long id, long syncUpdatedAt, String title) throws Exception {
        JSONObject item = followed(id, syncUpdatedAt);
        item.put("displayTitle", title);
        return item;
    }

    private static JSONObject task(String id, long animeId, String status, long syncUpdatedAt) throws Exception {
        JSONObject task = new JSONObject();
        task.put("id", id);
        task.put("animeId", animeId);
        task.put("animeTitle", "Anime " + animeId);
        task.put("episode", 1L);
        task.put("airingAt", 10L);
        task.put("status", status);
        task.put("createdAt", 10L);
        task.put("completedAt", status.equals("completed") ? 20L : JSONObject.NULL);
        task.put("syncUpdatedAt", syncUpdatedAt);
        return task;
    }

    private static JSONObject doc(JSONArray following, JSONArray tasks, JSONObject tombstones) throws Exception {
        JSONObject document = new JSONObject();
        document.put("version", 1L);
        document.put("following", following == null ? new JSONArray() : following);
        document.put("tasks", tasks == null ? new JSONArray() : tasks);
        document.put("followingDeletedAt", tombstones == null ? new JSONObject() : tombstones);
        return document;
    }

    private static JSONArray array(Object... items) throws Exception {
        JSONArray array = new JSONArray();
        for (Object item : items) array.put(item);
        return array;
    }

    private static long tombstone(JSONObject merged, long id) {
        return merged.optJSONObject("followingDeletedAt").optLong(String.valueOf(id), 0);
    }

    // ------------------------------------------------------------------
    // golden 1：远端较新任务覆盖本地（LWW）
    // ------------------------------------------------------------------

    @Test
    public void remoteNewerTaskOverwritesLocal() throws Exception {
        JSONObject local = doc(array(followed(1, 1000)), array(task("1-1", 1, "pending", 1000)), null);
        JSONObject remote = doc(
            array(followedWithTitle(1, 1000, "远端标题")),
            array(task("1-1", 1, "pending", 5000)), null);
        remote.optJSONArray("tasks").getJSONObject(0).put("animeTitle", "远端标题");

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        assertEquals("远端标题", result.merged.optJSONArray("tasks").optJSONObject(0).optString("animeTitle"));
        assertEquals(5000, result.merged.optJSONArray("tasks").optJSONObject(0).optLong("syncUpdatedAt"));
        assertTrue(result.localChanged);
        assertFalse(result.remoteDiffers);
    }

    // ------------------------------------------------------------------
    // golden 2：本地较新任务保留（与 Rust newest_task_record_wins_conflict 对齐）
    // ------------------------------------------------------------------

    @Test
    public void localNewerTaskKept() throws Exception {
        JSONObject local = doc(array(followed(1, 3000)), array(task("1-1", 1, "completed", 2000)), null);
        JSONObject remote = doc(array(followed(1, 3000)), array(task("1-1", 1, "pending", 1000)), null);

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        JSONArray tasks = result.merged.optJSONArray("tasks");
        assertEquals(1, tasks.length());
        assertEquals("completed", tasks.optJSONObject(0).optString("status"));
        assertEquals(2000, tasks.optJSONObject(0).optLong("syncUpdatedAt"));
    }

    // ------------------------------------------------------------------
    // golden 3：墓碑删除（与 Rust newer_tombstone_removes_following_and_pending_tasks 对齐）
    // ------------------------------------------------------------------

    @Test
    public void newerTombstoneRemovesFollowingAndPendingTasks() throws Exception {
        JSONObject local = doc(array(followed(1, 1000)), array(task("1-1", 1, "pending", 1000)), null);
        JSONObject remote = doc(null, null, new JSONObject().put("1", 2000L));

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        assertTrue(result.localChanged);
        assertEquals(0, result.merged.optJSONArray("following").length());
        assertEquals(0, result.merged.optJSONArray("tasks").length());
        assertEquals(2000, tombstone(result.merged, 1));
    }

    // ------------------------------------------------------------------
    // golden 4：复活——比墓碑新的记录存活（墓碑并集保留，与 Rust 权威实现一致：
    // 前台用户显式重新追番时才由 mark_following_changed 清墓碑）
    // ------------------------------------------------------------------

    @Test
    public void resurrectionRecordSurvivesTombstone() throws Exception {
        // 本地重新追番（记录时间戳 5000 晚于远端墓碑 2000）。
        JSONObject local = doc(array(followed(1, 5000)), array(task("1-1", 1, "pending", 5000)), null);
        JSONObject remote = doc(null, null, new JSONObject().put("1", 2000L));

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        assertEquals(1, result.merged.optJSONArray("following").length());
        assertEquals(1, result.merged.optJSONArray("tasks").length());
        assertEquals(2000, tombstone(result.merged, 1));
    }

    // ------------------------------------------------------------------
    // golden 5：取消追番作品的 pending 任务剔除
    // ------------------------------------------------------------------

    @Test
    public void orphanPendingTaskDropped() throws Exception {
        JSONObject local = doc(
            array(followed(2, 1000)),
            array(task("1-1", 1, "pending", 1000), task("2-1", 2, "pending", 1000)), null);
        JSONObject remote = doc(null, null, null);

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        JSONArray tasks = result.merged.optJSONArray("tasks");
        assertEquals(1, tasks.length());
        assertEquals("2-1", tasks.optJSONObject(0).optString("id"));
    }

    // ------------------------------------------------------------------
    // golden 6：未追番作品的 completed 任务保留（观看历史）
    // ------------------------------------------------------------------

    @Test
    public void completedTaskPreservedWhenNotFollowed() throws Exception {
        JSONObject local = doc(null, array(task("1-1", 1, "completed", 1000)), null);
        JSONObject remote = doc(null, null, null);

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        JSONArray tasks = result.merged.optJSONArray("tasks");
        assertEquals(1, tasks.length());
        assertEquals("completed", tasks.optJSONObject(0).optString("status"));
    }

    // ------------------------------------------------------------------
    // 附加 golden：墓碑并集取 max + 时间戳毫秒优先 + 秒回落
    // ------------------------------------------------------------------

    @Test
    public void tombstoneUnionTakesMax() throws Exception {
        JSONObject local = doc(null, null, new JSONObject().put("1", 3000L));
        JSONObject remote = doc(null, null, new JSONObject().put("1", 2000L).put("2", 4000L));

        SyncMerge.Result result = SyncMerge.merge(local, remote);

        assertEquals(3000, tombstone(result.merged, 1));
        assertEquals(4000, tombstone(result.merged, 2));
    }

    @Test
    public void recordTimestampPrefersSyncUpdatedAtMillisAndFallsBackToSeconds() throws Exception {
        JSONObject withExplicit = new JSONObject().put("syncUpdatedAt", 2000L).put("followedAt", 9999L);
        JSONObject withFallback = new JSONObject().put("followedAt", 1L);
        assertEquals(2000, SyncMerge.recordTimestamp(withExplicit, "followedAt"));
        assertEquals(1000, SyncMerge.recordTimestamp(withFallback, "followedAt"));
        // 秒×1000 的回落者 vs 显式毫秒：显式毫秒大者胜。
        JSONObject winner = SyncMerge.chooseRecord(withExplicit, withFallback, "followedAt");
        assertSame(withExplicit, winner);
    }

    @Test
    public void missingFallbackTimestampsResolveDeterministically() throws Exception {
        // 双方都无时间戳（MobileStore 投影可能缺字段）：仍需确定性决胜，不抛异常。
        JSONObject left = new JSONObject().put("id", 1L).put("displayTitle", "A");
        JSONObject right = new JSONObject().put("id", 1L).put("displayTitle", "B");
        JSONObject winner = SyncMerge.chooseRecord(left, right, "followedAt");
        assertTrue(winner == left || winner == right);
        // 相同输入两次决胜结果一致。
        assertSame(winner, SyncMerge.chooseRecord(left, right, "followedAt"));
    }

    // ------------------------------------------------------------------
    // 5 MB 拒绝 + version 拒绝
    // ------------------------------------------------------------------

    @Test
    public void oversizedDocumentRejected() throws Exception {
        StringBuilder huge = new StringBuilder();
        for (int index = 0; index < 5 * 1024 * 1024 + 100; index += 1) huge.append('x');
        JSONObject document = new JSONObject();
        document.put("version", 1L);
        document.put("following", new JSONArray());
        document.put("tasks", new JSONArray());
        document.put("followingDeletedAt", new JSONObject());
        document.put("remark", huge.toString());

        try {
            SyncMerge.validateDocument(document.toString());
            fail("超过 5MB 的文档应被拒绝");
        } catch (SyncMerge.MergeException expected) {
            assertTrue(expected.getMessage().contains("5 MB"));
        }
    }

    @Test
    public void unsupportedVersionRejected() throws Exception {
        JSONObject remote = doc(null, null, null).put("version", 2L);
        try {
            SyncMerge.merge(doc(null, null, null), remote);
            fail("version != 1 应被拒绝");
        } catch (SyncMerge.MergeException expected) {
            assertFalse(expected.getMessage().isEmpty());
        }
        JSONObject broken = new JSONObject().put("version", "not-a-number");
        try {
            SyncMerge.normalizeDocument(broken);
            fail("version 非数字应被拒绝");
        } catch (SyncMerge.MergeException expected) {
            // 预期路径
        }
    }
}
