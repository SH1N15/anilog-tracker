use serde_json::{json, Value};
#[cfg(feature = "standard")]
use std::collections::HashSet;

use super::value_i64;

pub(super) fn needs_review(task: &Value) -> bool {
    task["status"] == "completed" && task["completionReview"]["decision"] == "review"
}

#[cfg(any(feature = "standard", test))]
pub(super) fn is_completed(task: &Value) -> bool {
    task["status"] == "completed" && !needs_review(task)
}

pub(super) fn is_reset(task: &Value) -> bool {
    task["status"] == "pending" && task["completionReview"]["decision"] == "reset"
}

pub(super) fn is_future(task: &Value, now: i64) -> bool {
    let at = value_i64(task.get("airingAt"));
    at > 0
        && match task["airingPrecision"].as_str() {
            Some("instant") => at > now,
            Some("date") => at / 86400 > now / 86400,
            _ => false,
        }
}

pub(super) fn is_pending(task: &Value, now: i64) -> bool {
    task["status"] == "pending" && !(is_reset(task) && is_future(task, now))
}

#[cfg(any(feature = "standard", target_os = "android", test))]
pub(super) fn review_future_completion(task: &mut Value, now: i64) -> bool {
    if task["status"] != "completed"
        || task["completionReview"]["decision"] == "keep"
        || needs_review(task)
        || task["airingSource"] != "bangumi_episode"
        || value_i64(task.get("episodeId")) <= 0
        || !is_future(task, now)
    {
        return false;
    }
    let revision = now
        .saturating_mul(1000)
        .max(value_i64(task.get("syncUpdatedAt")) + 1);
    // Keep the completion and its provenance intact. Time alone is not
    // permission to erase a viewing record or change the user's account.
    task["completionReview"] = json!({
        "decision": "review", "reason": "before_airing", "detectedAt": revision,
        "airingAt": task["airingAt"], "airingPrecision": task["airingPrecision"],
        "originalCompletedAt": task.get("completedAt").cloned().unwrap_or(Value::Null),
        "originalCreatedAt": task.get("createdAt").cloned().unwrap_or(Value::Null),
        "originalSyncUpdatedAt": task.get("syncUpdatedAt").cloned().unwrap_or(Value::Null),
        "originalLastPushedToBangumiAt": task.get("lastPushedToBangumiAt").cloned().unwrap_or(Value::Null),
    });
    task["syncUpdatedAt"] = json!(revision);
    true
}

#[cfg(feature = "standard")]
pub(super) fn completed_count(state: &Value, entry: &Value, include_review: bool) -> i64 {
    let subject = value_i64(entry.get("id"));
    let total = value_i64(entry.get("episodes"));
    state["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| {
            let episode = value_i64(task.get("episode"));
            (value_i64(task.get("subjectId")) == subject
                || value_i64(task.get("animeId")) == subject)
                && (task["status"] == "completed" || (include_review && is_reset(task)))
                && (include_review || !needs_review(task))
                && episode > 0
                && (total <= 0 || episode <= total)
        })
        .map(|task| value_i64(task.get("episode")))
        .collect::<HashSet<_>>()
        .len() as i64
}

#[cfg(feature = "standard")]
pub(super) fn reconcile(state: &mut Value, now: i64) -> bool {
    let mut changed = false;
    for task in state["tasks"].as_array_mut().into_iter().flatten() {
        changed |= review_future_completion(task, now);
    }
    let counts: Vec<_> = state["following"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, entry)| entry["source"] == "bangumi" && entry["watchedEpisode"].is_number())
        .map(|(index, entry)| {
            (
                index,
                completed_count(state, entry, false),
                completed_count(state, entry, true),
            )
        })
        .collect();
    for (index, count, with_review) in counts {
        let entry = &mut state["following"][index];
        let previous = value_i64(entry.get("watchedEpisode"));
        // Only lower a locally derivable total. A partial history cannot
        // establish that a larger remote aggregate is wrong.
        if count > previous || (count < with_review && previous == with_review) {
            entry["watchedEpisode"] = json!(count);
            entry["syncUpdatedAt"] = json!(now
                .saturating_mul(1000)
                .max(value_i64(entry.get("syncUpdatedAt")) + 1));
            changed = true;
        }
    }
    changed
}

pub(super) fn resolve(task: &mut Value, keep_completed: bool, now: i64) -> Result<(), String> {
    if !needs_review(task) {
        return Err("这条记录已更新，请刷新后重试".into());
    }
    let revision = now
        .saturating_mul(1000)
        .max(value_i64(task.get("syncUpdatedAt")) + 1);
    task["completionReview"]["decision"] = json!(if keep_completed { "keep" } else { "reset" });
    task["completionReview"]["resolvedAt"] = json!(revision);
    task["status"] = json!(if keep_completed {
        "completed"
    } else {
        "pending"
    });
    if !keep_completed {
        task["completedAt"] = Value::Null;
    }
    task["statusSource"] = json!("local");
    #[cfg(feature = "standard")]
    if value_i64(task.get("subjectId")) > 0 {
        task["lastChangedBy"] = json!("local");
    }
    task["syncUpdatedAt"] = json!(revision);
    task.as_object_mut()
        .unwrap()
        .remove("lastPushedToBangumiAt");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future_task() -> Value {
        json!({
            "id": "638497-13", "animeId": 638497, "subjectId": 638497,
            "episode": 13, "episodeId": 1717851, "status": "completed",
            "airingAt": 200000, "airingPrecision": "instant", "airingSource": "bangumi_episode",
            "completedAt": 100, "createdAt": 80, "syncUpdatedAt": 1000
        })
    }

    #[test]
    fn completion_is_retained_and_review_does_not_expire_at_airtime() {
        let mut task = future_task();
        assert!(review_future_completion(&mut task, 200));
        assert!(needs_review(&task));
        assert_eq!(task["completedAt"], 100);
        assert_eq!(task["createdAt"], 80);
        assert_eq!(task["completionReview"]["originalSyncUpdatedAt"], 1000);
        let before = task.clone();
        assert!(!review_future_completion(&mut task, 300000));
        assert_eq!(task, before);
        assert!(!is_completed(&task));
    }

    #[test]
    fn same_day_date_and_unverified_times_do_not_quarantine_history() {
        let mut task = future_task();
        task["airingPrecision"] = json!("date");
        task["airingAt"] = json!(86400);
        assert!(!review_future_completion(&mut task, 86401));
        task["airingAt"] = json!(3 * 86400);
        assert!(review_future_completion(&mut task, 86401));
        let mut unknown = future_task();
        unknown["airingSource"] = json!("offline");
        assert!(!review_future_completion(&mut unknown, 200));
    }

    #[test]
    fn explicit_reset_waits_for_airing_and_retains_the_original_completion() {
        let mut task = future_task();
        review_future_completion(&mut task, 200);
        resolve(&mut task, false, 201).unwrap();
        assert!(is_reset(&task));
        assert!(!is_pending(&task, 300));
        assert!(is_pending(&task, 200000));
        assert_eq!(task["completionReview"]["originalCompletedAt"], 100);
        assert!(task["completedAt"].is_null());
        assert!(
            resolve(&mut task, true, 202).is_err(),
            "stale double actions cannot flip a resolution"
        );
    }

    #[test]
    fn explicit_confirmation_is_not_quarantined_again() {
        let mut task = future_task();
        review_future_completion(&mut task, 200);
        resolve(&mut task, true, 201).unwrap();
        assert!(is_completed(&task));
        assert!(!review_future_completion(&mut task, 202));
    }
}
