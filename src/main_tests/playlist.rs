use crate::*;

fn ctx_row() -> LibItem {
    LibItem::ctx(
        "Chill Vibes".into(),
        "you · 142".into(),
        "yt:playlist:PLabc".into(),
    )
}

// ------------------------------------------------ optional integrations

#[test]
fn optional_integration_keeps_successful_service() {
    assert_eq!(
        optional_integration(true, || Ok::<_, ()>("media")),
        Some("media")
    );
}

#[test]
fn optional_integration_degrades_on_initialization_failure() {
    assert_eq!(
        optional_integration(true, || Err::<(), _>("no session bus")),
        None
    );
}

#[test]
fn optional_integration_skips_init_when_platform_is_unavailable() {
    let called = std::cell::Cell::new(false);
    let service = optional_integration(false, || {
        called.set(true);
        Ok::<_, ()>("media")
    });
    assert_eq!(service, None);
    assert!(!called.get());
}

#[test]
fn disconnected_media_channel_disables_future_receives() {
    let mut open = true;
    let event: Result<(), flume::RecvError> = Err(flume::RecvError::Disconnected);
    assert_eq!(consume_media_event(event, &mut open), None);
    assert!(!open);
}

#[test]
fn active_scrub_rejects_stale_engine_position() {
    assert!(!should_apply_engine_position(true, Some(42_000)));
    assert!(should_apply_engine_position(true, None));
    assert!(should_apply_engine_position(false, Some(42_000)));
}

#[test]
fn startup_restore_requires_the_setting_and_a_saved_track() {
    let mut saved = SavedState::default();
    assert!(!should_restore_saved_playback(true, None, &saved));

    saved.last_played = Some(LastPlayed::default());
    assert!(should_restore_saved_playback(true, None, &saved));
    assert!(!should_restore_saved_playback(false, None, &saved));
}

#[test]
fn explicit_startup_uri_wins_over_the_saved_track() {
    let saved = SavedState {
        last_played: Some(LastPlayed::default()),
        ..SavedState::default()
    };
    assert!(!should_restore_saved_playback(
        true,
        Some("yt:playlist:PLabc"),
        &saved,
    ));
}

// -------------------------------------------------------- context_target

#[test]
fn context_target_accepts_context_rows() {
    let (uri, name) = context_target(&ctx_row()).expect("playlist is a context");
    assert_eq!(uri, "yt:playlist:PLabc");
    assert_eq!(name, "Chill Vibes");
}

#[test]
fn context_target_accepts_synthesized_play_row() {
    // "▶︎ Play X" rows carry the context URI, so P works inside a drill-in.
    let row = LibItem::play("▶︎ Play Chill Vibes".into(), "yt:playlist:PLabc".into());
    assert_eq!(
        context_target(&row).map(|(u, _)| u),
        Some("yt:playlist:PLabc".to_string())
    );
}

#[test]
fn context_target_rejects_tracks_and_headers() {
    let track = LibItem::track("Song".into(), "Artist".into(), "yt:video:abc".into());
    assert!(context_target(&track).is_none());
    assert!(context_target(&LibItem::header("Songs")).is_none());
}

// -------------------------------------------------------- meta_is_current

#[test]
fn stale_metadata_replies_are_dropped() {
    let a = "yt:video:AAA";
    let b = "yt:video:BBB";
    // Waiting on B: B's reply applies, A's late reply does not.
    assert!(meta_is_current(Some(b), b));
    assert!(!meta_is_current(Some(b), a));
    // Nothing outstanding -> accept (the guard only drops provable mismatches).
    assert!(meta_is_current(None, a));
}

// ------------------------------------------------------------ enter_label

#[test]
fn enter_label_matches_context_target() {
    let track = LibItem::track("Song".into(), "Artist".into(), "yt:video:abc".into());
    assert_eq!(enter_label(Some(&ctx_row())), "open");
    assert_eq!(enter_label(Some(&track)), "select");
    assert_eq!(enter_label(Some(&LibItem::header("Songs"))), "select");
    assert_eq!(enter_label(None), "select");

    // The invariant the footer relies on: Enter says "open" for exactly
    // the rows P can play.
    for row in [ctx_row(), track, LibItem::header("Songs")] {
        let opens = enter_label(Some(&row)) == "open";
        assert_eq!(opens, context_target(&row).is_some() && !row.is_play);
    }
}
