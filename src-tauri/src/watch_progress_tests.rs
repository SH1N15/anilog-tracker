use super::*;

fn progress_state() -> Value {
    let mut state = default_state(false);
    state["following"] = json!([{
        "id": 633836, "source": "bangumi", "anilistId": 189046,
        "title": {"english": "Split season"}, "displayTitle": "Split season",
        "episodes": 8, "watchedEpisode": 4, "bangumiStatus": "doing",
        "followedAt": 1, "syncUpdatedAt": 1000, "lastChangedBy": "bangumi"
    }]);
    state["tasks"] = json!([{
        "id": "633836-5", "animeId": 633836, "subjectId": 633836,
        "episodeId": 1656862, "episode": 5, "status": "pending",
        "airingAt": 100, "createdAt": 110, "syncUpdatedAt": 1000
    }]);
    state
}

#[test]
fn completion_and_undo_update_following_progress_immediately() {
    let mut state = progress_state();
    assert!(toggle_task_status(&mut state["tasks"][0]));
    let task = state["tasks"][0].clone();
    update_progress_after_task_toggle(&mut state, &task);
    assert_eq!(state["following"][0]["watchedEpisode"], 5);
    assert!(!toggle_task_status(&mut state["tasks"][0]));
    let task = state["tasks"][0].clone();
    update_progress_after_task_toggle(&mut state, &task);
    assert_eq!(state["following"][0]["watchedEpisode"], 4);
}

#[test]
fn existing_history_repairs_stale_progress_without_counting_global_number_duplicates() {
    let mut state = progress_state();
    state["tasks"] = json!(
        (1..=5)
            .chain([5, 15])
            .map(|episode| json!({
                "id": format!("633836-{episode}"), "animeId": 633836, "subjectId": 633836,
                "episode": episode, "status": "completed"
            }))
            .collect::<Vec<_>>()
    );
    heal_following_progress(&mut state);
    assert_eq!(state["following"][0]["watchedEpisode"], 5);
    let snapshot = state.clone();
    heal_following_progress(&mut state);
    assert_eq!(state, snapshot);
}

#[test]
fn future_completed_legacy_record_is_preserved_but_cannot_inflate_progress() {
    let mut state = progress_state();
    state["following"][0]["episodes"] = json!(13);
    state["following"][0]["watchedEpisode"] = json!(12);
    state["tasks"] = json!((1..=11).chain([13]).map(|episode| json!({
        "id": format!("633836-{episode}"), "animeId": 633836, "subjectId": 633836,
        "episode": episode, "episodeId": 1000 + episode, "status": "completed",
        "airingAt": if episode == 13 { now_seconds() + 14 * 86400 } else { 100 },
        "airingPrecision": "instant", "airingSource": "bangumi_episode",
        "syncUpdatedAt": 1000, "lastPushedToBangumiAt": 1
    })).collect::<Vec<_>>());
    heal_following_progress(&mut state);
    assert_eq!(state["following"][0]["watchedEpisode"], 11);
    assert_eq!(state["tasks"].as_array().unwrap().len(), 12);
    let future = &state["tasks"][11];
    assert_eq!(future["status"], "completed", "retain the original record for review");
    assert_eq!(future["completionReview"]["decision"], "review");
    assert_eq!(future["lastPushedToBangumiAt"], 1);
    let snapshot = state.clone();
    heal_following_progress(&mut state);
    assert_eq!(state, snapshot, "repair is idempotent");
}

