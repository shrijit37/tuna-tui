//! The 24s sync tick's refresh gate (`refresh_needed`): when to re-format
//! the Queue view's labels. A length change in either the engine queue or
//! the metadata cache is exactly when the labels can have gone stale.

use crate::refresh_needed;

#[test]
fn first_tick_always_refreshes() {
    // The usize::MAX sentinel: the first tick after launch/resume refreshes
    // even when the queue and cache are already in steady state (the
    // resume-restore path — lengths match, but labels may be raw URIs).
    assert!(refresh_needed(5, 3, usize::MAX, usize::MAX));
    assert!(refresh_needed(0, 0, usize::MAX, usize::MAX));
}

#[test]
fn unchanged_lengths_skip() {
    assert!(!refresh_needed(5, 3, 5, 3));
    assert!(!refresh_needed(0, 0, 0, 0));
}

#[test]
fn queue_len_change_refreshes() {
    // Recovery-removal shrinks the engine snapshot; enqueue grows it.
    assert!(refresh_needed(4, 3, 5, 3));
    assert!(refresh_needed(6, 3, 5, 3));
}

#[test]
fn meta_len_change_refreshes() {
    // Every EngineMeta landing upgrades a raw-URI queue row to a title label.
    assert!(refresh_needed(5, 4, 5, 3));
    assert!(refresh_needed(5, 2, 5, 3));
}

#[test]
fn either_change_together_refreshes() {
    assert!(refresh_needed(1, 1, 0, 0));
}
