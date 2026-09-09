package io.anilog.android;

import org.json.JSONArray;
import org.json.JSONObject;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;

/**
 * 坚果云同步文档的 Java 版 LWW 合并（Phase 4 任务 2）。
 *
 * 规则移植自 src-tauri/src/lib.rs 的 {@code merge_document_into_state} /
 * {@code document_from_state} / {@code normalize_document} / {@code choose_record}
 * （坚果云 LWW+墓碑规则的权威定义）：
 * <ul>
 *   <li>文档固定 {@code {version:1, updatedAt, following[], tasks[], followingDeletedAt{}}}，
 *       version != 1 直接拒绝（normalize_document 同义）；</li>
 *   <li>LWW 按 record_timestamp：显式 {@code syncUpdatedAt}（毫秒）优先，否则回落
 *       followedAt/createdAt（秒）× 1000；时间戳相同时按键排序后的规范化 JSON 串
 *       字典序决胜（stable_record 同义），保证两端结果一致；</li>
 *   <li>墓碑并集取 max；记录时间戳 &lt;= 墓碑则剔除（resurrection：新记录时间戳
 *       更晚时记录存活，墓碑并集仍保留，与 Rust 权威实现一致——前台用户显式
 *       重新追番时才由 mark_following_changed 清墓碑）；</li>
 *   <li>取消追番作品的 pending 任务剔除，completed 任务作为观看历史保留；</li>
 *   <li>5 MB 上限（与 WebDavClient 下载侧一致，上传前再校验一次）。</li>
 * </ul>
 *
 * 与 Rust 的刻意差异（本地状态来源不同）：本地文档由 MobileStore 投影构建，
 * following 记录可能没有 Rust 状态里的 {@code title} 对象，因此 normalize 只要求
 * {@code id > 0}（Rust 还要求 title 是对象）。远端文档由 Rust 产出、必然满足更严
 * 过滤，故该放宽不会把不合格记录写回远端；合并结果写回 MobileStore 供前台
 * Rust 侧细化合并。
 */
final class SyncMerge {
    static final int SYNC_VERSION = 1;
    static final long MAX_DOCUMENT_BYTES = 5L * 1024L * 1024L;

    private SyncMerge() {}

    /** 合并结果：merged 为合并后的文档；localChanged 表示本地状态需更新；
     *  remoteDiffers 表示远端缺本地内容，需要 PUT。 */
    static final class Result {
        final JSONObject merged;
        final boolean localChanged;
        final boolean remoteDiffers;

        Result(JSONObject merged, boolean localChanged, boolean remoteDiffers) {
            this.merged = merged;
            this.localChanged = localChanged;
            this.remoteDiffers = remoteDiffers;
        }
    }

    /** 合并/校验失败（版本不支持、超 5 MB、JSON 形状非法）。消息绝不含凭据。 */
    static final class MergeException extends Exception {
        MergeException(String message) { super(message); }
    }

    // ------------------------------------------------------------------
    // 校验 + normalize（lib.rs normalize_document / document_from_state）
    // ------------------------------------------------------------------

    /** 上传前/合并前校验：5 MB 上限 + version 必须 == 1。 */
    static void validateDocument(String body) throws MergeException {
        if (body != null && body.getBytes(java.nio.charset.StandardCharsets.UTF_8).length > MAX_DOCUMENT_BYTES) {
            throw new MergeException("WebDAV 同步文件超过 5 MB，已停止同步");
        }
    }

    static JSONObject normalizeDocument(JSONObject document) throws MergeException, org.json.JSONException {
        if (document == null) throw new MergeException("同步文档为空");
        if (document.optLong("version", -1) != SYNC_VERSION) {
            throw new MergeException("WebDAV 同步文件版本不受支持");
        }
        JSONObject normalized = new JSONObject();
        normalized.put("version", SYNC_VERSION);

        JSONArray following = new JSONArray();
        JSONArray rawFollowing = document.optJSONArray("following");
        if (rawFollowing != null) {
            for (int index = 0; index < rawFollowing.length(); index += 1) {
                JSONObject item = rawFollowing.optJSONObject(index);
                if (item != null && item.optLong("id", 0) > 0) following.put(item);
            }
        }
        following = sortByFollowingId(following);
        normalized.put("following", following);

        JSONArray tasks = new JSONArray();
        JSONArray rawTasks = document.optJSONArray("tasks");
        if (rawTasks != null) {
            for (int index = 0; index < rawTasks.length(); index += 1) {
                JSONObject task = rawTasks.optJSONObject(index);
                if (task == null) continue;
                if (task.optString("id").isEmpty()) continue;
                long animeId = task.optLong("animeId", parseAnimeIdFromTaskId(task.optString("id")));
                if (animeId <= 0) continue;
                if (task.optLong("episode", 0) <= 0) continue;
                String status = task.optString("status", "pending");
                if (!"pending".equals(status) && !"completed".equals(status)) continue;
                tasks.put(task);
            }
        }
        tasks = sortByTaskId(tasks);
        normalized.put("tasks", tasks);

        JSONObject deleted = new JSONObject();
        for (Map.Entry<String, Long> entry : tombstones(document.optJSONObject("followingDeletedAt")).entrySet()) {
            deleted.put(entry.getKey(), entry.getValue());
        }
        normalized.put("followingDeletedAt", deleted);
        normalized.put("updatedAt", documentUpdatedAt(following, tasks, deleted));
        return normalized;
    }

