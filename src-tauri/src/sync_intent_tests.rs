use super::*;

fn followed(revision: i64) -> Value {
    json!({
        "id": 42, "source": "bangumi", "bangumiId": 42,
        "title": {"english": "Test series"}, "displayTitle": "Test series",
        "followedAt": 1, "syncUpdatedAt": revision,
        "bangumiStatus": "doing", "rating": 2, "watchedEpisode": 0
    })
}

#[test]
fn stale_follow_intent_cannot_undo_a_newer_deletion_in_either_direction() {
    for last_pull in [0, 1] {
        let mut initial = default_state(cfg!(feature = "original"));
        let mut entry = followed(2_000);
        entry["localFollowIntentAt"] = json!(2_000);
        entry["lastPulledFromBangumiAt"] = json!(last_pull);
        initial["following"] = json!([entry]);
        initial["tasks"] = json!([
            {"id": "42-1", "animeId": 42, "episode": 1, "status": "completed",
             "createdAt": 1, "syncUpdatedAt": 2_000},
            {"id": "42-2", "animeId": 42, "episode": 2, "status": "pending",
             "createdAt": 1, "syncUpdatedAt": 2_000}
        ]);
        let old_document = document_from_state(&mut initial);
        let mut deleted = initial.clone();
        assert!(remove_following(&mut deleted, 42));
        let deleted_document = document_from_state(&mut deleted);
        let deleted_at = deleted["syncMetadata"]["followingDeletedAt"]["42"].clone();

        for (mut state, remote) in [
            (deleted.clone(), old_document.clone()),
            (initial.clone(), deleted_document.clone()),
        ] {
            for _ in 0..3 {
                merge_document_into_state(&mut state, &remote).unwrap();
                assert!(state["following"].as_array().unwrap().is_empty());
                assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
                assert_eq!(state["tasks"][0]["status"], "completed");
                assert_eq!(
                    state["syncMetadata"]["followingDeletedAt"]["42"],
                    deleted_at
                );
            }
        }
    }
}

#[test]
fn old_follow_intent_does_not_override_a_newer_device_edit() {
    let mut older = default_state(cfg!(feature = "original"));
    let mut entry = followed(2_000);
    entry["localFollowIntentAt"] = json!(1_000);
    older["following"] = json!([entry]);
    let mut newer = older.clone();
    newer["following"][0]["rating"] = json!(9);
    newer["following"][0]["syncUpdatedAt"] = json!(3_000);
    let older_document = document_from_state(&mut older);
    let newer_document = document_from_state(&mut newer);
    merge_document_into_state(&mut older, &newer_document).unwrap();
    merge_document_into_state(&mut newer, &older_document).unwrap();
    assert_eq!(older["following"][0]["rating"], 9);
    assert_eq!(older["following"], newer["following"]);
}

#[test]
fn explicit_refollow_newer_than_tombstone_still_survives() {
    let mut state = default_state(cfg!(feature = "original"));
    let mut entry = followed(4_000);
    entry["localFollowIntentAt"] = json!(4_000);
    state["following"] = json!([entry]);
    let remote = json!({
        "version": 1, "following": [], "tasks": [],
        "followingDeletedAt": {"42": 3_000}
    });
    merge_document_into_state(&mut state, &remote).unwrap();
    assert_eq!(state["following"].as_array().unwrap().len(), 1);
    assert!(
        state["syncMetadata"]["followingDeletedAt"]
            .get("42")
            .is_none()
    );
}

#[test]
fn stale_native_snapshot_cannot_restore_an_unfollowed_series() {
    let mut state = default_state(cfg!(feature = "original"));
    let mut entry = followed(2_000);
    entry["localFollowIntentAt"] = json!(1_000);
    state["following"] = json!([entry]);
    let snapshot = json!({
        "document": document_from_state(&mut state),
        "following": state["following"], "events": []
    });
    remove_following(&mut state, 42);
    mobile_state::merge_snapshot(&mut state, &snapshot, now_seconds()).unwrap();
    assert!(state["following"].as_array().unwrap().is_empty());
}

