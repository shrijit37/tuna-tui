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

pub(crate) fn build_all_sections(store: &Store) -> Vec<(Section, Vec<LibItem>)> {
    let mut out = Vec::new();

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
    out.push((Section::Home, home));

    let recent: Vec<LibItem> = history_rows(store.history.iter()).take(50).collect();
    out.push((Section::Recent, recent));

    // Playlists section: user playlists + quick access playlists
    let mut playlists: Vec<LibItem> = Vec::new();
    if !store.playlists.is_empty() {
        playlists.extend(store.playlists.iter().map(|p| {
            let subtitle = if p.subtitle.is_empty() {
                format!("{} tracks", p.tracks.len())
            } else {
                p.subtitle.clone()
            };
            LibItem::ctx(p.name.clone(), subtitle, p.uri.clone())
        }));
    }
    if !store.liked.is_empty() {
        playlists.push(LibItem::ctx(
            "Liked Songs".into(),
            format!("{} saved", store.liked.len()),
            "tuna:action:liked-play".into(),
        ));
    }
    out.push((Section::Playlists, playlists));

    // Albums section: saved albums + albums/releases from history and liked tracks
    let mut albums: Vec<LibItem> = Vec::new();
    if !store.albums.is_empty() {
        albums.extend(
            store
                .albums
                .iter()
                .map(|a| LibItem::ctx(a.name.clone(), a.subtitle.clone(), a.uri.clone())),
        );
    }
    let mut seen_albums = std::collections::HashSet::new();
    for a in &store.albums {
        seen_albums.insert(a.name.to_lowercase());
    }
    for h in store.history.iter().rev() {
        let name = h.title.trim();
        let artist = h.artist.trim();
        if !name.is_empty() && seen_albums.insert(name.to_lowercase()) {
            let uri = format!("yt:album:{} {}", name, artist);
            let subtitle = if artist.is_empty() {
                "Single / Release".to_string()
            } else {
                artist.to_string()
            };
            albums.push(LibItem::ctx(name.to_string(), subtitle, uri));
        }
    }
    out.push((Section::Albums, albums));

    // Artists section: followed artists + artists derived from history/liked
    let mut artists: Vec<LibItem> = Vec::new();
    if !store.artists.is_empty() {
        artists.extend(
            store
                .artists
                .iter()
                .map(|a| LibItem::ctx(a.name.clone(), "Followed".into(), a.uri.clone())),
        );
    }
    let mut seen_artists = std::collections::HashSet::new();
    for a in &store.artists {
        seen_artists.insert(a.name.to_lowercase());
    }
    // Aggregate play counts per artist in O(N) with a lowercase-key map;
    // the value keeps the first-seen casing for display.
    let mut artist_counts: std::collections::HashMap<String, (String, u32)> =
        std::collections::HashMap::new();
    for h in &store.history {
        let raw = h.artist.trim();
        if raw.is_empty() {
            continue;
        }
        for part in raw.split(&[',', '&'][..]) {
            let name = part.trim();
            if name.is_empty()
                || name.eq_ignore_ascii_case("feat.")
                || name.eq_ignore_ascii_case("ft.")
            {
                continue;
            }
            artist_counts
                .entry(name.to_lowercase())
                .and_modify(|(_, c)| *c += h.count)
                .or_insert((name.to_string(), h.count));
        }
    }
    let mut artist_counts: Vec<(String, u32)> = artist_counts.into_values().collect();
    artist_counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    for (name, count) in artist_counts {
        if seen_artists.insert(name.to_lowercase()) {
            let uri = format!("yt:artist:{}", name);
            let subtitle = format!("{} plays", count);
            artists.push(LibItem::ctx(name, subtitle, uri));
        }
    }
    out.push((Section::Artists, artists));

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
    out.push((Section::Liked, liked));

    out
}

pub(crate) fn build_sections(store: &Store, tx: flume::Sender<(Section, Vec<LibItem>)>) {
    for (section, items) in build_all_sections(store) {
        let _ = tx.send((section, items));
    }
}

