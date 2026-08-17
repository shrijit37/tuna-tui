//! F22 — the display cache's FIFO cap bounds a long session's memory.

// `mod event` / `mod state` are private; the public(crate) paths are the
// `app` module's glob re-exports.
use crate::app::meta_cache_register;
use crate::app::META_CACHE_CAP;
use std::collections::{HashMap, VecDeque};

#[test]
fn meta_cache_caps_at_500() {
    let mut cache: HashMap<String, (String, String)> = HashMap::new();
    let mut order: VecDeque<String> = VecDeque::new();

    // 510 distinct tracks: the cache must settle at the cap, and the deque
    // (the eviction authority) must track it exactly.
    for i in 0..510u32 {
        let uri = format!("yt:video:cap-{i}");
        meta_cache_register(&mut cache, &mut order, &uri);
        cache.insert(uri, ("title".to_string(), String::new()));
    }
    assert_eq!(cache.len(), META_CACHE_CAP);
    assert_eq!(order.len(), META_CACHE_CAP);
    assert!(
        !cache.contains_key("yt:video:cap-0"),
        "oldest entries must be evicted"
    );
    assert!(cache.contains_key("yt:video:cap-509"), "newest survive");

    // A re-delivered key is not a new key: length and eviction order stay
    // exactly as they were (no deque inflation, no spurious eviction).
    let before_len = cache.len();
    let before: Vec<String> = order.iter().cloned().collect();
    meta_cache_register(&mut cache, &mut order, "yt:video:cap-509");
    assert_eq!(cache.len(), before_len);
    let after: Vec<String> = order.iter().cloned().collect();
    assert_eq!(
        after, before,
        "re-insert must not reorder or inflate the deque"
    );
}