#[test]
fn repeated_removal_is_idempotent_and_keeps_completed_history() {
    let mut state = default_state(cfg!(feature = "original"));
    state["following"] = json!([followed(2_000)]);
    state["tasks"] = json!([{
        "id": "42-1", "animeId": 42, "episode": 1,
        "status": "completed", "createdAt": 1, "syncUpdatedAt": 2_000
    }]);
    assert!(remove_following(&mut state, 42));
    let removed = state.clone();
    assert!(!remove_following(&mut state, 42));
    assert_eq!(state, removed);
}

#[cfg(not(target_os = "android"))]
#[test]
fn cached_airing_creates_tasks_without_network_and_never_repeats_notifications() {
    let mut state = default_state(cfg!(feature = "original"));
    state["following"] = json!([{"id": 42, "source":"anilist", "title":{"english":"Test series"},
        "displayTitle":"Test series", "followedAt":10}]);
    let batch = anilist_cache::Batch {
        snapshots: BTreeMap::from([(
            42,
            anilist_cache::Snapshot {
                fetched_at: 20,
                media: json!({"id":42, "nextAiringEpisode":{"episode":1,"airingAt":100},
                "futureAiringSchedule":{"nodes":[{"episode":2,"airingAt":200}]},
                "airingSchedule":{"nodes":[]}}),
            },
        )]),
        warnings: vec![],
    };
    let result = apply_anilist_snapshots(&mut state, &batch, cfg!(feature = "original"), 101);
    assert_eq!(result.created, 1);
    assert_eq!(result.aired, 1);
    assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 2);
    let second = apply_anilist_snapshots(&mut state, &batch, cfg!(feature = "original"), 101);
    assert_eq!(second.created, 0);
    assert_eq!(second.aired, 0);
    state["settings"]["createWatchTasks"] = json!(false);
    let third = apply_anilist_snapshots(&mut state, &batch, cfg!(feature = "original"), 201);
    assert_eq!(third.created, 0);
    assert_eq!(third.aired, 1);
}

#[cfg(feature = "standard")]
mod bangumi_cases {
    use super::*;
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;

    fn collection(rating: i64) -> Value {
        json!({
            "subject_id": 42, "subject_type": 2, "type": 3, "rate": rating,
            "ep_status": 0, "tags": [], "private": false,
            "subject": {"id": 42, "name": "Test series", "eps": 12}
        })
    }

    fn page(collections: Value) -> String {
        json!({
            "total": collections.as_array().unwrap().len(),
            "limit": 50, "offset": 0, "data": collections
        })
        .to_string()
    }

    fn hash(value: &Value) -> String {
        bangumi::collection_payload_hash(
            &serde_json::from_value::<bangumi::BangumiCollection>(value.clone()).unwrap(),
        )
    }

    fn sync_state() -> Value {
        let mut state = default_state(false);
        state["bangumi"]["syncEnabled"] = json!(true);
        state["bangumi"]["pullCollections"] = json!(true);
        state["bangumi"]["pushLocalChanges"] = json!(true);
        state["following"] = json!([followed(2_000)]);
        state
    }