    // ------------------------------------------------------------------
    // 合并主体（lib.rs merge_document_into_state）
    // ------------------------------------------------------------------

    static Result merge(JSONObject localDocument, JSONObject remoteDocument) throws MergeException, org.json.JSONException {
        JSONObject local = normalizeDocument(localDocument);
        JSONObject remote = normalizeDocument(remoteDocument);

        // 墓碑并集取 max。
        Map<String, Long> deleted = tombstones(local.optJSONObject("followingDeletedAt"));
        for (Map.Entry<String, Long> entry : tombstones(remote.optJSONObject("followingDeletedAt")).entrySet()) {
            Long existing = deleted.get(entry.getKey());
            deleted.put(entry.getKey(), existing == null ? entry.getValue() : Math.max(existing, entry.getValue()));
        }

        // following LWW + 墓碑剔除。
        Map<Long, JSONObject> localFollowing = indexFollowing(local.optJSONArray("following"));
        Map<Long, JSONObject> remoteFollowing = indexFollowing(remote.optJSONArray("following"));
        Set<Long> ids = new TreeSet<>(localFollowing.keySet());
        ids.addAll(remoteFollowing.keySet());
        for (String id : deleted.keySet()) {
            try { ids.add(Long.parseLong(id)); } catch (NumberFormatException ignored) {}
        }
        JSONArray following = new JSONArray();
        for (Long id : ids) {
            JSONObject localCandidate = localFollowing.get(id);
            long localCandidateIntent = localCandidate == null
                ? 0 : localCandidate.optLong("localFollowIntentAt", 0);
            long localCandidateRemoteAt = localCandidate == null
                ? 0 : localCandidate.optLong("lastPulledFromBangumiAt", 0);
            long tombstoneAt = deleted.containsKey(String.valueOf(id))
                ? deleted.get(String.valueOf(id)) : 0L;
            boolean localRefollow = localCandidate != null
                && localCandidateIntent > 0
                && (localCandidateIntent > tombstoneAt
                    || localCandidateRemoteAt <= 0
                    || localCandidateIntent > localCandidateRemoteAt * 1000L);
            JSONObject winner = localRefollow
                ? localCandidate
                : chooseRecord(localCandidate, remoteFollowing.get(id), "followedAt");
            if (winner == null) continue;
            long timestamp = recordTimestamp(winner, "followedAt");
            Long tombstone = deleted.get(String.valueOf(id));
            if (tombstone == null) tombstone = 0L;
            long localIntentAt = winner.optLong("localFollowIntentAt", 0);
            long remotePulledAt = winner.optLong("lastPulledFromBangumiAt", 0);
            if (localIntentAt > 0
                && (localIntentAt > tombstone
                    || remotePulledAt <= 0
                    || localIntentAt > remotePulledAt * 1000L)) {
                // A local re-follow is an explicit fresh intent.  Ignore and
                // clear stale WebDAV tombstones so Android converges with Rust.
                deleted.remove(String.valueOf(id));
                following.put(winner);
            } else if (timestamp > tombstone) {
                following.put(winner);
            }
        }
        following = sortByFollowingId(following);

        // tasks LWW；未追番作品的 pending 剔除；completed 保留；animeTitle 对齐追番。
        Map<String, JSONObject> localTasks = indexTasks(local.optJSONArray("tasks"));
        Map<String, JSONObject> remoteTasks = indexTasks(remote.optJSONArray("tasks"));
        Set<String> taskIds = new TreeSet<>(localTasks.keySet());
        taskIds.addAll(remoteTasks.keySet());
        Map<Long, String> followedTitles = new HashMap<>();
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject item = following.optJSONObject(index);
            if (item != null) followedTitles.put(item.optLong("id"), item.optString("displayTitle"));
        }
        List<JSONObject> mergedTasks = new ArrayList<>();
        for (String taskId : taskIds) {
            JSONObject winner = chooseRecord(localTasks.get(taskId), remoteTasks.get(taskId), "createdAt");
            if (winner == null) continue;
            long animeId = winner.optLong("animeId", parseAnimeIdFromTaskId(winner.optString("id")));
            boolean pending = "pending".equals(winner.optString("status", "pending"));
            if (!followedTitles.containsKey(animeId) && pending) continue;
            String title = followedTitles.get(animeId);
            if (title != null && !title.isEmpty()) {
                try { winner.put("animeTitle", title); } catch (Exception ignored) {}
            }
            mergedTasks.add(winner);
        }
        // 播出时刻倒序，同刻按 id 升序（与 Rust 排序一致）。
        mergedTasks.sort((left, right) -> {
            long leftAiring = left.optLong("airingAt", 0);
            long rightAiring = right.optLong("airingAt", 0);
            if (leftAiring != rightAiring) return Long.compare(rightAiring, leftAiring);
            return left.optString("id").compareTo(right.optString("id"));
        });
        JSONArray tasks = new JSONArray();
        for (JSONObject task : mergedTasks) tasks.put(task);

