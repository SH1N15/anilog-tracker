use serde_json::{Value, json};
use std::collections::HashSet;

use super::{merge_document_into_state, value_bool, value_i64, value_string};

pub(super) fn configuration_payload(state: &Value, original: bool) -> Value {
    let following: Vec<Value> = state["following"]
        .as_array()
        .into_iter()
        .flatten()
        .cloned()
        .map(|mut item| {
            let id = value_i64(item.get("id"));
            let bangumi = value_string(item.get("source")) == "bangumi";
            if !original {
                item["subjectId"] = if bangumi { json!(id) } else { Value::Null };
                if !bangumi {
                    item["anilistId"] = json!(id);
                }
            }
            let next = item["nextAiringEpisode"].clone();
            item["nextEpisode"] = json!(value_i64(next.get("episode")));
            item["nextAiringAt"] = json!(value_i64(next.get("airingAt")));
            item["nextEpisodeId"] = next["episodeId"].clone();
            item["nextAiringPrecision"] = next
                .get("airingPrecision")
                .cloned()
                .unwrap_or_else(|| json!(if bangumi { "unknown" } else { "instant" }));
            item
        })
        .collect();
    let settings = &state["settings"];
    json!({
        "following": following,
        "pendingTasks": state["tasks"],
        "followingDeletedAt": state["syncMetadata"]["followingDeletedAt"],
        "notificationsEnabled": value_bool(settings.get("notifyWhenAired")),
        "createTasksEnabled": value_bool(settings.get("createWatchTasks")),
        "dailyTaskReminderEnabled": value_bool(settings.get("dailyTaskReminderEnabled")),
        "dailyTaskReminderTime": value_string(settings.get("dailyTaskReminderTime")),
        "uiLanguage": value_string(settings.get("uiLanguage")),
        "bangumiApiBaseUrl": if original { String::new() } else { value_string(settings.get("bangumiApiBaseUrl")) },
        "pullCollections": !original && value_bool(state["bangumi"].get("syncEnabled"))
            && value_bool(state["bangumi"].get("pullCollections"))
    })
}