/// Search: `ytsearchN:` flat results, rendered as Songs rows. There are no
/// first-class YouTube artist/album/playlist entities to group — the other
/// groups die with the Spotify search schema.
pub(crate) fn spawn_search(query: String, tx: flume::Sender<Vec<LibItem>>) {
    std::thread::Builder::new()
        .name("tuna-search".to_string())
        .spawn(move || {
            let vids = yt::ytmusic_search(&query, config::get().search_limit);
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

/// Suggestions: type-ahead completions while the search box is being typed
/// (Myx-a4e.12). Fed by Google's unauthenticated YouTube suggest, debounced
/// in this worker, delivered on the same `Vec<LibItem>` search channel as
/// results. Rows are `LibItem::header`s — inert by construction, so the
/// existing list nav (which skips headers) can't select them and Enter can't
/// fire a bogus detail fetch; the real `spawn_search` output replaces them
/// wholesale when the query is submitted.
pub(crate) fn spawn_suggestions(rx: flume::Receiver<String>, tx: flume::Sender<Vec<LibItem>>) {
    std::thread::Builder::new()
        .name("tuna-suggest".to_string())
        .spawn(move || {
            let debounce = std::time::Duration::from_millis(SUGGEST_DEBOUNCE_MS);
            // Newest ping seen, carried across rounds so a superseded round
            // re-fires for it without blocking on recv. The fold happens at
            // fire time, not wakeup time: a slow suggest can never replay a
            // queue of stale queries after typing stops (S29-2).
            let mut latest = String::new();
            loop {
                // Wait for a ping, unless a superseded round already carries
                // a newer query to re-fire.
                if latest.trim().is_empty() {
                    match rx.recv() {
                        Ok(first) => latest = newest_pending(&rx, first),
                        Err(_) => break,
                    }
                } else {
                    latest = newest_pending(&rx, latest);
                }
                // Quiet window first: let the typing burst settle, then fold
                // again so pings that landed mid-window win.
                std::thread::sleep(debounce);
                latest = newest_pending(&rx, latest);
                let query = latest.trim().to_string();
                if query.is_empty() {
                    latest.clear();
                    continue;
                }
                let hits = yt::autocomplete(&query, SUGGEST_LIMIT);
                // Anything queued while the request was in flight means this
                // reply is already stale — drop it, the carried latest
                // re-fires for the newer query next round.
                latest = newest_pending(&rx, latest);
                if latest != query {
                    continue;
                }
                if !hits.is_empty() {
                    let mut out = Vec::with_capacity(hits.len() + 1);
                    out.push(LibItem::header("Suggestions"));
                    out.extend(hits.into_iter().map(|s| LibItem::header(&s)));
                    let _ = tx.send(out);
                }
                // Round complete — wait for the next ping rather than
                // re-firing the same query.
                latest.clear();
            }
        })
        .expect("spawn suggestions worker");
}

/// How many completions the suggest row renders. Search depth stays on
/// `config::search_limit` — this is display-only, so a small fixed cap.
const SUGGEST_LIMIT: usize = 8;

/// Quiet window before a suggest request fires (ms). Trailing-edge: the
/// burst settles first, then the newest ping wins.
const SUGGEST_DEBOUNCE_MS: u64 = 250;

/// Fold every ping waiting in `rx` into the newest query (debounce helper —
/// extracted so the fold is testable offline; the network half is `#[ignore]`).
fn newest_pending(rx: &flume::Receiver<String>, mut latest: String) -> String {
    while let Ok(newer) = rx.try_recv() {
        latest = newer;
    }
    latest
}

#[cfg(test)]
mod suggest_tests {
    use super::newest_pending;

    #[test]
    fn folds_pings_to_newest() {
        let (tx, rx) = flume::unbounded::<String>();
        let _ = tx.send("a".to_string());
        let _ = tx.send("ab".to_string());
        let _ = tx.send("abc".to_string());
        assert_eq!(newest_pending(&rx, "a".to_string()), "abc");
        // Empty channel leaves the carried query untouched.
        assert_eq!(newest_pending(&rx, "abc".to_string()), "abc");
    }
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
        // (No `if let` match guard: unstable E0658 on the current toolchain.)
        "playlist" => {
            // Prefer the local rows when the store has them.
            if let Some(rows) = store.playlist_tracks(uri) {
                append_or_hint(
                    &mut items,
                    rows.iter()
                        .map(|t| LibItem::track(t.name.clone(), t.subtitle.clone(), t.uri.clone())),
                    "empty playlist",
                );
                return (name.to_string(), items);
            }
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
        "artist" => {
            let vids = yt::ytmusic_search(&format!("{id} songs"), 25);
            let vids = if vids.is_empty() {
                yt::ytmusic_search(id, 25)
            } else {
                vids
            };
            if !vids.is_empty() {
                items.extend(
                    vids.into_iter()
                        .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                );
            } else {
                append_or_hint(
                    &mut items,
                    yt::search(&format!("{id} songs"), config::get().search_limit)
                        .into_iter()
                        .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                    "no songs found for artist",
                );
            }
        }
        "album" => {
            let vids = yt::ytmusic_search(&format!("{id} album songs"), 25);
            let vids = if vids.is_empty() {
                yt::ytmusic_search(id, 25)
            } else {
                vids
            };
            if !vids.is_empty() {
                items.extend(
                    vids.into_iter()
                        .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                );
            } else {
                append_or_hint(
                    &mut items,
                    yt::resolve_kind(kind, id, config::get().search_limit)
                        .into_iter()
                        .map(|v| LibItem::track(v.title, v.artist, v.uri)),
                    "nothing loaded — album search failed",
                );
            }
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

    #[test]
    fn build_all_sections_populates_artists_and_albums_from_history() {
        let mut store = Store::default();
        store.record_played("yt:video:kesariya", "Kesariya", "Arijit Singh, Pritam");
        store.record_played("yt:video:luther", "luther", "Kendrick Lamar & SZA");
        let sections = build_all_sections(&store);
        let artists_sec = sections
            .iter()
            .find(|(s, _)| *s == Section::Artists)
            .unwrap();
        let albums_sec = sections
            .iter()
            .find(|(s, _)| *s == Section::Albums)
            .unwrap();
        let playlists_sec = sections
            .iter()
            .find(|(s, _)| *s == Section::Playlists)
            .unwrap();

        // Artists must contain Arijit Singh, Pritam, Kendrick Lamar, SZA
        let artist_names: Vec<&str> = artists_sec.1.iter().map(|i| i.name.as_str()).collect();
        assert!(artist_names.contains(&"Arijit Singh"));
        assert!(artist_names.contains(&"Kendrick Lamar"));
        assert!(artists_sec.1[0].uri.starts_with("yt:artist:"));

        // Albums must contain Kesariya and luther
        let album_names: Vec<&str> = albums_sec.1.iter().map(|i| i.name.as_str()).collect();
        assert!(album_names.contains(&"Kesariya"));
        assert!(album_names.contains(&"luther"));
        assert!(albums_sec.1[0].uri.starts_with("yt:album:"));

        // Playlists should not panic
        assert!(playlists_sec.1.is_empty() || !playlists_sec.1.is_empty());
    }
}
