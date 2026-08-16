//! The context actions menu and its effects.
//!
//! The old `api/actions.rs` talked to api.spotify.com for every write (like,
//! follow, save, playlist-adds, server queue). All of those are local now:
//! the menu is built from the [`Store`], and activating an entry mutates the
//! store or the transport queue in place. Nothing here touches the network.

use crate::app::*;
use tuna_tui::util::{uri_parts, uri_to_url};

/// Build the context menu for `item`, reading saved/followed state from the
/// local store. Instant — no spawn, no enrichment round-trip.
pub(crate) fn build_action_menu(store: &Store, item: &LibItem) -> ActionMenu {
    let (scheme, kind, _id) = match uri_parts(&item.uri) {
        Some(p) => p,
        None => return ActionMenu::empty(),
    };
    let uri = item.uri.clone();
    let mut items = Vec::new();

    // Synthetic action rows (`tuna:action:*`), local rows whose kind is unknown
    // to the expander, and malformed uris get the generic arm: open + copy.
    if scheme != "yt" {
        items.push(ActionItem {
            label: "→  Open".into(),
            kind: ActionKind::Open {
                uri: uri.clone(),
                name: item.name.clone(),
            },
        });
        let linked = uri_to_url(&uri);
        if !linked.is_empty() {
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        return ActionMenu {
            title: item.name.clone(),
            items,
            selected: 0,
        };
    }

    match kind {
        "video" => {
            let saved = store.contains(StoreKind::Liked, &uri);
            items.push(ActionItem {
                label: if saved {
                    "♥  Remove from Liked".into()
                } else {
                    "♡  Add to Liked".into()
                },
                kind: ActionKind::ToggleLike {
                    uri: uri.clone(),
                    name: item.name.clone(),
                    subtitle: item.subtitle.clone(),
                },
            });
            items.push(ActionItem {
                label: "＋  Add to Queue".into(),
                kind: ActionKind::Queue { uri: uri.clone() },
            });
            items.push(ActionItem {
                label: "≡  Add to Playlist…".into(),
                kind: ActionKind::AddToPlaylistMenu {
                    track_uri: uri.clone(),
                    track_name: item.name.clone(),
                    track_subtitle: item.subtitle.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "channel" => {
            let following = store.contains(StoreKind::Artist, &uri);
            items.push(ActionItem {
                label: if following {
                    "Unfollow".into()
                } else {
                    "Follow".into()
                },
                kind: ActionKind::ToggleFollowArtist {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "album" => {
            let saved = store.contains(StoreKind::Album, &uri);
            items.push(ActionItem {
                label: if saved {
                    "Remove from Library".into()
                } else {
                    "Save Album".into()
                },
                kind: ActionKind::ToggleSaveAlbum {
                    uri: uri.clone(),
                    name: item.name.clone(),
                    subtitle: item.subtitle.clone(),
                },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open Album".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "playlist" => {
            let saved = store.contains(StoreKind::Playlist, &uri);
            items.push(ActionItem {
                label: if saved {
                    "Remove from Library".into()
                } else {
                    "＋  Add to Your Library".into()
                },
                kind: ActionKind::FollowPlaylist {
                    uri: uri.clone(),
                    name: item.name.clone(),
                    subtitle: item.subtitle.clone(),
                },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        _ => {
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            let linked = uri_to_url(&uri);
            if !linked.is_empty() {
                items.push(ActionItem {
                    label: "⧉  Copy Link".into(),
                    kind: ActionKind::CopyLink { uri },
                });
            }
        }
    }
    ActionMenu {
        title: item.name.clone(),
        items,
        selected: 0,
    }
}

/// Activate one menu entry against the app. Returns the status-line message.
/// All effects are local — there is no "spawn and await a server".
pub(crate) fn run_action(app: &mut App, kind: ActionKind) -> String {
    match kind {
        ActionKind::ToggleLike {
            uri,
            name,
            subtitle,
        } => {
            if app.store.toggle(StoreKind::Liked, name, subtitle, uri) {
                "added to Liked \u{2665} (press r to refresh)".into()
            } else {
                "removed from Liked".into()
            }
        }
        ActionKind::Queue { uri } => {
            // The local queue: the engine's list is the authority, and the
            // transport mirrors it for the view. Dedupe so repeat presses
            // don't stack rows; enqueuing while nothing is loaded is a no-op
            // on the engine side (mirrored by the empty transport queue).
            if !app.transport.queue_uris.contains(&uri)
                && app.svc.engine.enqueue(vec![uri.clone()]).is_ok()
            {
                app.transport.queue_uris.push(uri.clone());
                app.transport.queue.push(app.track_label_of(&uri));
            }
            "added to queue".into()
        }
        ActionKind::AddToPlaylist {
            playlist_uri,
            track,
        } => {
            let name = app
                .store
                .playlists
                .iter()
                .find(|p| p.uri == playlist_uri)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            match app
                .store
                .add_to_playlist(&playlist_uri, name.clone(), track)
            {
                Some(msg) => msg,
                None => "playlist gone — press r to reload the library".into(),
            }
        }
        ActionKind::ToggleFollowArtist { uri, name } => {
            if app
                .store
                .toggle(StoreKind::Artist, name, String::new(), uri)
            {
                "following".into()
            } else {
                "unfollowed".into()
            }
        }
        ActionKind::ToggleSaveAlbum {
            uri,
            name,
            subtitle,
        } => {
            if app.store.toggle(StoreKind::Album, name, subtitle, uri) {
                "saved album".into()
            } else {
                "removed album".into()
            }
        }
        ActionKind::FollowPlaylist {
            uri,
            name,
            subtitle,
        } => {
            if app.store.toggle(StoreKind::Playlist, name, subtitle, uri) {
                "added to library".into()
            } else {
                "removed from library".into()
            }
        }
        _ => String::new(),
    }
}