    fn client(server: &MockBangumiServer) -> bangumi::HttpBangumiClient {
        bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
            root: server.url(),
            v0: format!("{}/v0", server.url()),
        })
        .unwrap()
    }

    fn tokens() -> bangumi::MemoryTokenStore {
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("unit-test-token").unwrap();
        tokens
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn remote_doing_cannot_reimport_a_deleted_follow_even_with_upload_disabled() {
        let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| {
            (200, vec![], page(json!([collection(2)])))
        }));
        for push_enabled in [false, true] {
            let mut initial = sync_state();
            initial["bangumi"]["pushLocalChanges"] = json!(push_enabled);
            initial["tasks"] = json!([{
                "id": "42-1", "animeId": 42, "subjectId": 42, "episode": 1,
                "status": "completed", "createdAt": 1, "syncUpdatedAt": 1_000
            }]);
            remove_following(&mut initial, 42);
            let state = Mutex::new(initial);
            for _ in 0..2 {
                let report = runtime().block_on(bangumi_sync::run_bangumi_collection_sync(
                    &client(&server),
                    &tokens(),
                    &Mutex::new(Some("unit_user".into())),
                    &state,
                    &json!({}),
                ));
                assert!(report.errors.is_empty(), "{:?}", report.errors);
                assert_eq!(report.followed, 0);
                let state = state.lock().unwrap();
                assert!(state["following"].as_array().unwrap().is_empty());
                assert_eq!(state["tasks"][0]["status"], "completed");
                assert!(
                    state["syncMetadata"]["followingDeletedAt"]["42"]
                        .as_i64()
                        .unwrap()
                        > 0
                );
            }
        }
    }

    #[test]
    fn latest_preserves_a_local_rating_before_the_first_or_next_pull() {
        let server = MockBangumiServer::spawn(Arc::new(|method, target, _, _| {
            if method == "GET" {
                (
                    200,
                    vec![],
                    if target.contains('?') {
                        page(json!([collection(3)]))
                    } else {
                        collection(3).to_string()
                    },
                )
            } else {
                (204, vec![], String::new())
            }
        }));
        for has_baseline in [false, true] {
            let mut initial = sync_state();
            initial["following"][0]["rating"] = json!(9);
            initial["following"][0]["lastChangedBy"] = json!("local");
            if has_baseline {
                initial["following"][0]["lastPulledPayloadHash"] = json!(hash(&collection(2)));
                initial["following"][0]["lastPushedPayloadHash"] = json!(hash(&collection(2)));
            }
            let state = Mutex::new(initial);
            let client = client(&server);
            let tokens = tokens();
            let username = Mutex::new(Some("unit_user".into()));
            let rt = runtime();
            let report = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
                &client,
                &tokens,
                &username,
                &state,
                &json!({}),
            ));
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(state.lock().unwrap()["following"][0]["rating"], 9);
            let report = rt.block_on(bangumi_sync::push_local_changes(
                &client,
                &tokens,
                &username,
                &state,
                &std::env::temp_dir(),
            ));
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.pushed, 1);
            let request = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.method == "PATCH")
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request.body).unwrap()["rate"],
                9
            );
        }
    }

    #[test]
    fn matching_collection_values_converge_without_conflicts_or_writes() {
        let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| {
            (200, vec![], page(json!([collection(2)])))
        }));
        let mut initial = sync_state();
        initial["following"][0]["lastChangedBy"] = json!("local");
        let state = Mutex::new(initial);
        let client = client(&server);
        let tokens = tokens();
        let username = Mutex::new(Some("unit_user".into()));
        let rt = runtime();
        let report = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username,
            &state,
            &json!({}),
        ));
        assert_eq!(report.conflicts, 0);
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username,
            &state,
            &std::env::temp_dir(),
        ));
        assert_eq!(report.pushed, 0);
        assert!(
            server
                .requests()
                .iter()
                .all(|request| request.method == "GET")
        );
    }

    #[test]
    fn deletion_during_subject_fetch_is_rechecked_before_import() {
        let state = Arc::new(Mutex::new(sync_state()));
        state.lock().unwrap()["following"] = json!([]);
        let during_fetch = state.clone();
        let server = MockBangumiServer::spawn(Arc::new(move |_, target, _, _| {
            if target.starts_with("/v0/subjects/") {
                let mut state = during_fetch.lock().unwrap();
                state["syncMetadata"]["followingDeletedAt"]["42"] = json!(now_millis());
                (
                    200,
                    vec![],
                    json!({"id": 42, "type": 2, "name": "Test series"}).to_string(),
                )
            } else {
                let mut remote = collection(2);
                remote.as_object_mut().unwrap().remove("subject");
                (200, vec![], page(json!([remote])))
            }
        }));
        let report = runtime().block_on(bangumi_sync::run_bangumi_collection_sync(
            &client(&server),
            &tokens(),
            &Mutex::new(Some("unit_user".into())),
            &state,
            &json!({}),
        ));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.followed, 0);
        assert!(
            state.lock().unwrap()["following"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn edit_during_subject_fetch_invalidates_an_older_merge_plan() {
        let state = Arc::new(Mutex::new(sync_state()));
        let during_fetch = state.clone();
        let server = MockBangumiServer::spawn(Arc::new(move |_, target, _, _| {
            if target == "/v0/subjects/99" {
                apply_bangumi_rating(&mut during_fetch.lock().unwrap(), 42, Some(9));
                (
                    200,
                    vec![],
                    json!({"id": 99, "type": 2, "name": "Other series"}).to_string(),
                )
            } else {
                (
                    200,
                    vec![],
                    page(json!([
                        collection(3),
                        {"subject_id": 99, "subject_type": 2, "type": 3}
                    ])),
                )
            }
        }));
        let report = runtime().block_on(bangumi_sync::run_bangumi_collection_sync(
            &client(&server),
            &tokens(),
            &Mutex::new(Some("unit_user".into())),
            &state,
            &json!({}),
        ));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let state = state.lock().unwrap();
        let entry = state["following"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == 42)
            .unwrap();
        assert_eq!(entry["rating"], 9);
        assert_eq!(entry["lastChangedBy"], "local");
    }

    #[test]
    fn clearing_a_rating_survives_pull_and_sends_zero_to_bangumi() {
        let server = MockBangumiServer::spawn(Arc::new(|method, target, _, _| {
            if method == "GET" {
                (
                    200,
                    vec![],
                    if target.contains('?') {
                        page(json!([collection(2)]))
                    } else {
                        collection(2).to_string()
                    },
                )
            } else {
                (204, vec![], String::new())
            }
        }));
        let mut initial = sync_state();
        assert!(apply_bangumi_rating(&mut initial, 42, None));
        let state = Mutex::new(initial);
        let client = client(&server);
        let tokens = tokens();
        let username = Mutex::new(Some("unit_user".into()));
        let rt = runtime();
        rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username,
            &state,
            &json!({}),
        ));
        assert_eq!(state.lock().unwrap()["following"][0]["rating"], Value::Null);
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username,
            &state,
            &std::env::temp_dir(),
        ));
        assert_eq!(report.pushed, 1);
        let request = server
            .requests()
            .into_iter()
            .find(|request| request.method == "PATCH")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&request.body).unwrap()["rate"],
            0
        );
    }

    #[test]
    fn local_first_cannot_bypass_the_disabled_upload_switch() {
        let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| {
            (200, vec![], page(json!([collection(3)])))
        }));
        let mut initial = sync_state();
        initial["bangumi"]["conflictPolicy"] = json!("local-first");
        initial["bangumi"]["pushLocalChanges"] = json!(false);
        apply_bangumi_rating(&mut initial, 42, Some(9));
        let state = Mutex::new(initial);
        let client = client(&server);
        let tokens = tokens();
        let username = Mutex::new(Some("unit_user".into()));
        let rt = runtime();
        rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username,
            &state,
            &json!({}),
        ));
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username,
            &state,
            &std::env::temp_dir(),
        ));
        assert_eq!(report.pushed, 0);
        assert_eq!(state.lock().unwrap()["following"][0]["rating"], 9);
        assert!(
            server
                .requests()
                .iter()
                .all(|request| request.method == "GET")
        );
    }

    #[test]
    fn failed_upload_does_not_acknowledge_or_erase_the_local_edit() {
        let server = MockBangumiServer::spawn(Arc::new(|method, target, _, _| {
            if method == "GET" {
                (
                    200,
                    vec![],
                    if target.contains('?') {
                        page(json!([collection(2)]))
                    } else {
                        collection(2).to_string()
                    },
                )
            } else {
                (400, vec![], "{}".into())
            }
        }));
        let mut initial = sync_state();
        apply_bangumi_rating(&mut initial, 42, Some(9));
        let state = Mutex::new(initial);
        let client = client(&server);
        let tokens = tokens();
        let username = Mutex::new(Some("unit_user".into()));
        let rt = runtime();
        for _ in 0..2 {
            rt.block_on(bangumi_sync::run_bangumi_collection_sync(
                &client,
                &tokens,
                &username,
                &state,
                &json!({}),
            ));
            let report = rt.block_on(bangumi_sync::push_local_changes(
                &client,
                &tokens,
                &username,
                &state,
                &std::env::temp_dir(),
            ));
            assert!(!report.errors.is_empty());
            let state = state.lock().unwrap();
            assert_eq!(state["following"][0]["rating"], 9);
            assert!(state["following"][0]["lastPushedPayloadHash"].is_null());
        }
    }

    #[test]
    fn push_plan_is_rechecked_after_remote_existence_lookup() {
        let state = Arc::new(Mutex::new(sync_state()));
        apply_bangumi_rating(&mut state.lock().unwrap(), 42, Some(9));
        let during_fetch = state.clone();
        let server = MockBangumiServer::spawn(Arc::new(move |method, _, _, _| {
            if method == "GET" {
                apply_bangumi_rating(&mut during_fetch.lock().unwrap(), 42, Some(10));
                (200, vec![], collection(2).to_string())
            } else {
                (204, vec![], String::new())
            }
        }));
        let report = runtime().block_on(bangumi_sync::push_local_changes(
            &client(&server),
            &tokens(),
            &Mutex::new(Some("unit_user".into())),
            &state,
            &std::env::temp_dir(),
        ));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.pushed, 0);
        assert_eq!(state.lock().unwrap()["following"][0]["rating"], 10);
        assert!(
            server
                .requests()
                .iter()
                .all(|request| request.method == "GET")
        );
    }

    #[test]
    fn upload_acknowledges_the_sent_rating_not_a_later_local_edit() {
        let mut initial = sync_state();
        initial["following"][0]["lastPulledPayloadHash"] = json!(hash(&collection(2)));
        apply_bangumi_rating(&mut initial, 42, Some(9));
        let sent_hash = bangumi_sync::local_collection_hash(&initial["following"][0]);
        let state = Arc::new(Mutex::new(initial));
        let during_upload = state.clone();
        let server = MockBangumiServer::spawn(Arc::new(move |method, _, _, _| {
            assert_eq!(method, "PATCH");
            apply_bangumi_rating(&mut during_upload.lock().unwrap(), 42, Some(10));
            (204, vec![], String::new())
        }));
        let report = runtime().block_on(bangumi_sync::push_local_changes(
            &client(&server),
            &tokens(),
            &Mutex::new(Some("unit_user".into())),
            &state,
            &std::env::temp_dir(),
        ));
        assert_eq!(report.pushed, 1);
        let state = state.lock().unwrap();
        let current = &state["following"][0];
        assert_eq!(current["rating"], 10);
        assert_eq!(current["lastPushedPayloadHash"], sent_hash);
        assert_ne!(bangumi_sync::local_collection_hash(current), sent_hash);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn bangumi_notifications_use_cached_precision_without_requiring_task_creation() {
        let now = now_seconds();
        let root = std::env::temp_dir().join(format!(
            "anilog-precision-notification-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let directory = root.join("bangumi-cache");
        fs::create_dir_all(&directory).unwrap();
        let date = chrono::DateTime::from_timestamp(now - 60, 0)
            .unwrap()
            .date_naive()
            .to_string();
        fs::write(
            directory.join("episodes-42.json"),
            json!({
                "fetchedAt": now,
                "paged": {"total":1, "limit":200, "offset":0,
                    "data":[{"id":1001,"type":0,"ep":1,"sort":1,"airdate":date}]}
            })
            .to_string(),
        )
        .unwrap();
        anilist_cache::store_snapshot(&root.join("anilist-cache"), 99, &anilist_cache::Snapshot {
            fetched_at: now - 120,
            media: json!({"id":99, "airingSchedule":{"nodes":[{"episode":1,"airingAt":now - 60}]}})
        }).unwrap();
        let mut state = sync_state();
        state["following"][0]["anilistId"] = json!(99);
        state["following"][0]["followedAt"] = json!(now - 3600);
        state["settings"]["createWatchTasks"] = json!(false);
        assert_eq!(
            claim_bangumi_cached_notifications(&mut state, &directory, now),
            1
        );
        assert_eq!(
            claim_bangumi_cached_notifications(&mut state, &directory, now),
            0
        );
        assert!(state["tasks"].as_array().unwrap().is_empty());
        state["seenAiringEvents"] = json!([]);
        state["following"][0]["bangumiStatus"] = json!("on_hold");
        assert_eq!(
            claim_bangumi_cached_notifications(&mut state, &directory, now),
            0
        );
    }
}
