use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub(super) const DAY: i64 = 86_400;
pub(super) const MANUAL_COOLDOWN: i64 = 60;
pub(super) const PRECISE_MAX_AGE: i64 = 7 * DAY;
pub(super) const MEDIA_QUERY: &str = r#"query AniListAuthority($ids: [Int], $page: Int) {
  Page(page: $page, perPage: 50) { pageInfo { lastPage }
    media(id_in: $ids, type: ANIME) {
      id status episodes duration genres averageScore bannerImage
      coverImage { medium } studios(isMain: true) { nodes { name } }
      nextAiringEpisode { episode airingAt }
      airingSchedule(notYetAired: false, perPage: 50) { nodes { episode airingAt } }
      futureAiringSchedule: airingSchedule(notYetAired: true, perPage: 50) { nodes { episode airingAt } }
    }
  }
}"#;

static MEDIA_GATE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static FILE_GATE: Mutex<()> = Mutex::new(());
static NETWORK: LazyLock<tokio::sync::Mutex<HashMap<String, NetworkState>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

#[derive(Default)]
struct NetworkState {
    recent: VecDeque<Instant>,
    retry_at: i64,
    failures: u32,
}

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub fetched_at: i64,
    pub media: Value,
}

impl Snapshot {
    pub fn precise(&self, now: i64) -> bool {
        self.media.is_object() && fresh(self.fetched_at, now, PRECISE_MAX_AGE)
    }
}

#[derive(Default)]
pub(super) struct Batch {
    pub snapshots: BTreeMap<i64, Snapshot>,
    pub warnings: Vec<String>,
}

pub(super) fn fresh(fetched_at: i64, now: i64, ttl: i64) -> bool {
    fetched_at > 0 && fetched_at <= now && now - fetched_at < ttl
}

pub(super) fn media_id(entry: &Value, original: bool) -> Option<i64> {
    let key = if !original && entry["source"] == "bangumi" {
        "anilistId"
    } else {
        "id"
    };
    entry[key]
        .as_i64()
        .filter(|id| *id > 0 && *id <= i64::from(i32::MAX))
}

pub(super) fn following_requests(
    state: &Value,
    original: bool,
    active_ttl: i64,
) -> BTreeMap<i64, i64> {
    let mut requests = BTreeMap::<i64, i64>::new();
    for entry in state["following"].as_array().into_iter().flatten() {
        if !original && entry["bangumiStatus"] == "dropped" {
            continue;
        }
        let Some(id) = media_id(entry, original) else {
            continue;
        };
        let ttl = match entry["bangumiStatus"].as_str().unwrap_or("") {
            "done" => 30 * DAY,
            "wish" | "on_hold" => DAY,
            _ => active_ttl.max(MANUAL_COOLDOWN),
        };
        requests
            .entry(id)
            .and_modify(|old| *old = (*old).min(ttl))
            .or_insert(ttl);
    }
    requests
}

pub(super) fn read_snapshot(directory: &Path, id: i64) -> Option<Snapshot> {
    let raw = fs::read(directory.join(format!("media-{id}.json"))).ok()?;
    let record: Value = serde_json::from_slice(&raw).ok()?;
    let fetched_at = record["fetchedAt"].as_i64()?;
    let media = record.get("media")?.clone();
    if record["version"] != 1
        || fetched_at <= 0
        || (!media.is_null() && media["id"].as_i64() != Some(id))
    {
        return None;
    }
    Some(Snapshot { fetched_at, media })
}

