//! Input while the actions menu overlay is open, plus the clipboard escape
//! hatch its "copy link" entries use.

use crossterm::event::KeyCode;

use crate::actions;
use crate::app::*;
use crate::browse;
use tuna_tui::util::uri_to_url;

/// Handle input while the actions menu is open.
pub(crate) fn handle_action_key(
    app: &mut App,
    code: KeyCode,
    chans: &crate::UiChannels,
) {
    match code {
        KeyCode::Esc | KeyCode::Char('a') => {
            app.view.actions = None;
            app.status.clear();
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(m) = app.view.actions.as_mut() {
                m.selected = m.selected.saturating_sub(1);
            }
            return;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(m) = app.view.actions.as_mut() {
                m.selected = (m.selected + 1).min(m.items.len().saturating_sub(1));
            }
            return;
        }
        KeyCode::Enter => {}
        _ => return,
    }

    // Enter: act on the selected entry.
    let kind = app
        .view
        .actions
        .as_ref()
        .and_then(|m| m.items.get(m.selected))
        .map(|i| i.kind.clone());
    let Some(kind) = kind else { return };
    match kind {
        ActionKind::AddToPlaylistMenu {
            track_uri,
            track_name,
            track_subtitle,
        } => {
            // The viable targets are the saved playlists (drill-in rows), local
            // or external — all of them can accept local adds.
            let items: Vec<ActionItem> = app
                .browse
                .library
                .playlists
                .iter()
                .filter_map(|p| {
                    if p.uri.is_empty() {
                        return None;
                    }
                    Some(ActionItem {
                        label: p.name.clone(),
                        kind: ActionKind::AddToPlaylist {
                            playlist_uri: p.uri.clone(),
                            track: LibEntry {
                                name: track_name.clone(),
                                subtitle: track_subtitle.clone(),
                                uri: track_uri.clone(),
                            },
                        },
                    })
                })
                .collect();
            if items.is_empty() {
                app.status = "no playlists to add to".to_string();
                app.view.actions = None;
            } else {
                app.view.actions = Some(ActionMenu {
                    title: "Add to playlist".to_string(),
                    items,
                    selected: 0,
                });
            }
        }
        ActionKind::Play { uri, name } => {
            // Previously called engine.play_context directly, leaving
            // source/source_name stale: PLAYING FROM showed the wrong context
            // and resume-on-launch replayed the previous one.
            let shuffle = app.transport.shuffle;
            app.play_context_row(uri, name, shuffle);
            app.view.actions = None;
        }
        ActionKind::StartRadio { uri, name } => {
            if app.session.radio_in_flight {
                app.status = "radio already starting…".to_string();
            } else {
                app.session.radio_in_flight = true;
                app.status = format!("starting radio for {name}…");
                app.transport.source = PlaySource::Radio(uri.clone());
                app.transport.source_name = format!("Radio · {name}");
                crate::spawn_radio(app.svc.engine.clone(), uri, 0, chans.radio.clone());
            }
            app.view.actions = None;
        }
        ActionKind::Open { uri, name } => {
            browse::spawn_detail_fetch(app.store.clone(), uri, name, chans.detail.clone());
            app.view.actions = None;
        }
        ActionKind::CopyLink { uri } => {
            app.status = if copy_to_clipboard(&uri_to_url(&uri)) {
                "link copied".to_string()
            } else {
                "clipboard unavailable".to_string()
            };
            app.view.actions = None;
        }
        other => {
            // Every write is local now: mutate the store / transport and
            // report the status line directly.
            app.status = actions::run_action(app, other);
            app.view.actions = None;
        }
    }
}

/// Copy text to the system clipboard via whatever tool is available.
pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-b", "-i"]),
        ("pbcopy", &[]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut sin) = child.stdin.take() {
                let _ = sin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return true;
        }
    }
    false
}
