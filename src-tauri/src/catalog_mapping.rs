//! Runtime identity evidence. Catalog dates are display metadata, never episode events.
use super::*;

pub(super) const SEASON_REVISION: i64 = 2;
static MAPPING_GATE: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

fn titles(item: &Value) -> HashSet<String> {
    ["native", "romaji", "english"]
        .into_iter()
        .filter_map(|key| item["title"][key].as_str())
        .chain(
            item["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .map(normalize_title_key)
        .filter(|key| !key.is_empty())
        .collect()
}

fn date_parts(item: &Value) -> Option<(i64, u32, Option<u32>)> {
    let date = &item["startDate"];
    let year = date["year"].as_i64()?;
    let month = u32::try_from(date["month"].as_i64()?).ok()?;
    if !(1900..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let day = match date.get("day") {
        None | Some(Value::Null) => None,
        Some(day) => Some(u32::try_from(day.as_i64()?).ok()?),
    };
    if let Some(day) = day {
        chrono::NaiveDate::from_ymd_opt(year as i32, month, day)?;
    }
    Some((year, month, day))
}

/// Exact normalized titles retain season/part numbers. Never use substring scores
/// or a year alone to link a sequel, split subject, movie or similarly named work.
pub(super) fn same_work(left: &Value, right: &Value) -> bool {
    let left_format = value_string(left.get("format"));
    if left_format.is_empty() || left_format != value_string(right.get("format")) {
        return false;
    }
    let (Some((ly, lm, ld)), Some((ry, rm, rd))) = (date_parts(left), date_parts(right)) else {
        return false;
    };
    let dates_match = match (ld, rd) {
        (Some(ld), Some(rd)) => {
            let l = chrono::NaiveDate::from_ymd_opt(ly as i32, lm, ld).unwrap();
            let r = chrono::NaiveDate::from_ymd_opt(ry as i32, rm, rd).unwrap();
            (l - r).num_days().abs() <= 1
        }
        _ => ly == ry && lm == rm,
    };
    dates_match && !titles(left).is_disjoint(&titles(right))
}

pub(super) fn subject_anime(subject: &bangumi::BangumiSubject) -> Value {
    let mut anime = map_subjects_to_anime(
        std::slice::from_ref(subject),
        &json!({}),
        &[],
        "",
        0,
        now_seconds(),
    )
    .remove(0);
    anime["title"]["native"] = json!(subject.name);
    let aliases: Vec<Value> = subject
        .infobox
        .iter()
        .filter(|field| field.key == "别名")
        .flat_map(|field| field.value.as_array().into_iter().flatten())
        .filter_map(|alias| alias["v"].as_str().map(|value| json!(value)))
        .collect();
    anime["aliases"] = json!(aliases);
    anime
}

pub(super) fn broadcast_weekday(subject: &bangumi::BangumiSubject) -> Option<u32> {
    let text = subject
        .infobox
        .iter()
        .find(|field| field.key == "放送星期")?
        .value
        .as_str()?;
    [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
        "星期天",
    ]
    .iter()
    .position(|day| text.trim() == *day)
    .map(|day| if day == 7 { 7 } else { day as u32 + 1 })
}

/// Only a unique match in both directions can create a new catalog association.
/// Explicit manual and embedded split associations remain intact.
pub(super) fn link_catalog(anime: &mut [Value], catalog: &[Value], state: &Value) {
    for item in anime.iter_mut() {
        if value_i64(item.get("anilistId")) > 0 {
            continue;
        }
        let subject = value_i64(item.get("id"));
        let ids: HashSet<i64> = state["following"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["source"] == "bangumi" && value_i64(entry.get("id")) == subject)
            .map(|entry| value_i64(entry.get("anilistId")))
            .filter(|id| *id > 0)
            .collect();
        if ids.len() == 1 {
            item["anilistId"] = json!(ids.into_iter().next().unwrap());
        }
    }
    let candidates: Vec<Option<i64>> = anime
        .iter()
        .map(|item| {
            if value_i64(item.get("anilistId")) > 0 {
                return None;
            }
            let ids: HashSet<i64> = catalog
                .iter()
                .filter(|media| media["status"] != "CANCELLED" && same_work(item, media))
                .map(|media| value_i64(media.get("id")))
                .filter(|id| *id > 0)
                .collect();
            (ids.len() == 1).then(|| *ids.iter().next().unwrap())
        })
        .collect();
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(id) = candidate else {
            continue;
        };
        if candidates
            .iter()
            .filter(|other| other.as_ref() == Some(id))
            .count()
            != 1
            || anime
                .iter()
                .any(|item| value_i64(item.get("anilistId")) == *id)
        {
            continue;
        }
        anime[index]["anilistId"] = json!(id);
        anime[index]["mappingEvidence"] = json!("catalog-title-date");
    }
}

fn eligible(entry: &Value) -> bool {
    entry["source"] != "bangumi"
        && entry.get("mapping").is_none_or(Value::is_null)
        && value_i64(entry.get("id")) > 0
}

pub(super) fn retain_links(anime: &mut [Value], previous: &[Value]) {
    for item in anime {
        if value_i64(item.get("anilistId")) > 0 {
            continue;
        }
        if let Some(old) = previous.iter().find(|old| {
            old["id"] == item["id"] && value_i64(old.get("anilistId")) > 0 && same_work(item, old)
        }) {
            item["anilistId"] = old["anilistId"].clone();
            // Identity evidence survives a network outage; timestamps do not get renewed.
            item["mappingEvidence"] = old["mappingEvidence"].clone();
        }
    }
}

fn bind_verified(state: &mut Value, old_id: i64, subject: i64) -> bool {
    if old_id == subject {
        return false;
    }
    let occupied = state["following"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| value_i64(entry.get("id")) == subject);
    if let Some(target) = occupied {
        // An already bound target is safe to merge only when its AniList ID agrees.
        if target["source"] != "bangumi" || value_i64(target.get("anilistId")) != old_id {
            return false;
        }
        return merge_cross_key_entry(state, old_id, subject);
    }
    if !apply_mapping_with_confidence(state, old_id, subject, "title-year", "high") {
        return false;
    }
    if let Some(entry) = state["following"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|entry| value_i64(entry.get("id")) == subject)
    {
        // The old next event is device-local and must be rebound to episode evidence.
        entry["nextAiringEpisode"] = Value::Null;
        entry["siteUrl"] = json!(format!("https://bgm.tv/subject/{subject}"));
    }
    true
}

pub(super) fn apply_catalog(state: &mut Value, anime: &[Value]) -> bool {
    apply_catalog_selected(state, anime, None)
}

fn apply_catalog_selected(state: &mut Value, anime: &[Value], only: Option<&HashSet<i64>>) -> bool {
    let mut changed = false;
    for item in anime {
        let subject = value_i64(item.get("bangumiSubjectId"));
        let aid = value_i64(item.get("anilistId"));
        if subject <= 0 || aid <= 0 {
            continue;
        }
        if only.is_some_and(|ids| !ids.contains(&subject) && !ids.contains(&aid)) {
            continue;
        }
        let old = state["following"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| {
                value_i64(entry.get("id")) == aid
                    && eligible(entry)
                    && same_work(entry, item)
                    && (value_i64(entry.get("bangumiId")) <= 0
                        || value_i64(entry.get("bangumiId")) == subject)
            });
        if old.is_some() {
            changed |= bind_verified(state, aid, subject);
        }
        for entry in state["following"].as_array_mut().into_iter().flatten() {
            if entry["source"] == "bangumi"
                && value_i64(entry.get("id")) == subject
                && value_i64(entry.get("anilistId")) <= 0
                && same_work(entry, item)
            {
                entry["anilistId"] = json!(aid);
                entry["syncUpdatedAt"] = json!(now_millis());
                changed = true;
            }
        }
    }
    changed
}

async fn load_subject(
    http: &bangumi::HttpBangumiClient,
    directory: &Path,
    id: i64,
    force: bool,
    now: i64,
) -> Option<bangumi::BangumiSubject> {
    let path = directory.join(format!("mapping-subject-{id}.json"));
    let cached: Value = fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or(Value::Null);
    let age = now - value_i64(cached.get("fetchedAt"));
    let ttl = if force || cached["subject"].is_null() {
        60
    } else {
        86_400
    };
    if cached["version"] == 1 && age >= 0 && age < ttl {
        return serde_json::from_value(cached["subject"].clone()).ok();
    }
    let subject = http
        .get_subject_detail(id)
        .await
        .ok()
        .filter(|subject| subject.id == id);
    let record = json!({"version":1,"fetchedAt":now,"subject":subject});
    let _ = fs::create_dir_all(directory);
    if let Ok(raw) = serde_json::to_vec(&record) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, raw).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
    subject
}

pub(super) async fn repair_with_client(
    state: &Arc<Mutex<Value>>,
    http: &bangumi::HttpBangumiClient,
    directory: &Path,
    force: bool,
    only: Option<&HashSet<i64>>,
) -> bool {
    let _guard = MAPPING_GATE.lock().await;
    let mut changed = false;
    // Reuse identity evidence even when the seasonal page was loaded before a
    // follow arrived from another device. Never read settings or refresh a catalog here.
    for file in fs::read_dir(directory).into_iter().flatten().flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        if !["-WINTER.json", "-SPRING.json", "-SUMMER.json", "-FALL.json"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            continue;
        }
        if let Some((anime, _)) = read_bangumi_season_cache(&file.path(), false) {
            if let Ok(mut state) = state.lock() {
                changed |= apply_catalog_selected(&mut state, &anime, only);
            }
        }
    }
    let targets: Vec<(i64, i64)> = {
        let Ok(state) = state.lock() else {
            return false;
        };
        state["following"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| eligible(entry))
            .filter(|entry| only.is_none_or(|ids| ids.contains(&value_i64(entry.get("id")))))
            .filter_map(|entry| {
                let id = value_i64(entry.get("id"));
                let subject = value_i64(entry.get("bangumiId"));
                let title_match = &state["bangumiTitles"][id.to_string()];
                let subject = if subject > 0 {
                    subject
                } else if title_match["status"] == "matched" {
                    value_i64(title_match.get("subjectId"))
                } else {
                    0
                };
                (subject > 0).then_some((id, subject))
            })
            .collect()
    };
    for (old_id, subject_id) in targets {
        let Some(subject) = load_subject(http, directory, subject_id, force, now_seconds()).await
        else {
            continue;
        };
        let subject = subject_anime(&subject);
        let Ok(mut state) = state.lock() else {
            continue;
        };
        let current = state["following"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| value_i64(entry.get("id")) == old_id && eligible(entry));
        let Some(current) = current else {
            continue;
        };
        if value_i64(current.get("bangumiId")) > 0
            && value_i64(current.get("bangumiId")) != subject_id
        {
            continue;
        }
        if same_work(current, &subject) {
            changed |= bind_verified(&mut state, old_id, subject_id);
        }
    }
    changed
}

