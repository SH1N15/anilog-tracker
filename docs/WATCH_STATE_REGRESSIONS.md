# Watch State Regression Contracts

These contracts cover the v0.7.4 fixes for disappearing Android tasks,
premature notifications, unstable schedules, and stale following progress.
The maintainer confirmed acceptance of v0.7.4-rc.1 on 2026-09-13 and authorized
the v0.7.4 stable release. Stable packages use Android versionCode 13 rather
than the candidate's 12.

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