#[test]
fn review_and_reset_survive_old_device_merges_without_losing_history() {
    let mut state = progress_state();
    let now = now_seconds();
    state["following"][0]["watchedEpisode"] = json!(1);
    state["tasks"][0] = json!({
        "id": "633836-5", "subjectId": 633836, "animeId": 633836, "episode": 5,
        "episodeId": 1656862, "status": "completed", "syncUpdatedAt": 1000,
        "completedAt": 100, "createdAt": 80, "airingAt": now + 86400,
        "airingPrecision": "instant", "airingSource": "bangumi_episode"
    });
    let old = document_from_state(&mut state);
    heal_following_progress(&mut state);
    assert_eq!(state["following"][0]["watchedEpisode"], 0);
    for _ in 0..3 {
        merge_document_into_state(&mut state, &old).unwrap();
        assert!(watch_history::needs_review(&state["tasks"][0]));
        assert_eq!(state["following"][0]["watchedEpisode"], 0);
    }
    watch_history::resolve(&mut state["tasks"][0], false, now).unwrap();
    for _ in 0..3 {
        merge_document_into_state(&mut state, &old).unwrap();
        reconcile_following_entries(&mut state, &json!({}), false);
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
        assert!(watch_history::is_reset(&state["tasks"][0]));
        assert_eq!(state["tasks"][0]["completionReview"]["originalCompletedAt"], 100);
        assert_eq!(state["following"][0]["watchedEpisode"], 0);
    }
    let native = json!({"document": document_from_state(&mut state), "following": [{
        "id":633836, "scheduleUpdatedAt": now * 1000, "nextEpisode":5, "nextAiringAt":now + 86400,
        "episodeSchedule":[{"episode":5,"episodeId":1656862,"airingAt":now+86400,"airingPrecision":"instant"}]
    }]});
    mobile_state::merge_snapshot(&mut state, &native, now).unwrap();
    assert!(watch_history::is_reset(&state["tasks"][0]));
}

#[test]
fn partial_history_never_lowers_unrelated_remote_progress() {
    let mut state = progress_state();
    state["following"][0]["watchedEpisode"] = json!(7);
    state["tasks"][0]["status"] = json!("completed");
    state["tasks"][0]["airingSource"] = json!("bangumi_episode");
    state["tasks"][0]["airingPrecision"] = json!("instant");
    state["tasks"][0]["airingAt"] = json!(now_seconds() + 86400);
    heal_following_progress(&mut state);
    assert!(watch_history::needs_review(&state["tasks"][0]));
    assert_eq!(state["following"][0]["watchedEpisode"], 7);
}

#[cfg(not(target_os = "android"))]
#[test]
fn delayed_season_keeps_upstream_date_while_flagging_an_unaired_completion() {
    let now = 1789362000;
    let records = vec![
        bangumi::BangumiEpisode { id: 1001, ep: Some(1.0), sort: Some(13.0), airdate: Some("2026-07-05".into()), ..Default::default() },
        bangumi::BangumiEpisode { id: 1002, ep: Some(2.0), sort: Some(14.0), airdate: Some("2026-07-12".into()), ..Default::default() },
        bangumi::BangumiEpisode { id: 1712, ep: Some(12.0), sort: Some(24.0), airdate: Some("2026-09-27".into()), ..Default::default() },
        bangumi::BangumiEpisode { id: 1713, ep: Some(13.0), sort: Some(25.0), airdate: Some("2026-10-04".into()), ..Default::default() },
    ];
    let precision = HashMap::from([(1, 1783238400), (2, 1783843200), (12, 1790496000), (13, 1791100800)]);
    let mut state = progress_state();
    state["following"][0]["episodes"] = json!(13);
    state["following"][0]["watchedEpisode"] = json!(1);
    state["tasks"] = json!([{
        "id":"633836-13", "animeId":633836, "subjectId":633836, "episode":13, "status":"completed",
        "syncUpdatedAt":1000, "airingAt":100
    }]);
    apply_bangumi_episode_records_to_state(&mut state, 633836, &records, now, false, Some(&precision));
    assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 12);
    assert_eq!(state["following"][0]["nextAiringEpisode"]["airingAt"], 1790496000);
    assert!(watch_history::needs_review(&state["tasks"][0]));
    assert_eq!(state["following"][0]["watchedEpisode"], 0);
}