pub(super) async fn repair_following(
    app: &AppHandle,
    context: &AppContext,
    force: bool,
    only: Option<&HashSet<i64>>,
) -> bool {
    if context.original {
        return false;
    }
    let base = match context.state.lock() {
        Ok(state) => bangumi_base_urls(&state),
        Err(_) => return false,
    };
    let Ok(client) = context.http_client() else {
        return false;
    };
    let http = bangumi::HttpBangumiClient::new(client, base);
    let changed = repair_with_client(
        &context.state,
        &http,
        &bangumi_cache_dir(context),
        force,
        only,
    )
    .await;
    if changed {
        let _ = context.save_state();
        context.webdav_wakeup.notify_one();
        emit_state(app, context);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bangumi::test_support::MockBangumiServer;

    fn work(id: i64, title: &str, year: i64, month: u32, format: &str) -> Value {
        json!({"id":id,"title":{"native":title},"format":format,
            "startDate":{"year":year,"month":month,"day":1}})
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn base(server: &MockBangumiServer) -> bangumi::BangumiBaseUrls {
        bangumi::BangumiBaseUrls {
            root: server.url(),
            v0: format!("{}/v0", server.url()),
        }
    }

    /// Opt-in read-only upstream audit. The input is a disposable business-data
    /// projection; never point this at the application's live data directory.
    #[test]
    #[ignore = "requires ANILOG_CATALOG_AUDIT_DIR and live public APIs"]
    fn live_catalog_audit() {
        let root = PathBuf::from(
            std::env::var_os("ANILOG_CATALOG_AUDIT_DIR").expect("audit directory required"),
        );
        let input: Value =
            serde_json::from_slice(&fs::read(root.join("input.json")).unwrap()).unwrap();
        let mut state = default_state(false);
        for key in ["following", "tasks", "bangumiTitles", "syncMetadata"] {
            if let Some(value) = input.get(key) {
                state[key] = value.clone();
            }
        }
        let history: Vec<Value> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|task| task["status"] == "completed")
            .cloned()
            .collect();
        let before = state["following"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["mappingPending"] == true)
            .count();
        let state = Arc::new(Mutex::new(state));
        let client = build_http_client().unwrap();
        let base = bangumi::resolve_base_urls("");
        let http = bangumi::HttpBangumiClient::new(client.clone(), base.clone());
        let dir = root.join("bangumi-cache");
        fs::create_dir_all(&dir).unwrap();
        let today = Local::now();
        let year = i64::from(today.year());
        let season = ["WINTER", "SPRING", "SUMMER", "FALL"][(today.month0() / 3) as usize];
        let source = AniListSeasonSource {
            client: client.clone(),
            endpoint: ANILIST_API,
            cache_dir: root.join("anilist-cache"),
        };
        runtime().block_on(async {
            repair_with_client(&state, &http, &dir, false, None).await;
            let snapshot = state.lock().unwrap().clone();
            let result = fetch_season_bangumi_chain_with_policy(
                &client, base, &dir, &json!({}), &snapshot, season, year, Some(&source), true,
            ).await;
            let SeasonFetch::Bangumi { anime, stale, .. } = result else { panic!("Bangumi unavailable") };
            assert!(!stale, "upstream unavailable; live audit incomplete");
            let mut state = state.lock().unwrap();
            apply_catalog(&mut state, &anime);
            let after = state["following"].as_array().unwrap().iter().filter(|entry| entry["mappingPending"] == true).count();
            let retained: Vec<Value> = state["tasks"].as_array().unwrap().iter().filter(|task|task["status"]=="completed").cloned().collect();
            assert_eq!(history, retained, "completed history must remain unchanged");
            println!("Live catalog: {} {season}, {} subjects, {} mapped, {} precise schedules; pending mappings {before} -> {after}; {} completed records unchanged",
                year, anime.len(), anime.iter().filter(|item|value_i64(item.get("anilistId"))>0).count(),
                anime.iter().filter(|item|item["nextAiringEpisode"].is_object()).count(), history.len());
            fs::write(root.join("catalog.json"), serde_json::to_vec(&anime).unwrap()).unwrap();
            fs::write(root.join("state-preview.json"), serde_json::to_vec(&*state).unwrap()).unwrap();
        });
    }

    #[test]
    fn catalog_mapping_is_not_tied_to_a_quarter_or_tv_format() {
        for year in [2026, 2027, 2032] {
            for month in [1, 4, 7, 10] {
                for format in ["TV", "MOVIE", "ONA", "OVA"] {
                    let mut subjects = vec![work(900, "新しい物語 第2期", year, month, format)];
                    let catalog = vec![work(10, "新しい物語 第2期", year, month, format)];
                    link_catalog(&mut subjects, &catalog, &json!({}));
                    assert_eq!(subjects[0]["anilistId"], 10);
                    assert!(subjects[0]["nextAiringEpisode"].is_null());
                }
            }
        }
    }

    #[test]
    fn sequel_date_type_and_ambiguous_candidates_are_not_guessed() {
        let source = work(900, "Same Story 2", 2027, 1, "TV");
        for other in [
            work(10, "Same Story", 2027, 1, "TV"),
            work(10, "Same Story 3", 2027, 1, "TV"),
            work(10, "Same Story 2", 2026, 1, "TV"),
            work(10, "Same Story 2", 2027, 4, "TV"),
            work(10, "Same Story 2", 2027, 1, "MOVIE"),
            json!({"id":10,"title":{"native":"Same Story 2"},"format":"TV"}),
        ] {
            assert!(!same_work(&source, &other));
        }
        let duplicate = [
            work(10, "Same Story 2", 2027, 1, "TV"),
            work(11, "Same Story 2", 2027, 1, "TV"),
        ];
        let mut subjects = vec![source.clone()];
        link_catalog(&mut subjects, &duplicate, &json!({}));
        assert!(subjects[0]["anilistId"].is_null());
        let mut split = vec![source.clone(), work(901, "Same Story 2", 2027, 1, "TV")];
        link_catalog(&mut split, &duplicate[..1], &json!({}));
        assert!(split.iter().all(|item| item["anilistId"].is_null()));
        // Explicit split associations are retained instead of deduplicating subjects.
        split[0]["anilistId"] = json!(10);
        split[1]["anilistId"] = json!(10);
        link_catalog(&mut split, &duplicate, &json!({}));
        assert_eq!(split[0]["anilistId"], split[1]["anilistId"]);
    }

    #[test]
    fn new_evidence_retries_pending_but_respects_user_skip_and_history() {
        let mut state = default_state(false);
        let mut entry = work(10, "Future Story", 2027, 1, "TV");
        entry["mappingPending"] = json!(true);
        entry["bangumiId"] = json!(900);
        state["following"] = json!([entry]);
        let history = json!({"id":"10-1","animeId":10,"episode":1,"status":"completed","completedAt":100,"syncUpdatedAt":100000,"statusSource":"local"});
        state["tasks"] = json!([history.clone(),{"id":"10-2","animeId":10,"episode":2,"status":"pending","statusSource":"local","syncUpdatedAt":200000}]);
        let mut subject = work(900, "Future Story", 2027, 1, "TV");
        subject["bangumiSubjectId"] = json!(900);
        subject["anilistId"] = json!(10);
        assert!(apply_catalog(&mut state, &[subject.clone()]));
        assert_eq!(state["following"][0]["id"], 900);
        assert_eq!(state["following"][0]["mappingPending"], false);
        assert_eq!(state["tasks"][0], history);
        assert_eq!(state["tasks"][1]["id"], "900-2");
        assert!(!apply_catalog(&mut state, &[subject.clone()]));
        let payload = crate::mobile_state::configuration_payload(&state, false);
        assert_eq!(payload["following"][0]["subjectId"], 900);
        assert_eq!(payload["following"][0]["anilistId"], 10);
        assert_eq!(payload["pendingTasks"][0], history);
        let mut skipped = work(10, "Future Story", 2027, 1, "TV");
        skipped["mapping"] = json!({"method":"local","confidence":"low"});
        state["following"] = json!([skipped.clone()]);
        assert!(!apply_catalog(&mut state, &[subject]));
        assert_eq!(state["following"][0], skipped);
    }

    #[test]
    fn direct_mapping_verifies_live_subject_and_caches_misses() {
        let server = MockBangumiServer::spawn(Arc::new(|_, target, _, _| {
            if target == "/v0/subjects/900" {
                (
                    200,
                    vec![],
                    json!({"id":900,"name":"Future Story","platform":"TV","date":"2027-01-01"})
                        .to_string(),
                )
            } else {
                (503, vec![], "{}".into())
            }
        }));
        let http = bangumi::HttpBangumiClient::with_base(base(&server)).unwrap();
        let dir = std::env::temp_dir().join(format!("anilog-identity-{}", server.port()));
        let mut entry = work(10, "Future Story", 2027, 1, "TV");
        entry["bangumiId"] = json!(900);
        entry["mappingPending"] = json!(true);
        let mut missing = work(11, "Missing Story", 2027, 1, "TV");
        missing["bangumiId"] = json!(901);
        missing["syncUpdatedAt"] = json!(100000);
        let mut value = default_state(false);
        value["following"] = json!([entry, missing.clone()]);
        let state = Arc::new(Mutex::new(value));
        runtime().block_on(async {
            assert!(
                repair_with_client(&state, &http, &dir, false, Some(&HashSet::from([10]))).await
            );
            assert_eq!(
                server.requests().len(),
                1,
                "new-follow scope must not fetch other subjects"
            );
            assert!(!repair_with_client(&state, &http, &dir, false, None).await);
            assert!(!repair_with_client(&state, &http, &dir, true, None).await);
        });
        assert_eq!(
            server.requests().len(),
            2,
            "negative cache survives forced refresh within a minute"
        );
        let state = state.lock().unwrap();
        assert_eq!(state["following"][0]["id"], 900);
        assert_eq!(state["following"][1], missing);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_embedded_map_still_enriches_each_live_quarter_and_reuses_cache() {
        for (year, season, month) in [(2026, "FALL", 10), (2027, "WINTER", 1), (2032, "SPRING", 4)]
        {
            let server = MockBangumiServer::spawn(Arc::new(move |method, target, _, body| {
                if method == "GET" && target.starts_with("/v0/subjects?") {
                    return (200,vec![],json!({"total":1,"data":[{"id":900,"name":"Future Story","name_cn":"未来故事","platform":"TV","date":format!("{year}-{month:02}-01") }]}).to_string());
                }
                let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                assert_eq!(request["variables"]["season"], season);
                assert_eq!(request["variables"]["year"], year);
                let mut item = work(10, "Future Story", year, month, "TV");
                let event = json!({"episode":1,"airingAt":now_seconds()+86_400});
                item["airingSchedule"] = json!({"nodes":[event.clone()]});
                item["airedEpisodes"] = json!({"nodes":[]});
                item["nextAiringEpisode"] = event;
                (
                    200,
                    vec![],
                    json!({"data":{"Page":{"pageInfo":{"lastPage":1},"media":[item]}}}).to_string(),
                )
            }));
            let root = std::env::temp_dir().join(format!("anilog-live-quarter-{}", server.port()));
            let dir = root.join("bangumi-cache");
            fs::create_dir_all(&dir).unwrap();
            // A freshly written v0.7.5 empty-mapping cache must refresh after upgrade.
            fs::write(
                dir.join(format!("{year}-{season}.json")),
                json!({"version":1,"fetchedAt":now_millis(),"anime":[]}).to_string(),
            )
            .unwrap();
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let url = server.url();
            let source = AniListSeasonSource {
                client: client.clone(),
                endpoint: &url,
                cache_dir: root.join("anilist-cache"),
            };
            runtime().block_on(async {
                for _ in 0..2 {
                    let result = fetch_season_bangumi_chain_with_policy(
                        &client,
                        base(&server),
                        &dir,
                        &json!({}),
                        &json!({}),
                        season,
                        year,
                        Some(&source),
                        false,
                    )
                    .await;
                    let SeasonFetch::Bangumi { anime, .. } = result else {
                        panic!("expected Bangumi catalog")
                    };
                    assert_eq!(anime.len(), 1);
                    assert_eq!(anime[0]["anilistId"], 10);
                    assert_eq!(anime[0]["nextAiringEpisode"]["episode"], 1);
                }
            });
            assert_eq!(
                server
                    .requests()
                    .iter()
                    .filter(|r| r.method == "POST")
                    .count(),
                1,
                "catalog result must populate per-ID cache"
            );
            assert_eq!(
                server.requests().len(),
                4,
                "second read must reuse seasonal cache"
            );
            let _ = fs::remove_dir_all(root);
        }
    }
}
