//! F22 — the display cache is bounded by the engine queue, not by age:
//! the 24s sync tick retains only entries whose uri is still in the queue.

use std::collections::{HashMap, HashSet};

/// The retain predicate the sync tick applies: keep labels for queued uris.
fn retain_for_queue(cache: &mut HashMap<String, (String, String)>, queue_uris: &[String]) {
    let keep: HashSet<&String> = queue_uris.iter().collect();
    cache.retain(|uri, _| keep.contains(uri));
}

#[test]
fn meta_cache_is_bounded_by_the_queue() {
    let mut cache: HashMap<String, (String, String)> = HashMap::new();
    for i in 0..510u32 {
        cache.insert(
            format!("yt:video:cap-{i}"),
            ("title".to_string(), String::new()),
        );
    }

    // The queue holds only three tracks: everything else must be dropped.
    let queue = vec!["yt:video:cap-1".to_string(), "yt:video:cap-9".to_string()];
    retain_for_queue(&mut cache, &queue);

    assert_eq!(cache.len(), 2);
    assert!(cache.contains_key("yt:video:cap-1"));
    assert!(cache.contains_key("yt:video:cap-9"));
    assert!(
        !cache.contains_key("yt:video:cap-0"),
        "labels for tracks that left the queue are dropped"
    );
}

#[test]
fn meta_cache_empty_queue_drops_everything() {
    let mut cache: HashMap<String, (String, String)> = HashMap::new();
    cache.insert(
        "yt:video:gone".to_string(),
        ("title".to_string(), String::new()),
    );
    retain_for_queue(&mut cache, &[]);
    assert!(cache.is_empty(), "no queue, no labels");
}

#[test]
fn meta_cache_keeps_labels_when_the_queue_keeps_tracks() {
    let mut cache: HashMap<String, (String, String)> = HashMap::new();
    cache.insert(
        "yt:video:stay".to_string(),
        ("title".to_string(), String::new()),
    );
    let queue = vec!["yt:video:stay".to_string()];
    retain_for_queue(&mut cache, &queue);
    assert_eq!(cache.len(), 1, "queued tracks keep their labels");
}
