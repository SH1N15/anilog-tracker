package io.anilog.android;

import android.content.Context;
import androidx.annotation.NonNull;
import androidx.work.Worker;
import androidx.work.WorkerParameters;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;
import java.io.IOException;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * Android 完整后台同步 Worker（Phase 4 任务 1）。
 *
 * 背景：Rust 的 15 分钟 WebDAV 循环随进程死亡而停止（“Android 放几天失同步”的
 * 根因），且 Rust/Tauri runtime 在后台进程死亡后不可用。因此七步全部在 Java 侧
 * 完成，由 WorkManager 周期驱动（默认 6h，NetworkType.CONNECTED，指数退避）。
 *
 * 七步顺序由 {@link SyncPlan} 定义；单步失败只记摘要到 lastSyncError（绝不含
 * token），不阻断其余步骤；存在失败且未达重试上限时 Result.retry()（上限
 * MAX_ATTEMPTS，退避由 BackgroundSync 设置的 BackoffPolicy.EXPONENTIAL 驱动）。
 */
public class BackgroundSyncWorker extends Worker {
    /** 重试上限（WorkManager 运行次数：首次 + 重试 2 次）。 */
    public static final int MAX_ATTEMPTS = 3;

    public BackgroundSyncWorker(@NonNull Context context, @NonNull WorkerParameters params) {
        super(context, params);
    }

    @NonNull
    @Override
    public Result doWork() {
        Context context = getApplicationContext();
        List<String> errors = new ArrayList<>();
        Set<Integer> removedFollowIds = currentFollowIds(context);

        boolean webDavEnabled = WebDavStore.load(context).enabled;
        boolean bangumiPull = !BuildConfig.isOriginalEdition
            && BangumiTokenStore.load(context) != null
            && MobileStore.pullCollectionsEnabled(context);
        List<SyncPlan.Step> steps = SyncPlan.steps(BuildConfig.isOriginalEdition, webDavEnabled, bangumiPull);

        for (SyncPlan.Step step : steps) {
            try {
                runStep(context, step, removedFollowIds);
            } catch (Throwable error) {
                // 单步失败不阻断其余步骤；摘要净化后入库，绝不包含 token/凭据。
                String message = error.getMessage() == null || error.getMessage().isEmpty()
                    ? error.getClass().getSimpleName() : error.getMessage();
                errors.add(step.name() + ": " + sanitize(message));
            }
        }

        // 第 6 步兜底：lastSyncError 汇总 + lastFullSyncAt（仅本地，绝不进 WebDAV 文档）。
        MobileStore.setLastSyncError(context, joinErrors(errors));
        if (errors.isEmpty()) {
            MobileStore.setLastFullSyncAt(context, System.currentTimeMillis() / 1000L);
        }

        if (!errors.isEmpty() && getRunAttemptCount() + 1 < MAX_ATTEMPTS) {
            return Result.retry();
        }
        return Result.success();
    }

    private void runStep(Context context, SyncPlan.Step step, Set<Integer> removedFollowIds) throws Exception {
        switch (step) {
            case WEBDAV_MERGE:
                syncWebDav(context, removedFollowIds);
                break;
            case MASTER_DATA_AND_AIRING_REFRESH:
                // anilistId 条目补充（nextAiringEpisode/封面）。standard 的 Bangumi
                // 拉取在第 4 步（Java 层唯一 Bangumi 网络步骤）。
                AniListScheduler.sync(context);
                MobileStore.setLastScheduleSyncAt(context, System.currentTimeMillis() / 1000L);
                break;
            case NOTIFICATION_AND_TASK_DEDUPE:
                // 合并后的 airing 事件按 delivered/seen 机制去重（MobileStore.addAiredEvent）；
                // 重排前先撤销已不在追番列表的闹钟。
                for (Integer animeId : removedFollowIds) {
                    NotificationScheduler.cancel(context, animeId);
                }
                NotificationScheduler.scheduleAll(context);
                break;
            case BANGUMI_COLLECTION_MARK:
                // 后台只标记不破坏：建议追番/建议取消/ep_status 写入 MobileStore，
                // 真实合并（追番增删、hash 冲突、墓碑保护）由前台 Rust run_full_sync 完成。
                BangumiSync.pull(context);
                MobileStore.setLastBangumiSyncAt(context, System.currentTimeMillis() / 1000L);
                break;
            case WRITE_BACK_GUARD:
                // 显式空步骤：后台绝不执行写回（写回只在用户显式动作 / 前台
                // run_full_sync 进行，避免后台误写远端 Bangumi 状态）。
                break;
            case LOCAL_SYNC_STATUS:
                // 主体在 doWork 末尾统一写（lastFullSyncAt/lastSyncError）；
                // 各步已写 lastWebDavSyncAt/lastScheduleSyncAt/lastBangumiSyncAt。
                break;
            case RESCHEDULE:
                BackgroundSync.schedulePeriodic(context);
                NotificationScheduler.scheduleAll(context);
                break;
        }
    }

    // ------------------------------------------------------------------
    // 第 1 步：坚果云拉取 → Java 版 LWW 合并 → 本地写回 → 需要时 PUT
    // ------------------------------------------------------------------

