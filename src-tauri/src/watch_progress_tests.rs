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
