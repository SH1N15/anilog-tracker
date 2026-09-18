use super::*;

static NEXT_DIRECTORY: AtomicI64 = AtomicI64::new(0);

struct TestContext(AppContext);

#[test]
fn production_anilist_queries_use_media_schedule_arguments_supported_by_api() {
    let fields = regex::Regex::new(r"airingSchedule\s*\(([^)]*)\)").unwrap();
    for query in [anilist_cache::MEDIA_QUERY, SEASON_QUERY] {
        let mut found = 0;
        for field in fields.captures_iter(query) {
            for argument in field[1].split(',') {
                let name = argument.split(':').next().unwrap().trim();
                assert!(["notYetAired", "page", "perPage"].contains(&name),
                    "Media.airingSchedule does not accept {name}; sort belongs to Page.airingSchedules");
            }
            found += 1;
        }
        assert!(found >= 2);
    }
}

impl TestContext {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "anilog-startup-schedule-{}-{}-{}",
            std::process::id(),
            now_millis(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(AppContext {
            state: Arc::new(Mutex::new(default_state(cfg!(feature = "original")))),
            runtime: Arc::new(Mutex::new(json!({"platform": "android"}))),
            data_dir: directory.clone(),
            cache_dir: directory.join("season-cache"),
            client: reqwest::Client::new(),
            original: cfg!(feature = "original"),
            sync_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(not(target_os = "android"))]
            schedule_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(desktop)]
            main_window_opening: Arc::new(AtomicBool::new(false)),
            bangumi_lookup_lock: Arc::new(tokio::sync::Mutex::new(())),
            bangumi_unavailable_until: Arc::new(AtomicI64::new(0)),
            offline_bangumi: Arc::new(json!({})),
            #[cfg(feature = "standard")]
            bangumi_tokens: Arc::new(bangumi::MemoryTokenStore::new()),
            #[cfg(feature = "standard")]
            bangumi_username_cache: Arc::new(Mutex::new(None)),
            #[cfg(feature = "standard")]
            bangumi_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0.data_dir);
    }
}

#[test]
fn unreadable_existing_state_is_not_replaced_with_defaults() {
    let context = TestContext::new();
    let path = context.0.state_path();
    assert_eq!(read_saved_state(&path, context.0.original).unwrap()["following"], json!([]));
    for invalid in [b"{\"following\": [".as_slice(), b"null".as_slice()] {
        fs::write(&path, invalid).unwrap();
        assert!(read_saved_state(&path, context.0.original).is_err());
        assert_eq!(fs::read(&path).unwrap(), invalid);
    }
}

