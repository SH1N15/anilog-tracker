# Watch State Regression Contracts

These contracts cover the v0.7.4 fixes for disappearing Android tasks,
premature notifications, unstable schedules, and stale following progress.
The maintainer confirmed acceptance of v0.7.4-rc.1 on 2026-09-13 and authorized
the v0.7.4 stable release. Stable packages use Android versionCode 13 rather
than the candidate's 12.

## Background Sync Liveness And Notifications (v0.7.5-rc.4)

The 2026-09-17 field incident: the desktop AniList poll and WebDAV background
loops went silent for ~8 hours while the daily-reminder loop kept running.
A single panicked or wedged iteration killed the loop task with no log, no
restart, and no UI signal; the remote completion sat unpulled while the
settings page still showed a recent sync timestamp. Contracts added for rc.4:

- A panic or abnormal exit in the desktop AniList sync loop or the WebDAV
  background loop must be logged (WARN) and the loop rebuilt automatically;
  silent task death is unacceptable. Each loop also logs an INFO heartbeat
  every iteration so a stall is visible in the log as a missing heartbeat.
- State files with corrupted array fields must not panic the sync path:
  task sorting and `seenAiringEvents` insertion skip gracefully, and the
  Android native snapshot merge guards its object/array access.
- The desktop daily reminder must attempt a WebDAV pull-merge before counting
  pending tasks when the last successful sync is older than 30 minutes. A
  failed pull must not block the reminder (stale reminder beats no reminder).
- Episodes discovered through WebDAV merge (created by another device's
  airing pipeline within the last 24 hours) must trigger the same "追番已更新"
  desktop notification as the polling path, exactly once per task id
  (`claim_merged_airing_notifications`); completed, locally toggled, and
  older-than-24h tasks must not re-notify.
- Android user actions (task toggle, review resolution, follow changes) must
  enqueue a WorkManager immediate sync in addition to the in-process wakeup:
  the vendor may kill the foreground process within seconds, and the
  one-time worker survives process death (KEEP policy, idempotent).
- Android cold start must not gate the UI on the full JNI snapshot round
  trip: `get_state` returns the in-memory snapshot immediately and the
  native event consumption runs in the background, refreshing the UI via
  the state-changed event ("trusted snapshot first, refresh afterwards").
  Setup-stage and bridge timings are logged for field diagnosis.
- Opening the Android app must never re-fire the 20:00 daily summary
  (`checkMissed=false` in MainActivity): the user is already looking at the
  task list, and a late catch-up reads as a duplicate. BootReceiver keeps
  its catch-up for the missed-after-reboot scenario.

## Episode Identity And Time

- A Bangumi subject owns its local `ep` and `episodeId`. Two subjects sharing
  an AniList media ID are not duplicate follows or duplicate task lists.
- A date without a time has three states: a past date, today with an unknown
  time, or a future date. Today is not proof that an existing task is invalid.
- Only verified instant-precision data can trigger an episode notification.
  A UTC-midnight placeholder must never be scheduled as a broadcast instant.
- Match AniList precision by episode identity, or an unambiguous calendar
  match for split subjects. Do not match a split subject's local episode
  number against another part of the enclosing AniList season.
- Resolve cached precision before updating the native schedule. Keep its
  original fetch time; rereading a snapshot must not renew its seven-day TTL.
- Do not delete tasks based on `episode >= nextEpisode`. The next pointer is
  derived data and may be stale.
- Repeated refreshes, process restarts, and re-following must retain verified
  aired tasks and completed history.

## Native State And WebDAV

- Rust/native configuration transfers complete following and task records,
  including titles, episode IDs, creation/completion times, and sync versions.
- Consume the durable native snapshot before configuring Android at startup.
  A drained notification event queue is not evidence that tasks no longer exist.
- Native and foreground merges use record timestamps. An old projection with
  missing timestamps must not acquire a new timestamp and undo a completion.
- WebDAV still contains only `following`, `tasks`, `followingDeletedAt`, and
  document version/timestamp metadata. Both Java and Rust strip these
  device-local following fields before comparing or uploading:
  `nextAiringEpisode`, `nextEpisode`, `nextAiringAt`, `nextEpisodeId`,
  `nextAiringPrecision`, `scheduleUpdatedAt`, and `episodeSchedule`.
- Preserve native schedules independently of business-record LWW. Apply
  per-episode corrections before re-projecting an upload.