/// Merge the durable native snapshot, not just its one-shot notification queue.
pub(super) fn merge_snapshot(state: &mut Value, status: &Value, now: i64) -> anyhow::Result<usize> {
    let known: HashSet<String> = state["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|task| value_string(task.get("id")))
        .collect();
    if let Some(document) = status.get("document").filter(|value| value.is_object()) {
        let mut document = document.clone();
        // Upgrade the old Android projection without discarding metadata that
        // is still present in the Rust state.
        for entry in document["following"].as_array_mut().into_iter().flatten() {
            if !entry.get("title").is_some_and(Value::is_object) {
                let id = value_i64(entry.get("id"));
                if let Some(local) = state["following"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|item| value_i64(item.get("id")) == id)
                {
                    let incoming = entry.as_object().cloned().unwrap_or_default();
                    *entry = local.clone();
                    // rc.4 问题 1 加固：损坏状态记录不得 panic 整个启动桥接。
                    if let Some(object) = entry.as_object_mut() {
                        object.extend(incoming);
                    }
                }
            }
        }
        for task in document["tasks"].as_array_mut().into_iter().flatten() {
            if value_i64(task.get("syncUpdatedAt")) <= 0 {
                if let Some(local) = state["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|item| item.get("id") == task.get("id"))
                {
                    *task = local.clone();
                } else if value_i64(task.get("createdAt")) > 0 {
                    task["syncUpdatedAt"] = json!(value_i64(task.get("createdAt")) * 1000);
                }
            }
        }
        merge_document_into_state(state, &document)?;
    }
    for native in status["following"].as_array().into_iter().flatten() {
        let id = value_i64(native.get("id"));
        let revision = value_i64(native.get("scheduleUpdatedAt"));
        let Some(entry) = state["following"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .find(|item| value_i64(item.get("id")) == id)
        else {
            continue;
        };
        if revision <= 0 || revision < value_i64(entry.get("scheduleUpdatedAt")) {
            continue;
        }
        let episode = value_i64(native.get("nextEpisode"));
        let airing_at = value_i64(native.get("nextAiringAt"));
        entry["nextAiringEpisode"] = if episode > 0 && airing_at > 0 {
            json!({
                "episode": episode, "airingAt": airing_at,
                "episodeId": native["nextEpisodeId"],
                "airingPrecision": native["nextAiringPrecision"],
            })
        } else {
            Value::Null
        };
        entry["scheduleUpdatedAt"] = json!(revision);
        // Only an explicit per-episode future fact may retract a pending task.
        // A stale `next` or a date with no time is not evidence that it is unaired.
        if let Some(schedule) = native["episodeSchedule"].as_array() {
            let Some(tasks) = state["tasks"].as_array_mut() else { continue };
            tasks.retain_mut(|task| {
                if value_i64(task.get("subjectId")) != id
                    && value_i64(task.get("animeId")) != id
                {
                    return true;
                }
                let number = value_i64(task.get("episode"));
                if task["status"] == "completed" {
                    if let Some(row) = schedule.iter().find(|row| value_i64(row.get("episode")) == number) {
                        if value_i64(row.get("episodeId")) > 0 && value_i64(row.get("airingAt")) > 0 {
                            task["episodeId"] = row["episodeId"].clone();
                            task["airingAt"] = row["airingAt"].clone();
                            task["airingPrecision"] = row["airingPrecision"].clone();
                            task["airingSource"] = json!("bangumi_episode");
                            super::watch_history::review_future_completion(task, now);
                        }
                    }
                    return true;
                }
                if super::watch_history::is_reset(task) {
                    return true;
                }
                !schedule.iter().any(|row| {
                    let at = value_i64(row.get("airingAt"));
                    value_i64(row.get("episode")) == number
                        && at > 0
                        && if value_string(row.get("airingPrecision")) == "instant" {
                            at > now
                        } else {
                            at / 86_400 > now / 86_400
                        }
                })
            });
        }
    }
    if value_i64(status.get("syncedAt")) > value_i64(state.get("lastSyncAt")) {
        state["lastSyncAt"] = status["syncedAt"].clone();
    }
    #[cfg(feature = "standard")]
    super::heal_following_progress(state);
    Ok(state["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| {
            super::watch_history::is_pending(task, now)
                && !known.contains(&value_string(task.get("id")))
        })
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_state;

    fn sample() -> Value {
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 633836, "source": "bangumi", "anilistId": 189046,
            "title": {"english": "Split season"}, "displayTitle": "Split season",
            "episodes": 8, "watchedEpisode": 4, "followedAt": 1, "syncUpdatedAt": 1000,
            "nextAiringEpisode": {"episode": 6, "airingAt": 200000,
                "airingPrecision": "instant"}, "scheduleUpdatedAt": 3000
        }]);
        state["tasks"] = json!([{
            "id": "633836-5", "animeId": 633836, "subjectId": 633836,
            "episode": 5, "episodeId": 1656862, "airingAt": 100,
            "status": "completed", "createdAt": 110, "completedAt": 120,
            "syncUpdatedAt": 4000, "lastChangedBy": "local"
        }]);
        state
    }

    #[test]
    fn configuration_preserves_full_history_and_only_sends_device_configuration() {
        let mut state = sample();
        state["bangumi"]["token"] = json!("must-not-cross-bridge");
        let payload = configuration_payload(&state, false);
        assert_eq!(payload["pendingTasks"], state["tasks"]);
        assert_eq!(
            payload["following"][0]["title"],
            state["following"][0]["title"]
        );
        assert_eq!(payload["following"][0]["watchedEpisode"], 4);
        assert!(!payload.to_string().contains("must-not-cross-bridge"));
        assert_eq!(
            configuration_payload(&state, true)["pullCollections"],
            false
        );
    }

    #[test]
    fn snapshot_recovers_background_tasks_without_events_and_preserves_completion() {
        let mut state = sample();
        let mut native = state.clone();
        native["tasks"][0]["status"] = json!("pending");
        native["tasks"][0]["syncUpdatedAt"] = json!(2000);
        native["tasks"].as_array_mut().unwrap().push(json!({
            "id": "633836-6", "animeId": 633836, "subjectId": 633836,
            "episode": 6, "episodeId": 1656863, "airingAt": 200,
            "status": "pending", "createdAt": 201, "syncUpdatedAt": 5000
        }));
        let document = crate::document_from_state(&mut native);
        let status = json!({"document": document, "following": [], "events": []});
        assert_eq!(merge_snapshot(&mut state, &status, 300).unwrap(), 1);
        assert!(
            state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| task["id"] == "633836-5"
                    && task["status"] == "completed"
                    && task["completedAt"] == 120)
        );
        assert_eq!(merge_snapshot(&mut state, &status, 300).unwrap(), 0);
    }

    #[test]
    fn stale_or_date_only_next_does_not_delete_an_aired_episode() {
        let mut state = sample();
        state["tasks"][0]["status"] = json!("pending");
        let status = json!({"following": [{
            "id": 633836, "scheduleUpdatedAt": 4000, "nextEpisode": 5,
            "nextAiringAt": 86400, "nextAiringPrecision": "date",
            "episodeSchedule": [{"episode": 5, "airingAt": 86400, "airingPrecision": "date"}]
        }]});
        merge_snapshot(&mut state, &status, 90000).unwrap();
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
        let stale = json!({"following": [{
            "id": 633836, "scheduleUpdatedAt": 2000, "nextEpisode": 0, "nextAiringAt": 0
        }]});
        merge_snapshot(&mut state, &stale, 90000).unwrap();
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 5);
    }

    #[test]
    fn legacy_projection_without_timestamps_cannot_undo_a_completed_task() {
        let mut state = sample();
        let status = json!({"document": {
            "version": 1, "following": state["following"], "followingDeletedAt": {},
            "tasks": [{"id": "633836-5", "animeId": 633836, "episode": 5, "status": "pending"}]
        }});
        merge_snapshot(&mut state, &status, 90000).unwrap();
        assert_eq!(state["tasks"][0]["status"], "completed");
        assert_eq!(state["tasks"][0]["completedAt"], 120);
        assert_eq!(state["tasks"][0]["syncUpdatedAt"], 4000);
    }

    #[test]
    fn regenerated_pending_cannot_undo_completion_but_explicit_undo_can() {
        let mut state = sample();
        let mut generated = state["tasks"][0].clone();
        generated["status"] = json!("pending");
        generated["statusSource"] = json!("airing");
        generated["syncUpdatedAt"] = json!(9000);
        let status = json!({"document": {
            "version": 1, "following": state["following"], "tasks": [generated],
            "followingDeletedAt": {}
        }});
        merge_snapshot(&mut state, &status, 100).unwrap();
        assert_eq!(state["tasks"][0]["status"], "completed");
        let mut undo = status;
        undo["document"]["tasks"][0]["statusSource"] = json!("local");
        merge_snapshot(&mut state, &undo, 100).unwrap();
        assert_eq!(state["tasks"][0]["status"], "pending");
    }
}