    private static void syncWebDav(Context context, Set<Integer> removedFollowIds) throws Exception {
        WebDavStore.Config config = WebDavStore.load(context);
        if (!config.enabled) return; // 未启用坚果云：跳过（不算失败）

        WebDavClient client = new WebDavClient();
        WebDavClient.Download download = client.download(config);
        Set<Integer> before = currentFollowIds(context);
        SyncMerge.Result result;
        if (download.found) {
            String body = download.body;
            SyncMerge.validateDocument(body);
            result = MobileStore.mergeDocument(context, new JSONObject(body));
        } else {
            // 远端无文档：视作空文档合并（远端必然缺本地内容 → 首次上传）。
            result = MobileStore.mergeDocument(context, SyncMerge.emptyDocument());
        }

        removedFollowIds.addAll(before);
        removedFollowIds.removeAll(currentFollowIds(context));

        // 云端文档可能还带着旧的未来假票。先合并，再用 Bangumi episode
        // 表清理，最后重新投影上传；这样 AniList 暂停或分季错位都不会让
        // 错误任务在 Android 端重新写回坚果云。
        JSONObject mergedDocument = result.merged;
        boolean remoteDiffers = result.remoteDiffers;
        if (!BuildConfig.isOriginalEdition) {
            try {
                AniListScheduler.syncBangumiEpisodeSchedules(context);
                mergedDocument = localDocument(context);
                if (download.found) {
                    SyncMerge.validateDocument(download.body);
                    remoteDiffers = !SyncMerge.sameBusinessDocument(mergedDocument, new JSONObject(download.body));
                } else {
                    remoteDiffers = true;
                }
            } catch (IOException | JSONException ignored) {
                // Bangumi 单次失败不应抹掉已完成的同步结果；下一周期继续修复。
            }
        }

        boolean hasContent =
            mergedDocument.optJSONArray("following").length() > 0
                || mergedDocument.optJSONArray("tasks").length() > 0
                || mergedDocument.optJSONObject("followingDeletedAt").length() > 0;
        boolean needUpload = !download.found || remoteDiffers;
        if (needUpload && hasContent) {
            String document = mergedDocument.toString();
            SyncMerge.validateDocument(document); // 5MB 上限（上传前复检）
            boolean uploaded = client.upload(config, document, download.found, download.etag);
            if (!uploaded) throw new IOException("WebDAV 写入冲突（412/409），等待下个周期");
        }
        WebDavStore.finishSync(context, null);
        MobileStore.setLastWebDavSyncAt(context, System.currentTimeMillis() / 1000L);
    }

    /** 本地文档（MobileStore 投影）：following + pendingTasks + 墓碑。 */
    static JSONObject localDocument(Context context) throws org.json.JSONException, SyncMerge.MergeException {
        JSONObject document = SyncMerge.emptyDocument();
        document.put("following", MobileStore.following(context));
        JSONArray pendingTasks = MobileStore.allTasks(context);
        JSONArray tasks = new JSONArray();
        for (int index = 0; index < pendingTasks.length(); index += 1) {
            JSONObject task = pendingTasks.optJSONObject(index);
            if (task == null) continue;
            JSONObject normalized = new JSONObject();
            for (String key : SyncMerge.namesOf(task)) normalized.put(key, task.opt(key));
            if (normalized.optString("id").isEmpty()) continue;
            if (normalized.optLong("animeId", 0) <= 0) {
                long animeId = SyncMerge.parseAnimeIdFromTaskId(normalized.optString("id"));
                if (animeId > 0) normalized.put("animeId", animeId);
            }
            if (normalized.optString("status").isEmpty()) normalized.put("status", "pending");
            if (normalized.optLong("episode", 0) <= 0) continue;
            tasks.put(normalized);
        }
        document.put("tasks", tasks);
        document.put("followingDeletedAt", MobileStore.tombstones(context));
        document.put("updatedAt", SyncMerge.documentUpdatedAt(
            document.optJSONArray("following"), tasks, document.optJSONObject("followingDeletedAt")));
        return SyncMerge.normalizeDocument(document);
    }

    private static Set<Integer> currentFollowIds(Context context) {
        Set<Integer> ids = new HashSet<>();
        JSONArray following = MobileStore.following(context);
        for (int index = 0; index < following.length(); index += 1) {
            JSONObject item = following.optJSONObject(index);
            if (item != null && item.optInt("id") > 0) ids.add(item.optInt("id"));
        }
        return ids;
    }

    // ------------------------------------------------------------------
    // 错误净化：绝不把 token/凭据写进 lastSyncError
    // ------------------------------------------------------------------

    static String sanitize(String message) {
        String value = message == null ? "" : message;
        value = value.replace("Bearer ", "Bearer ***");
        // 防御： Authorization 头整段剔除（本仓库错误消息均不携带，双保险）。
        int authorization = value.indexOf("Authorization");
        if (authorization >= 0) value = value.substring(0, authorization).trim();
        if (value.length() > 300) value = value.substring(0, 300);
        return value;
    }

    static String joinErrors(List<String> errors) {
        if (errors == null || errors.isEmpty()) return "";
        StringBuilder builder = new StringBuilder();
        for (int index = 0; index < errors.size() && index < 5; index += 1) {
            if (index > 0) builder.append(" | ");
            builder.append(errors.get(index));
        }
        return builder.toString();
    }
}