#[test]
fn concurrent_state_saves_preserve_settings_and_complete_snapshots() {
    let context = TestContext::new();
    {
        let mut state = context.0.state.lock().unwrap();
        state["following"] = json!([{
            "id": 607340, "displayTitle": "Test series", "followedAt": 1,
            "syncUpdatedAt": 1000
        }]);
        state["settings"]["createWatchTasks"] = json!(false);
        #[cfg(feature = "standard")]
        {
            state["bangumi"]["syncEnabled"] = json!(true);
        }
    }
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let context = context.0.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                (0..24)
                    .map(|_| {
                        context.state.lock().unwrap()["lastSyncAt"] = json!(now_seconds());
                        context.save_state()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect();
    assert!(
        results.iter().all(Result::is_ok),
        "concurrent saves must not compete for the same temporary file: {:?}",
        results.iter().filter_map(|result| result.as_ref().err()).collect::<Vec<_>>()
    );
    let restored = merge_defaults(
        serde_json::from_slice(&fs::read(context.0.state_path()).unwrap()).unwrap(),
        context.0.original,
    );
    assert_eq!(restored["following"][0]["id"], 607340);
    assert_eq!(restored["settings"]["createWatchTasks"], false);
    #[cfg(feature = "standard")]
    assert_eq!(restored["bangumi"]["syncEnabled"], true);
}

#[test]
fn invalid_native_snapshot_does_not_reset_local_settings_or_follows() {
    let context = TestContext::new();
    let mut state = context.0.state.lock().unwrap();
    state["following"] = json!([{
        "id": 607340, "displayTitle": "Test series", "followedAt": 1,
        "syncUpdatedAt": 1000
    }]);
    #[cfg(feature = "standard")]
    {
        state["bangumi"]["syncEnabled"] = json!(true);
    }
    let before = state.clone();
    assert!(mobile_state::merge_snapshot(
        &mut state,
        &json!({"document": {"version": 999}}),
        now_seconds()
    ).is_err());
    assert_eq!(*state, before);
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
mod schedules {
    use super::*;
    use bangumi::test_support::MockBangumiServer;

    fn fixtures() -> Value {
        serde_json::from_str(include_str!("../fixtures/anilist/schedule-regressions.json")).unwrap()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn failed_schedule_supplement_is_not_reported_as_successful_sync() {
        let context = TestContext::new();
        let no_webdav: Option<std::future::Ready<anyhow::Result<Value>>> = None;
        let result = runtime().block_on(run_full_bangumi_sync_core(
            &context.0,
            BangumiSyncScope::LocalOnly { reason: "No account" },
            no_webdav,
            async { Ok(json!({"warning": "AniList HTTP 400"})) },
        )).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["report"]["errors"][0], "AniList HTTP 400");
        assert_eq!(context.0.state.lock().unwrap()["bangumiSyncStatus"]["lastSyncError"], "AniList HTTP 400");
    }

    #[test]
    fn reported_series_get_identical_precise_next_from_cache_when_followed() {
        let fixture = fixtures();
        let now = value_i64(fixture.get("checkedAt"));
        for case in fixture["cases"].as_array().unwrap() {
            let context = TestContext::new();
            let subject = value_i64(case.get("subjectId"));
            let anilist = value_i64(case.get("anilistId"));
            let path = bangumi_cache_dir(&context.0).join(format!("episodes-{subject}.json"));
            fs::write(path, json!({"fetchedAt": now, "paged": {
                "total": case["episodes"].as_array().unwrap().len(), "data": case["episodes"]
            }}).to_string()).unwrap();
            anilist_cache::store_snapshot(&context.0.cache_dir.join("anilist-cache"), anilist,
                &anilist_cache::Snapshot { fetched_at: now, media: case["media"].clone() }).unwrap();
            let anime = json!({
                "id": subject, "bangumiSubjectId": subject, "source": "bangumi",
                "anilistId": anilist, "episodes": case["media"]["episodes"],
                "title": {"romaji": "Regression series"}, "nameCn": "Regression series",
                // This unverified card value must not be copied into a follow.
                "nextAiringEpisode": {"episode": 99, "airingAt": now + 1}
            });
            let mut state = default_state(false);
            add_following_entry(&mut state, &anime, false, &json!({}));
            assert!(state["following"][0]["nextAiringEpisode"].is_null());
            let added = state["following"].as_array().unwrap().clone();
            hydrate_cached_follow_schedules(&mut state, &context.0.cache_dir, false, &added, now);
            let next = &state["following"][0]["nextAiringEpisode"];
            assert_eq!(next["episode"], case["expectedNext"]["episode"]);
            assert_eq!(next["episodeId"], case["expectedNext"]["episodeId"]);
            assert_eq!(next["airingAt"], case["expectedNext"]["airingAt"]);
            assert_eq!(next["airingPrecision"], "instant");
            assert!(state["tasks"].as_array().unwrap().is_empty());
            let document = document_from_state(&mut state);
            assert!(document["following"][0].get("nextAiringEpisode").is_none());
        }
    }

    #[test]
    fn targeted_manual_refresh_skips_other_subjects_and_reuses_short_cooldown() {
        let fixture = fixtures();
        let now = value_i64(fixture.get("checkedAt"));
        let episodes = fixture["cases"][0]["episodes"].clone();
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&received);
        let server = MockBangumiServer::spawn(Arc::new(move |_, target, _, _| {
            recorded.lock().unwrap().push(target.to_string());
            (200, vec![], json!({"total": 5, "data": episodes}).to_string())
        }));
        let context = TestContext::new();
        let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
            root: server.url(), v0: format!("{}/v0", server.url()),
        }).unwrap();
        {
            let mut state = context.0.state.lock().unwrap();
            state["following"] = json!([
                {"id": 1, "source": "bangumi", "followedAt": now},
                {"id": 607340, "source": "bangumi", "followedAt": now, "anilistId": 202269}
            ]);
        }
        let path = bangumi_cache_dir(&context.0).join("episodes-607340.json");
        fs::write(path, json!({"fetchedAt": now - 3600, "paged": {"total": 0, "data": []}}).to_string()).unwrap();
        let only = HashSet::from([607340]);
        runtime().block_on(apply_bangumi_episode_authority_with_client(&context.0, &http, now, false, Some(&only)));
        assert!(received.lock().unwrap().is_empty(), "automatic refresh honors the day cache");
        runtime().block_on(apply_bangumi_episode_authority_with_client(&context.0, &http, now, true, Some(&only)));
        let requests = received.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("subject_id=607340"));
        runtime().block_on(apply_bangumi_episode_authority_with_client(&context.0, &http, now + 10, true, Some(&only)));
        assert_eq!(received.lock().unwrap().len(), 1, "double refresh honors the one-minute floor");
    }

    #[test]
    fn one_calendar_match_does_not_rebind_shared_anilist_numbering() {
        let fixture = fixtures();
        let case = &fixture["cases"][1];
        let records: Vec<bangumi::BangumiEpisode> = serde_json::from_value(case["episodes"].clone()).unwrap();
        let precision = HashMap::from([(1, 1783238400), (12, 1789891200)]);
        assert!(!anilist_uses_subject_episode_numbers(&records, &precision, 1789345800));
        let rows = bangumi_episode_schedule_with_precision(&records, 1789345800, &precision);
        assert_eq!(rows.iter().find(|row| row.0 == 12).unwrap().2, 1790467200);
    }

    #[test]
    fn future_date_coincidence_cannot_override_established_season_numbering() {
        let fixture = fixtures();
        let case = &fixture["cases"][1];
        let records: Vec<bangumi::BangumiEpisode> = serde_json::from_value(case["episodes"].clone()).unwrap();
        let precision = HashMap::from([
            (1, 1783238400), (2, 1783843200), (11, 1789286400),
            (12, 1789891200), (13, 1790496000),
        ]);
        let now = value_i64(fixture.get("checkedAt"));
        assert!(anilist_uses_subject_episode_numbers(&records, &precision, now));
        let rows = bangumi_episode_schedule_with_precision(&records, now, &precision);
        assert_eq!(rows.iter().find(|row| row.0 == 12).unwrap().2, 1789891200);
        assert_eq!(rows.iter().find(|row| row.0 == 13).unwrap().2, 1790496000);
    }

    #[test]
    fn precise_schedule_can_fill_a_missing_bangumi_airdate() {
        let now = 1_789_318_800;
        let at = 1_789_387_200;
        let records = vec![bangumi::BangumiEpisode {
            id: 1_702_103,
            ep: Some(11.0),
            sort: Some(11.0),
            airdate: None,
            ..Default::default()
        }];
        let precision = HashMap::from([(11, at)]);
        let schedule = bangumi_episode_schedule_with_precision(&records, now, &precision);
        assert_eq!(schedule, vec![(11, 1_702_103, at, false)]);
        assert!(bangumi_episode_schedule_with_precision(&records, now, &HashMap::new()).is_empty());
    }
}