pub(super) fn store_snapshot(directory: &Path, id: i64, snapshot: &Snapshot) -> anyhow::Result<()> {
    if id <= 0
        || snapshot.fetched_at <= 0
        || (!snapshot.media.is_null() && snapshot.media["id"].as_i64() != Some(id))
    {
        return Err(anyhow!("Invalid AniList cache entry"));
    }
    let _guard = FILE_GATE
        .lock()
        .map_err(|_| anyhow!("AniList cache lock unavailable"))?;
    if read_snapshot(directory, id).is_some_and(|old| {
        old.fetched_at <= super::now_seconds() && old.fetched_at >= snapshot.fetched_at
    }) {
        return Ok(());
    }
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("media-{id}.json"));
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(&json!({
            "version": 1, "fetchedAt": snapshot.fetched_at, "media": snapshot.media
        }))?,
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(super) fn next_episode(media: &Value, now: i64) -> Value {
    media["futureAiringSchedule"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(media.get("nextAiringEpisode"))
        .filter(|node| {
            node["episode"].as_i64().is_some_and(|ep| ep > 0)
                && node["airingAt"].as_i64().is_some_and(|at| at > now)
        })
        .min_by_key(|node| node["airingAt"].as_i64().unwrap_or(i64::MAX))
        .cloned()
        .unwrap_or(Value::Null)
}

pub(super) async fn media(
    client: &reqwest::Client,
    endpoint: &str,
    directory: &Path,
    requests: &BTreeMap<i64, i64>,
    force: bool,
    now: i64,
) -> Batch {
    // Read again after acquiring the gate so overlapping batches reuse the
    // response just written by the preceding caller, even across view changes.
    let _guard = MEDIA_GATE.lock().await;
    let mut batch = Batch::default();
    let mut missing = Vec::new();
    for (&id, &ttl) in requests
        .iter()
        .filter(|(id, _)| **id > 0 && **id <= i64::from(i32::MAX))
    {
        let snapshot = read_snapshot(directory, id);
        let ttl = if force {
            MANUAL_COOLDOWN
        } else if snapshot
            .as_ref()
            .is_some_and(|s| s.media.is_null() || s.media["_missing"] == true)
        {
            DAY
        } else if snapshot
            .as_ref()
            .is_some_and(|s| matches!(s.media["status"].as_str(), Some("FINISHED" | "CANCELLED")))
        {
            30 * DAY
        } else {
            ttl
        };
        if !snapshot
            .as_ref()
            .is_some_and(|s| fresh(s.fetched_at, now, ttl))
        {
            missing.push(id);
        }
        if let Some(snapshot) = snapshot {
            batch.snapshots.insert(id, snapshot);
        }
    }
    for ids in missing.chunks(50) {
        match fetch_media(client, endpoint, ids, Some(directory)).await {
            Ok(records) => {
                for &id in ids {
                    let snapshot = Snapshot {
                        fetched_at: now,
                        media: records.get(&id).cloned().unwrap_or(Value::Null),
                    };
                    let _ = store_snapshot(directory, id, &snapshot);
                    batch.snapshots.insert(id, snapshot);
                }
            }
            Err(error) => {
                batch.warnings.push(error.to_string());
                break;
            }
        }
    }
    batch
}

pub(super) async fn fetch_media(
    client: &reqwest::Client,
    endpoint: &str,
    ids: &[i64],
    directory: Option<&Path>,
) -> anyhow::Result<HashMap<i64, Value>> {
    let mut records = HashMap::new();
    for page in 1..=10 {
        let data = request(
            client,
            endpoint,
            MEDIA_QUERY,
            json!({"ids": ids, "page": page}),
            directory,
        )
        .await?;
        let media = data["Page"]["media"]
            .as_array()
            .context("Invalid AniList media response")?;
        for item in media {
            if let Some(id) = item["id"].as_i64().filter(|id| ids.contains(id)) {
                records.insert(id, item.clone());
            }
        }
        let last_page = data["Page"]["pageInfo"]["lastPage"].as_i64().unwrap_or(1);
        if page >= last_page {
            return Ok(records);
        }
    }
    Err(anyhow!("AniList pagination limit"))
}

fn retry_seconds(value: Option<&str>, now: i64) -> i64 {
    value
        .and_then(|value| {
            value.trim().parse::<i64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .map(|date| date.timestamp() - now)
            })
        })
        .unwrap_or(0)
        .clamp(0, 7 * DAY)
}

fn backoff_seconds(status: u16, retry_after: Option<&str>, failures: u32, now: i64) -> i64 {
    let base = if status == 403 { 15 * 60 } else { 60 };
    (base * (1_i64 << failures.saturating_sub(1).min(6)))
        .min(3600)
        .max(retry_seconds(retry_after, now))
}