#[test]
fn future_completed_record_is_not_uploaded_even_with_explicit_old_provenance() {
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;
    let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| (204, vec![], String::new())));
    let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
        root: server.url(), v0: format!("{}/v0", server.url()),
    }).unwrap();
    let tokens = bangumi::MemoryTokenStore::new();
    tokens.store("unit-test-token").unwrap();
    let mut initial = progress_state();
    initial["bangumi"]["pushCompletedEpisodes"] = json!(true);
    initial["bangumi"]["pushLocalChanges"] = json!(false);
    initial["tasks"][0]["status"] = json!("completed");
    initial["tasks"][0]["lastChangedBy"] = json!("local");
    initial["tasks"][0]["airingPrecision"] = json!("instant");
    initial["tasks"][0]["airingAt"] = json!(now_seconds() + 86400);
    let state = Mutex::new(initial);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let report = runtime.block_on(bangumi_sync::push_local_changes(
        &http, &tokens, &Mutex::new(Some("unit_user".to_string())), &state, &std::env::temp_dir()));
    assert_eq!(report.pushed, 0);
    assert!(server.requests().is_empty());
    {
        let mut guard = state.lock().unwrap();
        guard["tasks"][0]["airingSource"] = json!("bangumi_episode");
        heal_following_progress(&mut guard);
        watch_history::resolve(&mut guard["tasks"][0], false, now_seconds()).unwrap();
    }
    let report = runtime.block_on(bangumi_sync::push_local_changes(
        &http, &tokens, &Mutex::new(Some("unit_user".to_string())), &state, &std::env::temp_dir()));
    assert_eq!(report.pushed, 1, "only the explicit reset is sent");
    let body: Value = serde_json::from_str(&server.requests()[0].body).unwrap();
    assert_eq!(body["type"], 0);
}

#[test]
fn unproven_legacy_completion_is_never_an_automatic_bangumi_write() {
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;
    let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| (204, vec![], String::new())));
    let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
        root: server.url(), v0: format!("{}/v0", server.url()),
    }).unwrap();
    let tokens = bangumi::MemoryTokenStore::new();
    tokens.store("unit-test-token").unwrap();
    let mut initial = progress_state();
    initial["bangumi"]["pushCompletedEpisodes"] = json!(true);
    initial["bangumi"]["pushLocalChanges"] = json!(false);
    initial["tasks"][0]["status"] = json!("completed");
    let state = Mutex::new(initial);
    let report = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(bangumi_sync::push_local_changes(
            &http, &tokens, &Mutex::new(Some("unit_user".to_string())), &state, &std::env::temp_dir()));
    assert_eq!(report.pushed, 0);
    assert!(server.requests().is_empty());
}

#[test]
fn episode_upload_rechecks_plans_and_never_acknowledges_a_later_revision() {
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;
    for change_before_batch in [true, false] {
        let mut initial = progress_state();
        initial["bangumi"]["pushCompletedEpisodes"] = json!(true);
        initial["bangumi"]["pushLocalChanges"] = json!(change_before_batch);
        initial["following"][0]["lastChangedBy"] = json!("local");
        initial["following"][0]["lastPulledPayloadHash"] = json!("known");
        initial["tasks"][0]["status"] = json!("completed");
        initial["tasks"][0]["lastChangedBy"] = json!("local");
        let state = Arc::new(Mutex::new(initial));
        let changing = Arc::clone(&state);
        let server = MockBangumiServer::spawn(Arc::new(move |_, _, _, _| {
            let mut current = changing.lock().unwrap();
            current["tasks"][0]["syncUpdatedAt"] = json!(3000);
            current["tasks"][0]["completionReview"] = json!({"decision":"review"});
            (204, vec![], String::new())
        }));
        let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
            root: server.url(), v0: format!("{}/v0", server.url()),
        }).unwrap();
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("unit-test-token").unwrap();
        let report = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
            .block_on(bangumi_sync::push_local_changes(
                &http, &tokens, &Mutex::new(Some("unit_user".into())), &state, &std::env::temp_dir()));
        assert_eq!(report.pushed, 1);
        assert_eq!(server.requests().len(), 1);
        assert_eq!(server.requests()[0].target.ends_with("/episodes"), !change_before_batch);
        assert!(state.lock().unwrap()["tasks"][0].get("lastPushedToBangumiAt").is_none());
    }
}

