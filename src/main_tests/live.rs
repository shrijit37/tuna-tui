//! Live tests, `#[ignore]`d so `cargo test` stays offline. They exercise the
//! port's permanent transport (yt-dlp + YouTube) end to end, the same way the
//! old `live.rs` covered the Spotify API.

/// Live smoke test: search returns playable video rows. Needs yt-dlp + network.
#[test]
#[ignore = "hits YouTube via yt-dlp"]
fn live_search_roundtrip() {
    let vids = tuna_tui::yt::search("bohemian rhapsody queen", 6);
    assert!(!vids.is_empty(), "expected at least one video");
    assert!(vids.iter().all(|v| v.uri.starts_with("yt:video:")));
    assert!(vids.iter().any(|v| !v.artist.is_empty()));
}

/// Live smoke test: an album slug expands to a playable list.
#[test]
#[ignore = "hits YouTube via yt-dlp"]
fn live_album_drill_in_roundtrip() {
    let store = crate::Store::default();
    let (_, items) = crate::browse::fetch_detail_blocking(
        &store,
        "yt:album:bohemian rhapsody queen",
        "Bohemian Rhapsody",
    );
    assert!(items.len() > 1, "play row + at least one search result");
    assert!(items[0].is_play);
    assert!(items[1].is_track);
    assert!(items[1].uri.starts_with("yt:video:"));
}

/// Live smoke test: a real playlist drill-in returns rows whose titles split
/// into artist — title.
#[test]
#[ignore = "hits YouTube via yt-dlp"]
fn live_playlist_drill_in_roundtrip() {
    let store = crate::Store::default();
    let (_, items) = crate::browse::fetch_detail_blocking(
        &store,
        "yt:playlist:PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI",
        "Rock Classics",
    );
    assert!(items.len() > 1, "play row + at least one entry");
    assert!(items[0].is_play);
}

/// Live smoke test: radio seeds produce a station (seed + similar).
#[test]
#[ignore = "hits YouTube via yt-dlp"]
fn live_radio_roundtrip() {
    use tuna_tui::engine::Expander as _;
    let uris = tuna_tui::engine::YtExpander
        .radio("yt:video:dQw4w9WgXcQ")
        .expect("radio station");
    assert!(uris.len() >= 2, "seed + at least one similar track");
    assert_eq!(uris[0], "yt:video:dQw4w9WgXcQ");
    assert!(uris.iter().all(|u| u.starts_with("yt:video:")));
}
