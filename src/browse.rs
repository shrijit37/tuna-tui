//! The browse surface: library sections, search, and drill-ins.
//!
//! Everything the old `api/` layer fetched from api.spotify.com now comes from
//! two places: the local store + rolling history (`state.json`, zero network)
//! for Home/Recent/Liked/Albums/Artists/Playlists, and `src/yt` (the yt-dlp
//! CLI) for search and context contents. One-way dependency, like `api/` was:
//! spawns a worker thread, hands plain data back over channels, never touches
//! `App` or the render tree.
//!
//! The channel contracts (`(Section, Vec<LibItem>)`, search and detail
//! replies) are unchanged from the Spotify era — only the fetchers and their
//! call sites know they stopped being HTTP.

use crate::app::*;
use tuna_tui::config;
use tuna_tui::util::uri_parts;
use tuna_tui::yt;

/// Fetch the library incrementally: fast sections first, all local. The
/// `(Section, Vec<LibItem>)` chunks match the old Spotify fetch; there is no
/// done signal anymore — nothing can fail at the transport, an empty store is
/// a legitimate (fresh) state, and the reload path re-spawns with a fresh
/// store snapshot. (The app clears its loading status once the last section
/// of a drain lands.)
pub(crate) fn spawn_library_fetch(store: Store, tx: flume::Sender<(Section, Vec<LibItem>)>) {
    std::thread::Builder::new()
        .name("tuna-library".to_string())
        .spawn(move || build_sections(&store, tx))
        .expect("spawn library worker");
}

fn build_sections(store: &Store, tx: flume::Sender<(Section, Vec<LibItem>)>) {
    // Home: a rolling mix of recently played and most-played tracks.
    let mut home: Vec<LibItem> = Vec::new();
    if store.history.is_empty() {
        home.push(LibItem::header("nothing played yet — search for a song"));
    } else {
        home.push(LibItem::header("Recently Played"));
        home.extend(history_rows(store.history.iter()).take(6));
        let mut top: Vec<&PlayedEntry> = store.history.iter().collect();
        top.sort_by(|a, b| b.count.cmp(&a.count).then(b.last_ms.cmp(&a.last_ms)));
        home.push(LibItem::header("Top Tracks"));
        home.extend(history_rows(top.into_iter()).take(8));
    }
    let _ = tx.send((Section::Home, home));

    let recent: Vec<LibItem> = history_rows(store.history.iter()).take(50).collect();
    let _ = tx.send((Section::Recent, recent));

    let playlists: Vec<LibItem> = store
        .playlists
        .iter()
        .map(|p| {
            let subtitle = if p.subtitle.is_empty() {
                format!("{} saved", p.tracks.len())
            } else {
                p.subtitle.clone()
            };
            LibItem::ctx(p.name.clone(), subtitle, p.uri.clone())
        })
        .collect();
    let _ = tx.send((Section::Playlists, playlists));

    let albums: Vec<LibItem> = store
        .albums
        .iter()
        .map(|a| LibItem::ctx(a.name.clone(), a.subtitle.clone(), a.uri.clone()))
        .collect();
    let _ = tx.send((Section::Albums, albums));

    let artists: Vec<LibItem> = store
        .artists
        .iter()
        .map(|a| LibItem::ctx(a.name.clone(), String::new(), a.uri.clone()))
        .collect();
    let _ = tx.send((Section::Artists, artists));

    // Liked keeps its synthetic play row and header, matching the old shape.
    let mut liked: Vec<LibItem> = vec![
        LibItem::play(
            "▶︎  Play Liked Songs".into(),
            "tuna:action:liked-play".into(),
        ),
        LibItem::header("Songs"),
    ];
    liked.extend(
        store
            .liked
            .iter()
            .map(|e| LibItem::track(e.name.clone(), e.subtitle.clone(), e.uri.clone())),
    );
    let _ = tx.send((Section::Liked, liked));
}

