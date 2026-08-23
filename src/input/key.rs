//! Terminal key input — the main keymap.

use crate::*;

/// Returns true if the app should quit.
pub(crate) fn handle_key(
    app: &mut App,
    code: KeyCode,
    mods: KeyModifiers,
    chans: &UiChannels,
) -> bool {
    // --- Actions menu captures input while open ---
    // Double-press Ctrl-C to quit (works from anywhere). Single press arms it.
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        let now = Instant::now();
        if app
            .session
            .last_ctrl_c
            .map(|t| now.duration_since(t) < Duration::from_millis(1500))
            .unwrap_or(false)
        {
            return true;
        }
        app.session.last_ctrl_c = Some(now);
        app.status = "press Ctrl-C again to quit".to_string();
        return false;
    }

    if app.view.actions.is_some() {
        handle_action_key(app, code, chans);
        return false;
    }

    // --- Search input mode captures everything ---
    if app.search.input_mode {
        match code {
            KeyCode::Esc => {
                app.search.input_mode = false;
                app.search.search_results.clear(); // S29-1: Esc drops stale rows
            },
            KeyCode::Enter => {
                app.search.input_mode = false;
                let q = app.search.query().trim().to_string();
                if !q.is_empty() {
                    // Fresh submit: drop stale suggestions so "searching…"
                    // renders instead of a lingering completion list (a4e.12).
                    app.search.search_results.clear();
                    app.search.searching = true;
                    app.search.in_flight = true;
                    app.browse.selected = 0;
                    app.status = "searching…".to_string();
                    spawn_search(q, chans.search.clone());
                }
            }
            // Ctrl-U clears the query — readline muscle memory. The fork
            // binds Ctrl-U to undo; shadow it (same call as agent-runtime).
            KeyCode::Char('u') if mods.contains(crossterm::event::KeyModifiers::CONTROL) => {
                app.search.clear();
            }
            // Everything else — typing, cursor movement, word ops — is the
            // editor's business. Enter is intercepted above, so no newlines.
            _ => {
                app.search
                    .input
                    .input(crossterm::event::KeyEvent::new(code, mods));
                // Type-ahead ping (a4e.12): non-blocking — the suggest worker
                // debounces and the result supersedes nothing the user did.
                let _ = chans.suggest.try_send(app.search.query().to_string());
            }
        }
        return false;
    }

    // Zen hides the library, so the keys that drive one do nothing rather than
    // moving a selection nobody can see. Placed after the overlays above, which
    // stay usable if one was already open when zen came on.
    if app.view.zen && drives_library(code) {
        return false;
    }

    match code {
        KeyCode::Char('/') => {
            app.search.input_mode = true;
            app.search.clear();
            app.search.search_results.clear(); // fresh search, no stale rows
        }
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            if let Some(d) = app.browse.details.pop() {
                app.browse.selected = d.parent_selected;
            } else {
                if app.search.searching {
                    app.search.searching = false;
                    app.browse.selected = 0;
                }
                // S29-1: Esc fully out of a search drops its stale rows.
                app.search.search_results.clear();
            }
            // Nothing to back out of — Esc no longer quits (use q or Ctrl-C twice).
        }
        KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Media(MediaKeyCode::PlayPause) => {
            if app.transport.playback_started {
                let _ = app.svc.engine.toggle();
            } else {
                // Resume the persisted source (context/radio/liked).
                resume_source(app, &chans.radio);
                app.transport.playback_started = true;
            }
        }
        KeyCode::Media(MediaKeyCode::Stop) => {
            app.svc.engine.stop();
        }
        KeyCode::Char('n') | KeyCode::Media(MediaKeyCode::TrackNext) => {
            let _ = app.svc.engine.next();
        }
        KeyCode::Char('b') | KeyCode::Media(MediaKeyCode::TrackPrevious) => {
            let _ = app.svc.engine.prev();
        }
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Media(MediaKeyCode::RaiseVolume) => {
            app.transport.volume = (app.transport.volume + 5).min(100);
            let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
        }
        KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Media(MediaKeyCode::LowerVolume) => {
            app.transport.volume = app.transport.volume.saturating_sub(5);
            let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
        }
        KeyCode::Char('s') => {
            app.transport.shuffle = !app.transport.shuffle;
            let _ = app.svc.engine.shuffle(app.transport.shuffle);
        }
        // Play the highlighted playlist / album / artist outright. Enter still
        // opens; this is the direct route that used to require two Enters or
        // the actions menu.
        KeyCode::Char('P') => play_selected_context(app, false),
        KeyCode::Char('S') => {
            // Flip the global toggle too, or the footer would show shuffle off
            // while playback is shuffled, and `resume_source` would later
            // replay this context unshuffled.
            app.transport.shuffle = true;
            let _ = app.svc.engine.shuffle(true);
            play_selected_context(app, true);
        }
        KeyCode::Char('R') => {
            app.transport.repeat = !app.transport.repeat;
            let _ = app.svc.engine.repeat(app.transport.repeat);
        }
        KeyCode::Char('r') => {
            app.status = "loading library…".to_string();
            app.browse.library.reset_loading();
            spawn_library_fetch(app.store.clone(), chans.lib.clone());
        }
        KeyCode::Char('o') => {
            app.browse.sort = app.browse.sort.next();
            let m = app.browse.sort;
            sort_list(app.cur_list_mut(), m);
            app.browse.selected = app.first_selectable();
            app.status = format!("sorted by {}", m.label());
        }
        KeyCode::Char('a') => {
            // Zen hides the library, so the menu belongs to what is playing —
            // acting on a selection nobody can see is how it ends up offering
            // "remove from Liked" for the wrong track.
            let item = if app.view.zen {
                app.playback
                    .now
                    .as_ref()
                    .filter(|n| !n.uri.is_empty())
                    .map(|n| LibItem::track(n.title.clone(), n.artist.clone(), n.uri.clone()))
            } else {
                app.cur_items().get(app.browse.selected).cloned()
            };
            if let Some(item) = item {
                if !item.is_header && !item.is_play {
                    // Instant, fully local: the menu reads saved state from the
                    // store, so there is nothing to enrich later.
                    app.view.actions = Some(build_action_menu(&app.store, &item));
                }
            }
        }
        // Tab / Shift+Tab (and [ ]) rotate the library sections.
        KeyCode::Tab | KeyCode::Char(']') => {
            app.search.searching = false;
            app.browse.section = app.browse.section.shift(1);
            app.browse.selected = app.first_selectable();
        }
        KeyCode::BackTab | KeyCode::Char('[') => {
            app.search.searching = false;
            app.browse.section = app.browse.section.shift(-1);
            app.browse.selected = app.first_selectable();
        }
        // Arrow keys rotate the right-pane view; Shift+arrows seek ±5s.
        KeyCode::Right if mods.contains(KeyModifiers::SHIFT) => {
            app.playback.seek_step(SEEK_STEP_MS)
        }
        KeyCode::Left if mods.contains(KeyModifiers::SHIFT) => {
            app.playback.seek_step(-SEEK_STEP_MS)
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.view.mode = app.view.mode.shift(1);
            if app.view.mode == RightView::Queue && app.transport.playback_started {
                app.refresh_local_queue();
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            app.view.mode = app.view.mode.shift(-1);
            if app.view.mode == RightView::Queue && app.transport.playback_started {
                app.refresh_local_queue();
            }
        }
        KeyCode::Char('Q') => {
            if app.view.mode == RightView::Queue {
                app.view.mode = RightView::NowPlaying;
            } else {
                app.view.mode = RightView::Queue;
                if app.transport.playback_started {
                    app.refresh_local_queue();
                }
            }
        }
        // The frame loop notices the layout change and wipes the art box.
        KeyCode::Char('z') => app.view.zen = !app.view.zen,
        KeyCode::Down | KeyCode::Char('j') => {
            if app.view.mode == RightView::Queue && !app.transport.queue.is_empty() {
                app.view.queue_selected =
                    (app.view.queue_selected + 1).min(app.transport.queue.len().saturating_sub(1));
            } else {
                app.move_sel(1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.view.mode == RightView::Queue && !app.transport.queue.is_empty() {
                app.view.queue_selected = app.view.queue_selected.saturating_sub(1);
            } else {
                app.move_sel(-1);
            }
        }
        // Needs a terminal that reports modified Enter (kitty, WezTerm, foot).
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => play_selected_context(app, false),
        KeyCode::Enter => {
            if app.view.mode == RightView::Queue && !app.transport.queue_uris.is_empty() {
                let sel = app
                    .view
                    .queue_selected
                    .min(app.transport.queue_uris.len().saturating_sub(1));
                let target_uri = app.transport.queue_uris[sel].clone();
                let remaining = app.transport.queue_uris[sel..].to_vec();
                if let Err(e) = app.svc.engine.play_tracks(
                    remaining,
                    Some(target_uri),
                    0,
                    app.transport.shuffle,
                ) {
                    app.status = format!("couldn't play: {e:#}");
                }
                app.refresh_local_queue();
            } else {
                match app.activate() {
                    Activated::Open(uri, name) => {
                        spawn_detail_fetch(app.store.clone(), uri, name, chans.detail.clone());
                    }
                    Activated::Radio(uri) => {
                        if app.session.radio_in_flight {
                            app.status = "radio already starting…".to_string();
                            return false;
                        }
                        app.session.radio_in_flight = true;
                        app.status = "starting radio…".to_string();
                        crate::spawn_radio(app.svc.engine.clone(), uri, 0, chans.radio.clone());
                    }
                    Activated::None => {}
                }
            }
        }
        _ => {}
    }
    false
}

/// Keys whose whole effect is on the library pane.
///
/// Zen hides that pane, so these do nothing there rather than moving a selection
/// nobody can see — and `a` is deliberately absent, because it retargets onto
/// the playing track instead of going quiet.
pub(crate) fn drives_library(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('/' | '[' | ']' | 'j' | 'k' | 'o' | 'r' | 'P' | 'S')
    )
}
