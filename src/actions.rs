//! The context actions menu and its effects.
//!
//! The old `api/actions.rs` talked to api.spotify.com for every write (like,
//! follow, save, playlist-adds, server queue). All of those are local now:
//! the menu is built from the [`Store`], and activating an entry mutates the
//! store or the transport queue in place. Nothing here touches the network.

use crate::app::*;
use tuna_tui::util::{uri_parts, uri_to_url};

/// The shared context tail — Play, Open (with the arm's own label), Copy Link —
/// appended by the channel, album, and playlist arms. The video arm's
/// Queue/AddToPlaylist tail stays inline: a single helper can't produce both
/// orders.
fn push_context_tail(items: &mut Vec<ActionItem>, uri: &str, name: &str, open_label: &str) {
    items.push(ActionItem {
        label: "▶︎  Play".into(),
        kind: ActionKind::Play {
            uri: uri.into(),
            name: name.into(),
        },
    });
    items.push(ActionItem {
        label: open_label.into(),
        kind: ActionKind::Open {
            uri: uri.into(),
            name: name.into(),
        },
    });
    items.push(ActionItem {
        label: "⧉  Copy Link".into(),
        kind: ActionKind::CopyLink { uri: uri.into() },
    });
}

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
                label: "▶︎  Start Radio".into(),
                kind: ActionKind::StartRadio {
                    uri: uri.clone(),
                    name: item.name.clone(),
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
            if !item.subtitle.is_empty() {
                items.push(ActionItem {
                    label: format!("👤  Go to Artist ({})", item.subtitle),
                    kind: ActionKind::Open {
                        uri: format!("yt:artist:{}", item.subtitle),
                        name: item.subtitle.clone(),
                    },
                });
            }
            items.push(ActionItem {
                label: format!("💽  Go to Album ({})", item.name),
                kind: ActionKind::Open {
                    uri: format!(
                        "yt:album:{}",
                        if item.subtitle.is_empty() {
                            item.name.clone()
                        } else {
                            format!("{} {}", item.name, item.subtitle)
                        }
                    ),
                    name: format!("Album: {}", item.name),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "channel" | "artist" => {
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
            push_context_tail(&mut items, &uri, &item.name, "→  Open Artist");
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
            push_context_tail(&mut items, &uri, &item.name, "→  Open Album");
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
            push_context_tail(&mut items, &uri, &item.name, "→  Open");
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

/// Each store kind's lib-toggle status pair: (added, removed). One body serves
/// all four toggle arms; only the wording differs per kind.
fn toggle_msg(kind: StoreKind) -> (&'static str, &'static str) {
    match kind {
        StoreKind::Liked => (
            "added to Liked \u{2665} (press r to refresh)",
            "removed from Liked",
        ),
        StoreKind::Artist => ("following", "unfollowed"),
        StoreKind::Album => ("saved album", "removed album"),
        StoreKind::Playlist => ("added to library", "removed from library"),
    }
}

/// Toggle `uri` in the given store kind and return its status message.
fn apply_toggle(
    app: &mut App,
    kind: StoreKind,
    uri: String,
    name: String,
    subtitle: String,
) -> String {
    // The store mutated (added or removed) — the next 24s sync tick must
    // persist it even though playback may be idle.
    app.store_dirty = true;
    let (added, removed) = toggle_msg(kind);
    let is_added = app.store.toggle(kind, name, subtitle, uri);
    match kind {
        StoreKind::Liked => {
            let mut liked: Vec<LibItem> = vec![
                LibItem::play(
                    "▶︎  Play Liked Songs".into(),
                    "tuna:action:liked-play".into(),
                ),
                LibItem::header("Songs"),
            ];
            liked.extend(
                app.store
                    .liked
                    .iter()
                    .map(|e| LibItem::track(e.name.clone(), e.subtitle.clone(), e.uri.clone())),
            );
            app.browse.library.set(Section::Liked, liked);
        }
        StoreKind::Album => {
            let albums: Vec<LibItem> = app
                .store
                .albums
                .iter()
                .map(|a| LibItem::ctx(a.name.clone(), a.subtitle.clone(), a.uri.clone()))
                .collect();
            app.browse.library.set(Section::Albums, albums);
        }
        StoreKind::Artist => {
            let artists: Vec<LibItem> = app
                .store
                .artists
                .iter()
                .map(|a| LibItem::ctx(a.name.clone(), String::new(), a.uri.clone()))
                .collect();
            app.browse.library.set(Section::Artists, artists);
        }
        StoreKind::Playlist => {
            let playlists: Vec<LibItem> = app
                .store
                .playlists
                .iter()
                .map(|p| LibItem::ctx(p.name.clone(), p.subtitle.clone(), p.uri.clone()))
                .collect();
            app.browse.library.set(Section::Playlists, playlists);
        }
    }
    if is_added {
        added.into()
    } else {
        removed.into()
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
        } => apply_toggle(app, StoreKind::Liked, uri, name, subtitle),
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
                app.queue_dirty = true;
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
                Some(msg) => {
                    app.store_dirty = true;
                    let playlists: Vec<LibItem> = app
                        .store
                        .playlists
                        .iter()
                        .map(|p| LibItem::ctx(p.name.clone(), p.subtitle.clone(), p.uri.clone()))
                        .collect();
                    app.browse.library.set(Section::Playlists, playlists);
                    msg
                }
                None => "playlist gone — press r to reload the library".into(),
            }
        }
        ActionKind::ToggleFollowArtist { uri, name } => {
            apply_toggle(app, StoreKind::Artist, uri, name, String::new())
        }
        ActionKind::ToggleSaveAlbum {
            uri,
            name,
            subtitle,
        } => apply_toggle(app, StoreKind::Album, uri, name, subtitle),
        ActionKind::FollowPlaylist {
            uri,
            name,
            subtitle,
        } => apply_toggle(app, StoreKind::Playlist, uri, name, subtitle),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_menu_for_video_contains_all_actions() {
        let store = Store::default();
        let track = LibItem::track("Kesariya".into(), "Arijit Singh".into(), "yt:video:NJAv_7lHUIU".into());
        let menu = build_action_menu(&store, &track);
        assert_eq!(menu.title, "Kesariya");
        assert!(menu.items.iter().any(|i| i.label.contains("Liked")));
        assert!(menu.items.iter().any(|i| i.label.contains("Start Radio")));
        assert!(menu.items.iter().any(|i| i.label.contains("Add to Queue")));
        assert!(menu.items.iter().any(|i| i.label.contains("Add to Playlist")));
        assert!(menu.items.iter().any(|i| i.label.contains("Go to Artist")));
        assert!(menu.items.iter().any(|i| i.label.contains("Go to Album")));
        assert!(menu.items.iter().any(|i| i.label.contains("Copy Link")));
    }

    #[test]
    fn action_menu_for_artist_contains_open_and_follow() {
        let store = Store::default();
        let artist = LibItem::ctx("Arijit Singh".into(), "".into(), "yt:artist:Arijit Singh".into());
        let menu = build_action_menu(&store, &artist);
        assert!(menu.items.iter().any(|i| i.label.contains("Follow")));
        assert!(menu.items.iter().any(|i| i.label.contains("Play")));
        assert!(menu.items.iter().any(|i| i.label.contains("Open Artist")));
    }

    #[test]
    fn action_menu_for_album_contains_open_and_save() {
        let store = Store::default();
        let album = LibItem::ctx("Brahmastra".into(), "Pritam".into(), "yt:album:Brahmastra".into());
        let menu = build_action_menu(&store, &album);
        assert!(menu.items.iter().any(|i| i.label.contains("Save Album")));
        assert!(menu.items.iter().any(|i| i.label.contains("Play")));
        assert!(menu.items.iter().any(|i| i.label.contains("Open Album")));
    }
}