pub(super) async fn request(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
    variables: Value,
    directory: Option<&Path>,
) -> anyhow::Result<Value> {
    let key = endpoint.trim_end_matches('/').to_string();
    let retry_path =
        directory.map(|dir| dir.join(format!("retry-{:x}.json", Sha256::digest(key.as_bytes()))));
    loop {
        let delay = {
            let mut network = NETWORK.lock().await;
            let state = network.entry(key.clone()).or_default();
            let now = super::now_seconds();
            if let Some(path) = &retry_path {
                if let Ok(raw) = fs::read(path) {
                    if let Ok(saved) = serde_json::from_slice::<Value>(&raw) {
                        state.retry_at = state.retry_at.max(saved["retryAt"].as_i64().unwrap_or(0));
                        state.failures = state
                            .failures
                            .max(saved["failures"].as_u64().unwrap_or(0).min(16) as u32);
                    }
                }
            }
            if state.retry_at > now {
                return Err(anyhow!(
                    "AniList 请求冷却中，{} 秒后可重试",
                    state.retry_at - now
                ));
            }
            let instant = Instant::now();
            while state
                .recent
                .front()
                .is_some_and(|at| instant.duration_since(*at) >= Duration::from_secs(60))
            {
                state.recent.pop_front();
            }
            if state.recent.len() < 30 {
                state.recent.push_back(instant);
                None
            } else {
                state
                    .recent
                    .front()
                    .map(|at| Duration::from_secs(60).saturating_sub(instant.duration_since(*at)))
            }
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        } else {
            break;
        }
    }
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::ORIGIN, "https://anilist.co")
        .header(reqwest::header::REFERER, "https://anilist.co/")
        .header(
            reqwest::header::USER_AGENT,
            concat!(
                "AniLog/",
                env!("CARGO_PKG_VERSION"),
                " (https://github.com/SH1N15/anilog-tracker)"
            ),
        )
        .json(&json!({"query": query, "variables": variables}))
        .send()
        .await;
    let (status, retry_after) = response
        .as_ref()
        .map(|response| {
            (
                response.status().as_u16(),
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            )
        })
        .unwrap_or((0, None));
    let result = match response {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(payload)
                if payload["errors"].as_array().is_none_or(Vec::is_empty)
                    && payload["data"].is_object() =>
            {
                Ok(payload["data"].clone())
            }
            _ => Err(anyhow!("AniList 返回无效数据")),
        },
        Ok(_) => Err(anyhow!("AniList HTTP {status}")),
        Err(_) => Err(anyhow!("AniList 网络请求失败")),
    };
    let mut network = NETWORK.lock().await;
    let state = network.entry(key).or_default();
    if result.is_ok() {
        state.failures = 0;
        state.retry_at = 0;
    } else {
        state.failures = state.failures.saturating_add(1);
        state.retry_at = super::now_seconds()
            + backoff_seconds(
                status,
                retry_after.as_deref(),
                state.failures,
                super::now_seconds(),
            );
    }
    if let Some(path) = retry_path {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(
            path,
            json!({"retryAt": state.retry_at, "failures": state.failures}).to_string(),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn directory() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "anilog-request-policy-{}-{}-{}",
            std::process::id(),
            super::super::now_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn requests_use_real_ids_and_shortest_tracking_ttl() {
        let state = json!({"following": [
            {"id": 1, "source": "bangumi"},
            {"id": 2, "source": "bangumi", "anilistId": 100, "bangumiStatus": "wish"},
            {"id": 3, "source": "bangumi", "anilistId": 100, "bangumiStatus": "doing"},
            {"id": 4, "source": "bangumi", "anilistId": 200, "bangumiStatus": "done"},
            {"id": 5, "source": "bangumi", "anilistId": 300, "bangumiStatus": "dropped"},
            {"id": 400, "source": "anilist"}
        ]});
        assert_eq!(
            following_requests(&state, false, 300),
            BTreeMap::from([(100, 300), (200, 30 * DAY), (400, 300)])
        );
        assert_eq!(
            following_requests(&json!({"following":[{"id":7}]}), true, 300),
            BTreeMap::from([(7, 300)])
        );
    }

    #[test]
    fn freshness_and_retry_after_are_bounded() {
        assert!(fresh(100, 159, 60));
        assert!(!fresh(100, 160, 60));
        assert!(!fresh(200, 100, 60));
        assert!(!fresh(0, 100, 60));
        assert_eq!(backoff_seconds(429, Some("120"), 1, 0), 120);
        assert_eq!(backoff_seconds(503, None, 2, 0), 120);
        assert_eq!(backoff_seconds(403, None, 1, 0), 900);
        assert_eq!(backoff_seconds(503, None, 100, 0), 3600);
        let date = chrono::DateTime::from_timestamp(1_800_000_000, 0)
            .unwrap()
            .to_rfc2822();
        assert_eq!(retry_seconds(Some(&date), 1_799_999_900), 100);
    }

    #[test]
    fn next_is_a_verified_future_node_not_weekly_extrapolation() {
        let media = json!({"nextAiringEpisode": {"episode": 5, "airingAt": 100},
            "futureAiringSchedule": {"nodes":[{"episode":6,"airingAt":200}]}});
        assert_eq!(next_episode(&media, 150)["episode"], 6);
        assert!(next_episode(&media, 201).is_null());
    }

    #[test]
    fn disk_cache_preserves_timestamp_and_rejects_older_snapshots() {
        let directory = directory();
        let original = Snapshot {
            fetched_at: 100,
            media: json!({"id": 42, "nextAiringEpisode": {"episode": 5, "airingAt": 1000}}),
        };
        store_snapshot(&directory, 42, &original).unwrap();
        store_snapshot(
            &directory,
            42,
            &Snapshot {
                fetched_at: 90,
                media: json!({"id":42}),
            },
        )
        .unwrap();
        assert_eq!(read_snapshot(&directory, 42).unwrap().media, original.media);
        assert_eq!(read_snapshot(&directory, 42).unwrap().fetched_at, 100);
        assert!(
            !read_snapshot(&directory, 42)
                .unwrap()
                .precise(100 + PRECISE_MAX_AGE)
        );
        assert!(store_snapshot(&directory, 43, &original).is_err());
        fs::write(directory.join("media-42.json"), "broken").unwrap();
        assert!(read_snapshot(&directory, 42).is_none());
    }

    #[cfg(feature = "standard")]
    fn server() -> crate::bangumi::test_support::MockBangumiServer {
        crate::bangumi::test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_, _, _, body| {
                let body: Value = serde_json::from_str(body).unwrap();
                let items: Vec<Value> = body["variables"]["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| {
                        json!({"id":id, "status":"RELEASING", "airingSchedule":{"nodes":[]},
                    "nextAiringEpisode":null, "futureAiringSchedule":{"nodes":[]}})
                    })
                    .collect();
                (
                    200,
                    vec![],
                    json!({"data":{"Page":{"pageInfo":{"lastPage":1}, "media":items}}}).to_string(),
                )
            },
        ))
    }

    #[cfg(feature = "standard")]
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[cfg(feature = "standard")]
    #[test]
    fn batches_cache_hits_manual_cooldown_and_new_follow_only_fetch_missing() {
        let server = server();
        let client = reqwest::Client::new();
        let directory = directory();
        let requests: BTreeMap<i64, i64> = (1..=105).map(|id| (id, DAY)).collect();
        let rt = runtime();
        let now = super::super::now_seconds();
        let first = rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &requests,
            false,
            now,
        ));
        assert_eq!(first.snapshots.len(), 105);
        assert!(first.warnings.is_empty());
        assert_eq!(server.requests().len(), 3);
        for request in server.requests() {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert!(body["variables"]["ids"].as_array().unwrap().len() <= 50);
            assert!(
                body["query"]
                    .as_str()
                    .unwrap()
                    .contains("futureAiringSchedule")
            );
        }
        rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &requests,
            false,
            now + 300,
        ));
        rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &requests,
            true,
            now + 59,
        ));
        assert_eq!(server.requests().len(), 3);
        let mut new_follow = BTreeMap::from([(1, 300), (106, 300)]);
        rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &new_follow,
            false,
            now + 10,
        ));
        assert_eq!(server.requests().len(), 4);
        let last: Value = serde_json::from_str(&server.requests().last().unwrap().body).unwrap();
        assert_eq!(last["variables"]["ids"], json!([106]));
        new_follow.remove(&106);
        rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &new_follow,
            true,
            now + 61,
        ));
        assert_eq!(server.requests().len(), 5);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn concurrent_batches_share_the_first_response() {
        let server = server();
        let directory = directory();
        let rt = runtime();
        let first = {
            let url = server.url();
            let directory = directory.clone();
            rt.spawn(async move {
                media(
                    &reqwest::Client::new(),
                    &url,
                    &directory,
                    &BTreeMap::from([(1, DAY)]),
                    false,
                    super::super::now_seconds(),
                )
                .await
            })
        };
        let second = {
            let url = server.url();
            rt.spawn(async move {
                media(
                    &reqwest::Client::new(),
                    &url,
                    &directory,
                    &BTreeMap::from([(1, DAY)]),
                    false,
                    super::super::now_seconds(),
                )
                .await
            })
        };
        rt.block_on(async {
            assert!(first.await.unwrap().warnings.is_empty());
            assert!(second.await.unwrap().warnings.is_empty());
        });
        assert_eq!(server.requests().len(), 1);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn failure_retains_timestamp_and_persists_retry_across_network_state_reset() {
        let server = crate::bangumi::test_support::MockBangumiServer::spawn(std::sync::Arc::new(
            |_, _, _, _| (429, vec![("Retry-After".into(), "180".into())], "{}".into()),
        ));
        let directory = directory();
        let now = super::super::now_seconds();
        store_snapshot(
            &directory,
            1,
            &Snapshot {
                fetched_at: now - DAY,
                media: json!({"id":1}),
            },
        )
        .unwrap();
        let requests = BTreeMap::from([(1, 300)]);
        let client = reqwest::Client::new();
        let rt = runtime();
        let first = rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &requests,
            false,
            now,
        ));
        assert_eq!(first.snapshots[&1].fetched_at, now - DAY);
        assert!(!first.warnings.is_empty());
        rt.block_on(async {
            NETWORK.lock().await.remove(&server.url());
        });
        let second = rt.block_on(media(
            &client,
            &server.url(),
            &directory,
            &requests,
            true,
            now,
        ));
        assert!(!second.warnings.is_empty());
        assert_eq!(server.requests().len(), 1);
        assert_eq!(read_snapshot(&directory, 1).unwrap().fetched_at, now - DAY);
    }
}
