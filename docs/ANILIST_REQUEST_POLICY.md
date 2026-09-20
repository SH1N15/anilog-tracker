# AniList Request Policy

This policy applies to the Tauri v0.7.5 release. Electron and Capacitor
fallback implementations keep their existing caches.

## Refresh Classes

| Data | Automatic refresh | Explicit refresh |
| --- | --- | --- |
| Current or future seasonal catalog | 24 hours, only when viewed | 60-second cache floor |
| Historical seasonal catalog | 30 days, only when viewed | 60-second cache floor |
| Actively followed media, Windows | Existing `pollIntervalMinutes`, minimum 1 minute | 60-second cache floor |
| Actively followed media, Android | Existing 6-hour WorkManager cadence | 60-second cache floor |
| Wish or on-hold media | 24 hours | 60-second cache floor |
| Done, FINISHED or CANCELLED media | 30 days | 60-second cache floor |
| A valid ID absent from a successful response | 24 hours | 60-second cache floor |

Adding a follow immediately requests its schedule, reusing a result fetched
within the last minute. It does not refresh the whole seasonal catalog.
The target is a following ID, not an AniList ID: both the Bangumi episode
request and AniList supplement must be limited to that newly followed work.
Verified per-episode cache data is applied before returning the follow result.
Manual refresh also uses a 60-second floor for Bangumi episode lists.
An alarm may enqueue the existing one-time Android worker; no high-frequency
Android polling or persistent process is introduced.

The top-bar refresh includes the visible seasonal catalog when that view is
open. Merely opening Following, Tasks or Settings does not fetch a season.
Cached catalogs appear immediately; expired entries refresh in the background.

## Cache And Network

- Rust stores per-media snapshots under `season-cache/anilist-cache`.
  Snapshot `fetchedAt` is in seconds; the seasonal catalog envelope retains
  its existing millisecond timestamp.
- Seasonal enrichment, new follows and regular schedule checks reuse these
  snapshots. A batch uses at most 50 distinct, positive AniList IDs per request.
  Unmapped Bangumi subjects are never sent as AniList media IDs.
- Only due IDs are requested. Multiple Bangumi subjects sharing an AniList ID
  share its request, not their task identity. The shortest required lifetime
  wins when more than one follow uses that ID.
- `Media.airingSchedule` accepts `notYetAired`, `page` and `perPage`, but not
  `sort`. The `sort` argument belongs to `Page.airingSchedules`. Do not mix
  these contracts; an invalid argument rejects the entire media batch.
- A shared async gate rechecks disk cache after the previous batch finishes.
  Manual refresh cannot bypass its one-minute floor.
- A successful response stores every requested ID, including a negative cache
  for missing media. Failures do not replace the last successful snapshot.
- Requests remain under 30 per rolling minute. HTTP 403 starts a 15-minute
  cooldown; other errors start at one minute, with exponential backoff capped
  at one hour. A longer valid `Retry-After` takes precedence. Cooldown state is
  persisted, so restarting does not immediately retry the upstream.
- The User-Agent comes from the build version. Error messages do not include
  raw response bodies.
- A failed supplement is surfaced as a warning, including in full Bangumi
  sync. Keeping stale data is not equivalent to a successful refresh.
- Each Tauri network operation builds a `reqwest` client with `system-proxy`
  enabled even though default features stay disabled. reqwest snapshots the
  operating system proxy when a client is built, so the client is deliberately
  not cached for the process lifetime. Windows and macOS builds therefore honor
  the current system proxy for Bangumi, AniList, and WebDAV requests after a
  VPN/proxy switch; do not replace this with `no_proxy()` or a hard-coded proxy
  address.

## Android Bridge

Android keeps its own persistent per-ID cache for WorkManager/AlarmManager.
The foreground/native bridge exchanges only followed IDs and their original
fetch times, outside the business document. An older snapshot cannot replace
a newer one or renew its lifetime.

Native schedule refresh is synchronized and rechecks each ID's cache. Request
selection and response filtering are pure functions in `AniListRequestPolicy`,
with JVM tests for edition separation, deduplication, lifetimes, manual
cooldown, missing records and retry delay.

## Time Safety

- Precise episode timestamps expire after seven days independently of metadata
  lifetime. A 30-day FINISHED cache is not a 30-day notification authority.
- Cached future nodes may be advanced only to explicit returned instants.
  Never extrapolate a weekly schedule.
- Standard still resolves episode identity and dates from each Bangumi subject.
  AniList global numbering cannot create tasks under a split subject.
- Device-local playback schedules and all caches remain outside WebDAV.
- Notifications and task creation are independent. Cached data must still
  deliver one notification when task creation is disabled, with normal dedupe.

## Verification

Run both Cargo editions and both Android JVM editions. Relevant tests:

- [Rust cache and request tests](../src-tauri/src/anilist_cache.rs)
- [Task and local-intent regressions](../src-tauri/src/sync_intent_tests.rs)
- [Android request policy tests](../src-tauri/gen/android/app/src/test/java/io/anilog/android/AniListRequestPolicyTest.java)
- [Episode precision tests](../src-tauri/gen/android/app/src/test/java/io/anilog/android/EpisodeScheduleTest.java)

Mock servers verify batching, cache hits, concurrent request sharing, explicit
refresh, missing IDs and persistent rate-limit backoff without contacting
AniList. JVM and browser checks do not replace device testing of scheduled
notifications on vendor Android firmware.