- Disabling task creation does not disable notifications. Delivery deduplication
  must not prevent recovery of a missing legitimate task.
- Newly generated pending tasks carry `statusSource=airing`; they cannot undo
  a completed record just because regeneration has a later sync timestamp.
  Explicit toggles set `statusSource=local` and still use normal timestamp
  conflict resolution. Both devices must be upgraded to enforce this rule.

## Progress And Layout

- Toggling a task updates local following progress immediately, including undo.
- Bangumi progress is refreshed for in-progress collections, not only completed
  collections. A stored remote hash cannot hide a stale local progress field.
- Do not overwrite an unpushed local progress change with an earlier pull.
  Undo uses the same episode ID and the uncollected episode state.
- Repair existing counters from distinct local completed episodes when the
  counter is known. Ignore legacy global episode numbers beyond the subject's
  total, and never delete completed history to repair a counter.
- Progress text remains one horizontal item. Status/rating/progress controls
  may wrap as items, but the progress label cannot be squeezed vertically.

## Regression Entry Points

- [Rust mobile snapshots](../src-tauri/src/mobile_state.rs)
- [Rust progress tests](../src-tauri/src/watch_progress_tests.rs)
- [Android episode tests](../src-tauri/gen/android/app/src/test/java/io/anilog/android/EpisodeScheduleTest.java)
- [Android merge tests](../src-tauri/gen/android/app/src/test/java/io/anilog/android/SyncMergeTest.java)

Run both Cargo editions and the Android `testUniversalDebugUnitTest` task with
each `ANILOG_ANDROID_EDITION` value. Android builds require the compatible JBR 21
described in [the maintainer handoff](MAINTAINER_HANDOFF.md).

For future device regressions, use an actual Android device: leave the app closed
across an airing and midnight, then check task retention, a single on-time
notification, reopen recovery, and bidirectional WebDAV completion/undo.
Browser screenshots and JVM tests do not establish AlarmManager behavior on
a vendor's Android firmware.

The verified baseline is 171 Standard Rust tests, 30 Original Rust tests,
31 JVM tests per edition, 12 Node regression scripts, and browser layout
checks at 320, 390, 768, and 1280 pixels. These automated checks and the
maintainer's acceptance are separate evidence; neither establishes behavior
on every Android device.

## Unreleased Follow And Collection Sync

The following contracts were added after the v0.7.4 release. They do not imply
that the published packages or the maintainer's installed apps contain the fixes.

- Unfollowing is an idempotent command, not a toggle that can accidentally
  re-add an already removed record. Completed history remains intact.
- Rust and Android apply the same timestamp comparison in both merge
  directions. A historical `localFollowIntentAt` cannot bypass a newer
  deletion or override a newer edit from another device.
- Bangumi `doing` is not proof of a new follow intent. A deletion tombstone
  blocks automatic reimport, including when collection upload is disabled.
  Explicitly following the subject again clears the local deletion intent.
- Equal collection values are convergence, not a conflict. Remote-only
  comments, tags, and privacy settings do not create a conflict in fields
  AniLog does not edit.
- The persisted `latest` policy retains unacknowledged local edits because
  Bangumi does not expose a reliable rating/progress edit timestamp. Its UI
  label must not promise timestamp ordering that the API cannot support.
  `bangumi-first` resolves actual differing-value conflicts in favor of
  Bangumi; unchanged remote content must not undo a local edit.
- Pulling never writes to the account. `local-first` cannot bypass the upload
  switches. Failed writes remain pending; only the sent payload is acknowledged.
- A rating cleared by the user carries `localRatingUpdatedAt` in milliseconds
  and uploads `rate=0`. A missing, never-edited rating remains unknown and does
  not reset a pre-existing remote rating.
- Network plans recheck deletions and record revisions before applying a pull
  or sending an upload. Manual and automatic Rust collection sync transactions
  share one lock. Android receives local rating/status edits before a later
  native snapshot can be merged.
- With Bangumi account sync enabled, the top-bar sync action runs the full
  sync transaction, including its gated upload phase, rather than only fetching
  airing schedules. Original keeps its AniList-only path.

Regression entry points:
[cross-device and collection intent tests](../src-tauri/src/sync_intent_tests.rs)
and [Android merge tests](../src-tauri/gen/android/app/src/test/java/io/anilog/android/SyncMergeTest.java).
The older collection-policy tests now use genuinely different local and
remote values; a test expecting equal values to conflict or a deleted follow
to be automatically restored is not a valid acceptance criterion.