        JSONObject merged = new JSONObject();
        merged.put("version", SYNC_VERSION);
        merged.put("following", following);
        merged.put("tasks", tasks);
        JSONObject deletedJson = new JSONObject();
        for (Map.Entry<String, Long> entry : new TreeMap<>(deleted).entrySet()) {
            deletedJson.put(entry.getKey(), entry.getValue());
        }
        merged.put("followingDeletedAt", deletedJson);
        merged.put("updatedAt", documentUpdatedAt(following, tasks, deletedJson));

        boolean localChanged = !businessKey(local).equals(businessKey(merged));
        boolean remoteDiffers = !businessKey(remote).equals(businessKey(merged));
        return new Result(merged, localChanged, remoteDiffers);
    }

    // ------------------------------------------------------------------
    // 时间戳 / 决胜 / 排序辅助（lib.rs record_timestamp / choose_record / stable_record）
    // ------------------------------------------------------------------

    /** syncUpdatedAt（毫秒）优先，否则 fallback（秒）× 1000。 */
    static long recordTimestamp(JSONObject record, String fallback) {
        long explicit = record.optLong("syncUpdatedAt", 0);
        if (explicit > 0) return explicit;
        return record.optLong(fallback, 0) * 1000L;
    }

    /** 时间戳相同则按键排序的规范化 JSON 字典序决胜，保证两端一致。 */
    static JSONObject chooseRecord(JSONObject left, JSONObject right, String fallback) {
        if (left == null && right == null) return null;
        if (left == null) return right;
        if (right == null) return left;
        long leftTime = recordTimestamp(left, fallback);
        long rightTime = recordTimestamp(right, fallback);
        if (leftTime != rightTime) return leftTime > rightTime ? left : right;
        return stableRecord(left).compareTo(stableRecord(right)) >= 0 ? left : right;
    }

    /** 键排序后的规范化 JSON（stable_record 同义；org.json 迭代顺序不稳定，需自行排序）。 */
    static String stableRecord(JSONObject record) {
        return canonical(record);
    }

    /** 业务三字段的可比较串（comparable_document 同义）。 */
    private static String businessKey(JSONObject document) throws org.json.JSONException {
        JSONObject business = new JSONObject();
        business.put("following", document.optJSONArray("following"));
        business.put("tasks", document.optJSONArray("tasks"));
        business.put("followingDeletedAt", document.optJSONObject("followingDeletedAt"));
        return canonical(business);
    }

    /** 比较同步业务字段；updatedAt 等派生字段不参与。 */
    static boolean sameBusinessDocument(JSONObject left, JSONObject right) throws MergeException, org.json.JSONException {
        return businessKey(normalizeDocument(left)).equals(businessKey(normalizeDocument(right)));
    }

    private static String canonical(Object value) {
        if (value instanceof JSONObject) {
            JSONObject object = (JSONObject) value;
            StringBuilder builder = new StringBuilder("{");
            List<String> names = namesOf(object);
            Collections.sort(names);
            for (int index = 0; index < names.size(); index += 1) {
                if (index > 0) builder.append(',');
                builder.append(quote(names.get(index))).append(':').append(canonical(object.opt(names.get(index))));
            }
            return builder.append('}').toString();
        }
        if (value instanceof JSONArray) {
            JSONArray array = (JSONArray) value;
            StringBuilder builder = new StringBuilder("[");
            for (int index = 0; index < array.length(); index += 1) {
                if (index > 0) builder.append(',');
                builder.append(canonical(array.opt(index)));
            }
            return builder.append(']').toString();
        }
        if (value instanceof String) return quote((String) value);
        return String.valueOf(value);
    }

    private static String quote(String value) {
        StringBuilder builder = new StringBuilder("\"");
        for (int index = 0; index < value.length(); index += 1) {
            char ch = value.charAt(index);
            if (ch == '"' || ch == '\\') builder.append('\\');
            builder.append(ch);
        }
        return builder.append('"').toString();
    }

    // ------------------------------------------------------------------
    // 形状辅助
    // ------------------------------------------------------------------

    /** 墓碑解析：仅保留 id 解析为正整数且时间戳为正的条目（tombstones 同义）。 */
    static Map<String, Long> tombstones(JSONObject value) {
        Map<String, Long> result = new TreeMap<>();
        if (value == null) return result;
        for (String id : namesOf(value)) {
            long idNumber;
            try { idNumber = Long.parseLong(id); } catch (NumberFormatException ignored) { continue; }
            long timestamp = value.optLong(id, 0);
            if (idNumber > 0 && timestamp > 0) result.put(String.valueOf(idNumber), timestamp);
        }
        return result;
    }

    /** android.jar 的 org.json 无 keySet()，统一用 names()（null 安全）。 */
    static List<String> namesOf(JSONObject object) {
        List<String> names = new ArrayList<>();
        JSONArray raw = object.names();
        if (raw == null) return names;
        for (int index = 0; index < raw.length(); index += 1) {
            String name = raw.optString(index, null);
            if (name != null) names.add(name);
        }
        return names;
    }

    /** 文档 updatedAt：following/tasks 回落时间戳与墓碑值的最大值。 */
    static long documentUpdatedAt(JSONArray following, JSONArray tasks, JSONObject deleted) {
        long updatedAt = 0;
        if (following != null) {
            for (int index = 0; index < following.length(); index += 1) {
                JSONObject item = following.optJSONObject(index);
                if (item != null) updatedAt = Math.max(updatedAt, recordTimestamp(item, "followedAt"));
            }
        }
        if (tasks != null) {
            for (int index = 0; index < tasks.length(); index += 1) {
                JSONObject task = tasks.optJSONObject(index);
                if (task != null) updatedAt = Math.max(updatedAt, recordTimestamp(task, "createdAt"));
            }
        }
        if (deleted != null) {
            for (Long timestamp : tombstones(deleted).values()) updatedAt = Math.max(updatedAt, timestamp);
        }
        return updatedAt;
    }

    static JSONObject emptyDocument() throws org.json.JSONException {
        JSONObject document = new JSONObject();
        document.put("version", SYNC_VERSION);
        document.put("updatedAt", 0L);
        document.put("following", new JSONArray());
        document.put("tasks", new JSONArray());
        document.put("followingDeletedAt", new JSONObject());
        return document;
    }

    private static Map<Long, JSONObject> indexFollowing(JSONArray following) {
        Map<Long, JSONObject> map = new HashMap<>();
        if (following == null) return map;
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject item = following.optJSONObject(index);
            if (item != null) map.put(item.optLong("id"), item);
        }
        return map;
    }

    private static Map<String, JSONObject> indexTasks(JSONArray tasks) {
        Map<String, JSONObject> map = new HashMap<>();
        if (tasks == null) return map;
        for (int index = 0; index < tasks.length(); index += 1) {
            JSONObject task = tasks.optJSONObject(index);
            if (task != null) map.put(task.optString("id"), task);
        }
        return map;
    }

    /** 任务 id "{animeId}-{episode}" → animeId（MobileStore 投影任务缺 animeId 时的回落）。 */
    static long parseAnimeIdFromTaskId(String id) {
        int separator = id == null ? -1 : id.indexOf('-');
        if (separator <= 0) return 0;
        try { return Long.parseLong(id.substring(0, separator)); } catch (NumberFormatException ignored) {}
        return 0;
    }

    private static JSONArray sortByFollowingId(JSONArray following) {
        List<JSONObject> items = new ArrayList<>();
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject item = following.optJSONObject(index);
            if (item != null) items.add(item);
        }
        items.sort((left, right) -> Long.compare(left.optLong("id"), right.optLong("id")));
        JSONArray sorted = new JSONArray();
        for (JSONObject item : items) sorted.put(item);
        return sorted;
    }

    private static JSONArray sortByTaskId(JSONArray tasks) {
        List<JSONObject> items = new ArrayList<>();
        for (int index = 0; index < tasks.length(); index += 1) {
            JSONObject task = tasks.optJSONObject(index);
            if (task != null) items.add(task);
        }
        items.sort((left, right) -> left.optString("id").compareTo(right.optString("id")));
        JSONArray sorted = new JSONArray();
        for (JSONObject task : items) sorted.put(task);
        return sorted;
    }
}