/// Search: `ytsearchN:` flat results, rendered as Songs rows. There are no
/// first-class YouTube artist/album/playlist entities to group — the other
/// groups die with the Spotify search schema.
pub(crate) fn spawn_search(query: String, tx: flume::Sender<Vec<LibItem>>) {
    std::thread::Builder::new()
        .name("tuna-search".to_string())
        .spawn(move || {
            let vids = yt::search(&query, config::get().search_limit);
            let mut out = Vec::new();
            if !vids.is_empty() {
                out.push(LibItem::header("Songs"));
            }
            out.extend(
                vids.into_iter()
                    .map(|v| LibItem::track(v.title, v.artist, v.uri)),
            );
            let _ = tx.send(out);
        })
        .expect("spawn search worker");
}

/// Drill into a context (playlist / channel / album / single video). The
/// response tuple `(context_uri, title, items)` matches the old fetch.
pub(crate) fn spawn_detail_fetch(
    store: Store,
    uri: String,
    name: String,
    tx: flume::Sender<(String, String, Vec<LibItem>)>,
) {
    std::thread::Builder::new()
        .name("tuna-detail".to_string())
        .spawn(move || {
            let (title, items) = fetch_detail_blocking(&store, &uri, &name);
            let _ = tx.send((uri, title, items));
        })
        .expect("spawn detail worker");
}

pub(crate) fn fetch_detail_blocking(
    store: &Store,
    uri: &str,
    name: &str,
) -> (String, Vec<LibItem>) {
    // "Play all" row first.
    let mut items = vec![LibItem::play(format!("▶︎ Play {name}"), uri.to_string())];

    let (_, kind, id) = match uri_parts(uri) {
        Some(p) => p,
        None => return (name.to_string(), items),
    };

    match kind {
        // A playlist whose contents have grown locally renders its own rows;
        // otherwise the network copy (flat-extracted) is the contents.
        "playlist" if let Some(rows) = store.playlist_tracks(uri) => {
            append_or_hint(
                &mut items,
                rows.iter()
                    .map(|t| LibItem::track(t.name.clone(), t.subtitle.clone(), t.uri.clone())),
                "empty playlist",
            );
        }
        "playlist" => {
            // Capped (F14): the drill-in view must not paginate a whole
            // multi-hundred-row playlist. Deliberately NOT `search_limit`
            // (defaults to 6 — would truncate a 30-track playlist with no
            // hint) and NOT `resolve_kind`: that table feeds the PLAY path,
            // which must stay un-capped (bead Myx-a4.8).
            append_or_hint(
                &mut items,
                kind_rows(&yt::playlist_entries_capped(
                    &tuna_tui::util::playlist_uri(id),
                    yt::DRILLIN_FETCH_LIMIT,
                )),
                "no tracks — empty or restricted",
            );
        }
        "channel" => {
            append_or_hint(
                &mut items,
                kind_rows(&yt::playlist_entries_capped(
                    &tuna_tui::util::channel_videos_url(id),
                    yt::DRILLIN_FETCH_LIMIT,
                )),
                "no uploads — empty or restricted",
            );
        }
        "album" => {
            // YouTube has no first-class albums; the saved slug searches.
            append_or_hint(
                &mut items,
                yt::resolve_kind(kind, id, config::get().search_limit)
                    .into_iter()
                    .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                "nothing loaded — search failed",
            );
        }
        "video" => {
            append_or_hint(
                &mut items,
                yt::video_meta(id)
                    .into_iter()
                    .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                "couldn't load — check the network",
            );
        }
        _ => {}
    }

    (name.to_string(), items)
}

/// Extend `items` with `rows`; when the source yielded nothing (an empty or
/// restricted playlist, a failed search), push a header `hint` instead.
/// Returns whether rows were added.
fn append_or_hint(
    items: &mut Vec<LibItem>,
    rows: impl IntoIterator<Item = LibItem>,
    hint: &str,
) -> bool {
    let before = items.len();
    items.extend(rows);
    if items.len() == before {
        items.push(LibItem::header(hint));
        false
    } else {
        true
    }
}