## Rc.2 Schedule And Startup

- `Media.airingSchedule` does not accept `sort`. Invalid GraphQL arguments
  reject the whole batch, not just one media row. Mock responses alone cannot
  validate upstream query compatibility; keep the argument-contract tests.
- Bangumi `ep` and `sort` may differ while AniList still uses the local season
  number. Two distinct, unique aired-history calendar matches must establish that numbering
  before applying a changed AniList date by local episode. One match or a
  conflicting historical match must not rebind a shared/split season.
  A future date coincidence after a schedule change is not numbering evidence.
- An episode with a Bangumi ID but no date may use the timestamp from an
  already matched AniList episode. No identity, no invented schedule.
- Adding a follow reuses verified local caches and refreshes only that work.
  Do not copy an unverified quarterly broadcast estimate into its alarm.
- Manual refresh honors a one-minute floor for both AniList and Bangumi
  episode caches; automatic requests retain their normal longer lifetime.
- Native state is initialized before exposing commands to the UI. Android
  `get_state` performs bridge reads off the UI thread and retains the loaded
  local snapshot when native refresh is temporarily unavailable.
- Before the first real state snapshot, render loading/retry UI, not empty
  following lists or default settings. Startup read retries are bounded;
  disposal or a newer pushed snapshot cancels them. Resume errors retain
  the last real state instead of restarting polling.
- Serialize state saves through the temporary-file rename and flush the
  new file before replacing the old one. An unreadable existing state file
  must not be silently overwritten with defaults.
- Failed schedule requests remain visible in manual/full-sync results,
  including the narrow Android layout.

The [shared public-data fixture](../src-tauri/fixtures/anilist/schedule-regressions.json)
records subjects 607340/638497 and AniList 202269/210031. On 2026-09-14 the
public APIs returned the next verified instants 2026-09-14 20:00 and
2026-09-20 16:00 (UTC+8), respectively. These are dated regression samples,
not promises that the upstream schedules will never change.

Additional entry points:
[Rust startup and schedule regressions](../src-tauri/src/startup_schedule_tests.rs),
[frontend bootstrap races](../scripts/test-state-refresh.cjs), and the
Android episode tests above. ADB had no connected device for this session;
simulated cold starts do not replace the maintainer's force-stop/reopen test.

## Rc.3 Completion Review

- Keeping a completed record is not equivalent to trusting it for progress.
  Verified per-episode future facts mark a completion with `completionReview`.
  Preserve its ID, original status, timestamps, and prior upload evidence.
- Only `airingSource=bangumi_episode` with a positive episode ID and known
  `instant` or future-day `date` precision can trigger automatic review.
  A next-episode pointer or today's unknown broadcast time is not proof.
- `completionReview.decision=review` is excluded from completed counts,
  progress, and Bangumi uploads. It remains in `tasks` and WebDAV history,
  and remains under review even after its scheduled time passes.
- Progress can decrease automatically only when the previous aggregate
  equals the full local set including reviewed/reset episodes. Incomplete
  local history must not reduce an unrelated remote aggregate.
- An explicit user review sets `decision=keep` or `reset`. Keep restores its
  counted completion. Reset records local intent as `pending`, retains the
  original completion in review metadata, and becomes a watch task only after
  airing. Future-time cleanup must preserve this explicit correction so an
  old device cannot restore the obsolete completion.
- Only explicit local completion/undo intent may automatically write episode
  progress to Bangumi. Legacy `completed` records without provenance are not
  upload intentions. Recheck episode snapshots before sending and acknowledge
  only the sent revision.
- Review actions require confirmation and use an idempotent resolution
  command. Retrying the old action must not toggle the result.
- The actual delayed schedule stays upstream-controlled. On the later
  2026-09-14 check, both public sources placed subject 638497 episode 12 on
  September 27 (AniList: 16:00 UTC+8), not September 20. Never repair a
  progress problem by guessing a different broadcast date.

Tests:
[Rust review kernel](../src-tauri/src/watch_history.rs),
[Rust progress and old-snapshot regressions](../src-tauri/src/watch_progress_tests.rs),
[Android review kernel](../src-tauri/gen/android/app/src/test/java/io/anilog/android/WatchHistoryTest.java),
[frontend task history](../scripts/test-task-history.cjs).