#[test]
fn unchanged_remote_hash_repairs_doing_progress_but_not_unpushed_local_progress() {
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;

    let collection = json!({
        "subject_id": 633836, "subject_type": 2, "type": 3, "rate": 9,
        "ep_status": 5, "tags": []
    });
    let remote_hash = bangumi::collection_payload_hash(
        &serde_json::from_value::<bangumi::BangumiCollection>(collection.clone()).unwrap(),
    );
    let server = MockBangumiServer::spawn(Arc::new(move |_, _, _, _| {
        (
            200,
            vec![],
            json!({"total": 1, "limit": 50, "offset": 0, "data": [collection]}).to_string(),
        )
    }));
    let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
        root: server.url(),
        v0: format!("{}/v0", server.url()),
    })
    .unwrap();
    let tokens = bangumi::MemoryTokenStore::new();
    tokens.store("unit-test-token").unwrap();
    let username = Mutex::new(Some("unit_user".to_string()));
    let mut initial = progress_state();
    initial["bangumi"]["syncEnabled"] = json!(true);
    initial["following"][0]["lastPulledPayloadHash"] = json!(remote_hash);
    let state = Mutex::new(initial);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let report = runtime.block_on(bangumi_sync::run_bangumi_collection_sync(
        &http,
        &tokens,
        &username,
        &state,
        &json!({}),
    ));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(state.lock().unwrap()["following"][0]["watchedEpisode"], 5);

    {
        let mut state = state.lock().unwrap();
        state["following"][0]["watchedEpisode"] = json!(6);
        state["tasks"][0]["status"] = json!("completed");
        state["tasks"][0]["lastChangedBy"] = json!("local");
    }
    runtime.block_on(bangumi_sync::run_bangumi_collection_sync(
        &http,
        &tokens,
        &username,
        &state,
        &json!({}),
    ));
    assert_eq!(state.lock().unwrap()["following"][0]["watchedEpisode"], 6);
}

#[test]
fn completion_undo_is_written_once_and_uses_the_same_episode_identity() {
    use bangumi::BangumiTokenStore;
    use bangumi::test_support::MockBangumiServer;
    let server = MockBangumiServer::spawn(Arc::new(|_, _, _, _| (204, vec![], String::new())));
    let http = bangumi::HttpBangumiClient::with_base(bangumi::BangumiBaseUrls {
        root: server.url(),
        v0: format!("{}/v0", server.url()),
    })
    .unwrap();
    let tokens = bangumi::MemoryTokenStore::new();
    tokens.store("unit-test-token").unwrap();
    let username = Mutex::new(Some("unit_user".to_string()));
    let mut initial = progress_state();
    initial["bangumi"]["pushCompletedEpisodes"] = json!(true);
    initial["tasks"][0]["status"] = json!("completed");
    initial["tasks"][0]["lastPushedToBangumiAt"] = json!(200);
    assert!(!toggle_task_status(&mut initial["tasks"][0]));
    let state = Mutex::new(initial);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let report = runtime.block_on(bangumi_sync::push_local_changes(
        &http,
        &tokens,
        &username,
        &state,
        &std::env::temp_dir(),
    ));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.pushed, 1);
    let requests = server.requests();
    assert_eq!(
        requests[0].target,
        "/v0/users/-/collections/633836/episodes"
    );
    let body: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["episode_id"], json!([1656862]));
    assert_eq!(body["type"], 0);
    let report = runtime.block_on(bangumi_sync::push_local_changes(
        &http,
        &tokens,
        &username,
        &state,
        &std::env::temp_dir(),
    ));
    assert_eq!(report.pushed, 0);
    assert_eq!(server.requests().len(), 1);
}

#[cfg(not(target_os = "android"))]
#[test]
fn same_day_multiple_split_episodes_do_not_share_one_precision_timestamp() {
    let records = vec![
        bangumi::BangumiEpisode {
            id: 1001,
            ep: Some(1.0),
            sort: Some(78.0),
            airdate: Some("2026-09-10".into()),
            ..Default::default()
        },
        bangumi::BangumiEpisode {
            id: 1002,
            ep: Some(2.0),
            sort: Some(79.0),
            airdate: Some("2026-09-10".into()),
            ..Default::default()
        },
    ];
    let midnight = chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00Z")
        .unwrap()
        .timestamp();
    let precise = HashMap::from([(12, midnight + 13 * 3600)]);
    let schedule = bangumi_episode_schedule_with_precision(&records, midnight, &precise);
    assert!(
        schedule
            .iter()
            .all(|(_, _, at, aired)| *at == midnight && !aired)
    );
}