/// The history rows shared by Home's Recently Played / Top Tracks and the
/// Recent section — one mapping so `PlayedEntry`'s shape stays in sync
/// everywhere it renders.
fn history_rows<'a>(
    rows: impl Iterator<Item = &'a PlayedEntry> + 'a,
) -> impl Iterator<Item = LibItem> + 'a {
    rows.map(|h| LibItem::track(h.title.clone(), h.artist.clone(), h.uri.clone()))
}

/// Playlist / channel rows in local style: flat entries are title-only —
/// split "Artist - Title (…)" so the list isn't long pasted strings.
fn kind_rows(rows: &[yt::YtVideo]) -> Vec<LibItem> {
    rows.iter()
        .map(|v| {
            let (name, subtitle) = title_artist_split(&v.title);
            LibItem::track(name, subtitle, v.uri.clone())
        })
        .collect()
}

/// Split a YouTube title at a power-separator: "Artist - Title (Official
/// Video)" → ("Title (Official Video)", "Artist"). Falls back to the whole
/// string (subtitle empty) when no separator exists.
fn title_artist_split(s: &str) -> (String, String) {
    for sep in [" – ", " - ", " — ", "-"] {
        if let Some((artist, title)) = s.split_once(sep) {
            return (title.trim().to_string(), artist.trim().to_string());
        }
    }
    (s.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_pulls_artist_and_title_around_dashes() {
        assert_eq!(
            title_artist_split("Queen – Bohemian Rhapsody (Official Video Remastered)"),
            (
                "Bohemian Rhapsody (Official Video Remastered)".to_string(),
                "Queen".to_string()
            )
        );
        assert_eq!(
            title_artist_split("Survivor - Eye Of The Tiger (Official HD Video)"),
            (
                "Eye Of The Tiger (Official HD Video)".to_string(),
                "Survivor".to_string()
            )
        );
        assert_eq!(
            title_artist_split("Some Plain Podcast Episode"),
            ("Some Plain Podcast Episode".to_string(), String::new())
        );
        // No double-split: "A - B - C" takes the first separator only.
        assert_eq!(
            title_artist_split("A - B - C"),
            ("B - C".to_string(), "A".to_string())
        );
    }

    #[test]
    fn liked_rows_keep_the_synthetic_play_row() {
        let mut store = Store::default();
        store.toggle(
            StoreKind::Liked,
            "t".into(),
            "a".into(),
            "yt:video:x".into(),
        );
        let (tx, rx) = flume::unbounded();
        build_sections(&store, tx);
        // Six sections, Liked last — drain the five before it.
        for _ in 0..5 {
            rx.recv().unwrap();
        }
        let (s, liked) = rx.recv().unwrap();
        assert_eq!(s, Section::Liked);
        assert!(liked[0].is_play);
        assert_eq!(liked[0].uri, "tuna:action:liked-play");
        assert!(liked[1].is_header);
        assert_eq!(liked[2].uri, "yt:video:x");
    }

    #[test]
    fn home_builds_recent_and_top_from_history() {
        let mut store = Store::default();
        // Play one track more than the others: it must lead "Top Tracks".
        store.record_played("yt:video:a", "Alpha", "AA");
        store.record_played("yt:video:b", "Beta", "BB");
        store.record_played("yt:video:b", "Beta", "BB");
        let (tx, rx) = flume::unbounded();
        build_sections(&store, tx);
        let (sec, home) = rx.recv().unwrap();
        assert_eq!(sec, Section::Home);
        let headers: Vec<&str> = home
            .iter()
            .filter(|i| i.is_header)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(headers, ["Recently Played", "Top Tracks"]);
        let top: Vec<&str> = home
            .iter()
            .skip_while(|i| !i.is_header || i.name != "Top Tracks")
            .filter(|i| !i.is_header)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(top, ["Beta", "Alpha"]);
    }

    #[test]
    fn empty_history_renders_a_hint_not_nothing() {
        let (tx, rx) = flume::unbounded();
        build_sections(&Store::default(), tx);
        let (_, home) = rx.recv().unwrap();
        assert!(home.iter().all(|i| i.is_header));
        assert_eq!(home[0].name, "nothing played yet — search for a song");
    }
}
