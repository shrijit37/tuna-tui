//! The application state — the thing the other three layers are about.
//!
//! `ui/` reads `&App` and writes `FrameOut`; `input/` mutates `App`; `api/`
//! touches neither and talks HTTP over channels. This module is the state in
//! the middle. One module per part of the model, so the file to open is the one
//! named after it; `App` itself lives here, since every one of those parts
//! hangs off it.
//!
//! It would be tidier if this module depended on none of the others, and it
//! nearly does — with two exceptions, both in `event.rs`, where handling an
//! engine event spawns a fetch directly (`fetch_track_meta`, and the lyrics
//! fetch). Those reach into `api/`. The intended shape is for `event.rs` to
//! send a request over a channel and let `main.rs` — the wiring layer, which is
//! allowed to know both sides — service it. Until that lands, this is a real
//! edge in the graph, not an aspiration, so don't add more of them.

mod action;
mod event;
mod frame;
mod library;
mod persist;
mod playback;
mod state;

pub(crate) use action::*;
pub(crate) use event::*;
pub(crate) use frame::*;
pub(crate) use library::*;
pub(crate) use persist::*;
pub(crate) use playback::*;
pub(crate) use state::*;

use crate::*;

pub(crate) struct App {
    pub(crate) svc: Services,
    pub(crate) theme: ThemeState,
    pub(crate) playback: PlaybackState,
    // Best-effort OS integration. Headless/SSH sessions may not expose the
    // platform media service, but that must never prevent Tuna TUI from playing.
    pub(crate) media_controls: Option<MediaControls>,
    // The TXC colour publisher, when one could be bound. `None` means
    // publishing is disabled (`TUNA_NO_COLOR_SOCKET`) or the bind failed — both
    // are ordinary states, not errors: a player that refuses to play music
    // because a socket is unavailable would be a worse player. Every use site
    // is a `if let Some(..)`, so `None` is simply inert.
    #[cfg(all(feature = "txc", unix))]
    pub(crate) txc: Option<tuna_tui::txc::publish::Publisher>,
    pub(crate) status: String,
    pub(crate) browse: BrowseState,
    pub(crate) transport: Transport,
    pub(crate) search: SearchState,
    pub(crate) view: ViewState,
    pub(crate) session: SessionState,
    // The local library (likes / follows / saves / play history), persisted
    // to `state.json` alongside the rest of the session.
    pub(crate) store: Store,
    // Dirty gates for the periodic save tick: the store mutated (like /
    // follow / save / playlist add) or the transport queue was appended.
    // Set at the mutation sites; consumed (and reset) by the 24s sync tick
    // in `run_ui`. While playing, `transport.playback_started` keeps the
    // save cadence on its own, so these only matter when playback is idle.
    pub(crate) store_dirty: bool,
    pub(crate) queue_dirty: bool,
    // What the album art box owes the next frame. See ArtRepaint.
    pub(crate) art_repaint: ArtRepaint,
}

impl App {
    pub(crate) fn cur_items(&self) -> &[LibItem] {
        if let Some(d) = self.browse.details.last() {
            &d.items
        } else if self.search.searching {
            &self.search.search_results
        } else {
            self.browse.library.items(self.browse.section)
        }
    }
    pub(crate) fn cur_list_mut(&mut self) -> &mut Vec<LibItem> {
        if let Some(d) = self.browse.details.last_mut() {
            &mut d.items
        } else if self.search.searching {
            &mut self.search.search_results
        } else {
            self.browse.library.items_mut(self.browse.section)
        }
    }
    /// First non-header index (where a fresh selection should land).
    pub(crate) fn first_selectable(&self) -> usize {
        self.cur_items()
            .iter()
            .position(|i| !i.is_header)
            .unwrap_or(0)
    }
    /// Move the selection by `dir`, skipping header rows, clamped at the ends.
    pub(crate) fn move_sel(&mut self, dir: isize) {
        let items = self.cur_items();
        let n = items.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.browse.selected as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if !items[i as usize].is_header {
                self.browse.selected = i as usize;
                return;
            }
        }
    }
    /// If the selection landed on a header (e.g. after data loads), bump it off.
    pub(crate) fn normalize_selection(&mut self) {
        if self
            .cur_items()
            .get(self.browse.selected)
            .is_some_and(|i| i.is_header)
        {
            self.browse.selected = self.first_selectable();
        }
    }
    /// The single entry point for "play this context URI".
    ///
    /// Every caller must route through here so `source` / `source_name` stay in
    /// sync with what is actually playing — they back the Queue view's
    /// PLAYING FROM header and the resume-on-launch path in `resume_source`.
    /// `name` is a parameter rather than being derived from `details.last()`
    /// because the drill-in stack is empty when playing straight from a list.
    pub(crate) fn play_context_row(&mut self, uri: String, name: String, shuffle: bool) {
        self.status = format!("starting {name}…");
        self.transport.source = PlaySource::Context(uri.clone());
        self.transport.source_name = name;
        if let Err(e) = self.svc.engine.play_context(uri, shuffle) {
            self.status = format!("couldn't play: {e:#}");
        }
        self.refresh_local_queue();
    }

    /// Absorb title/artist hints drained from the engine into the session meta cache.
    pub(crate) fn absorb_meta_hints(&mut self) {
        for (uri, title, artist) in self.svc.engine.take_meta_hints() {
            self.session.meta_cache.insert(uri, (title, artist));
        }
    }

    /// A display string for a track uri: the cached "title — artist" once its
    /// metadata has landed, the bare uri before that.
    pub(crate) fn track_label_of(&self, uri: &str) -> String {
        if let Some((t, a)) = self.session.meta_cache.get(uri) {
            if a.is_empty() {
                return t.clone();
            } else {
                return format!("{t} — {a}");
            }
        }
        // Fallback: check store history, liked, and playlists
        if let Some(h) = self.store.history.iter().find(|h| h.uri == uri) {
            if !h.title.is_empty() {
                if h.artist.is_empty() {
                    return h.title.clone();
                } else {
                    return format!("{} — {}", h.title, h.artist);
                }
            }
        }
        if let Some(l) = self.store.liked.iter().find(|l| l.uri == uri) {
            if !l.name.is_empty() {
                if l.subtitle.is_empty() {
                    return l.name.clone();
                } else {
                    return format!("{} — {}", l.name, l.subtitle);
                }
            }
        }
        for p in &self.store.playlists {
            if let Some(t) = p.tracks.iter().find(|t| t.uri == uri) {
                if !t.name.is_empty() {
                    if t.subtitle.is_empty() {
                        return t.name.clone();
                    } else {
                        return format!("{} — {}", t.name, t.subtitle);
                    }
                }
            }
        }
        uri.to_string()
    }

    /// Refresh the Queue view's data from the engine's loaded list (the local
    /// replacement for the dead server queue). Called after every play start
    /// and on the periodic persist tick.
    pub(crate) fn refresh_local_queue(&mut self) {
        let uris = self.svc.engine.queue();
        if uris.is_empty() {
            return;
        }
        let titles: Vec<String> = uris.iter().map(|u| self.track_label_of(u)).collect();
        self.transport.queue_uris = uris;
        self.transport.queue = titles;
    }

    /// Play whatever's selected (in the current section, or in search results).
    /// Act on the selected item. Returns what the caller should do next.
    pub(crate) fn activate(&mut self) -> Activated {
        let Some(item) = self.cur_items().get(self.browse.selected).cloned() else {
            return Activated::None;
        };
        if item.is_header {
            return Activated::None;
        }
        if item.is_play {
            // Special synthetic rows: play the Liked list (optionally shuffled).
            // `myx:` rows can only come from state.json written pre-rename; the
            // matcher still accepts them so a like-play action never silently
            // degrades to "hmm, that row does nothing".
            if matches!(
                item.uri.as_str(),
                "tuna:action:liked-play" | "myx:action:liked-play"
            ) {
                let tracks: Vec<(String, String, String)> = self
                    .browse
                    .library
                    .liked
                    .iter()
                    .filter(|i| i.is_track && !i.name.is_empty())
                    .map(|i| (i.uri.clone(), i.name.clone(), i.subtitle.clone()))
                    .collect();
                for (uri, name, subtitle) in &tracks {
                    self.session
                        .meta_cache
                        .insert(uri.clone(), (name.clone(), subtitle.clone()));
                }
                let uris: Vec<String> = tracks.into_iter().map(|(u, _, _)| u).collect();
                if !uris.is_empty() {
                    self.transport.source = PlaySource::Liked;
                    self.transport.source_name = "Liked Songs".to_string();
                    self.status = "starting Liked Songs…".to_string();
                    // Honour the current shuffle toggle instead of a dedicated row.
                    if let Err(e) =
                        self.svc
                            .engine
                            .play_tracks(uris, None, 0, self.transport.shuffle)
                    {
                        self.status = format!("couldn't play: {e:#}");
                    }
                }
                return Activated::None;
            }
            // Inside a drill-in the enclosing title is the better label
            // ("Chill Vibes"); standalone play rows fall back to their own.
            let name = self
                .browse
                .details
                .last()
                .map(|d| d.title.clone())
                .unwrap_or_else(|| item.name.clone());
            let shuffle = self.transport.shuffle;
            if let Some(d) = self.browse.details.last() {
                for item in d.items.iter().filter(|i| i.is_track) {
                    if !item.name.is_empty() {
                        self.session
                            .meta_cache
                            .insert(item.uri.clone(), (item.name.clone(), item.subtitle.clone()));
                    }
                }
                let uris: Vec<String> = d
                    .items
                    .iter()
                    .filter(|i| i.is_track)
                    .map(|i| i.uri.clone())
                    .collect();
                if !uris.is_empty() {
                    self.transport.source = PlaySource::Context(d.context_uri.clone());
                    self.transport.source_name = name.clone();
                    self.status = format!("starting {name}…");
                    if let Err(e) = self.svc.engine.play_tracks(uris, None, 0, shuffle) {
                        self.status = format!("couldn't play: {e:#}");
                    }
                    self.refresh_local_queue();
                    return Activated::None;
                }
            }
            self.play_context_row(item.uri, name, shuffle);
            return Activated::None;
        }
        if item.is_track {
            if self.search.searching {
                // A search-result song starts that song's radio (seed + similar).
                self.transport.source = PlaySource::Radio(item.uri.clone());
                self.transport.source_name = format!("Radio · {}", item.name);
                return Activated::Radio(item.uri);
            }
            // Inside a drill-in → play its context at this track (real queue).
            if let Some(d) = self.browse.details.last() {
                let ctx = d.context_uri.clone();
                self.transport.source = PlaySource::Context(ctx);
                self.transport.source_name = d.title.clone();
                self.status = format!("starting {}…", item.name);
                for it in d.items.iter().filter(|i| i.is_track) {
                    if !it.name.is_empty() {
                        self.session
                            .meta_cache
                            .insert(it.uri.clone(), (it.name.clone(), it.subtitle.clone()));
                    }
                }
                let uris: Vec<String> = d
                    .items
                    .iter()
                    .filter(|i| i.is_track)
                    .map(|i| i.uri.clone())
                    .collect();
                if !uris.is_empty() {
                    if let Err(e) = self.svc.engine.play_tracks(
                        uris,
                        Some(item.uri.clone()),
                        0,
                        self.transport.shuffle,
                    ) {
                        self.status = format!("couldn't play: {e:#}");
                    }
                }
                return Activated::None;
            }
            // Section track list.
            let tracks: Vec<(String, String, String)> = self
                .cur_items()
                .iter()
                .filter(|i| i.is_track && !i.name.is_empty())
                .map(|i| (i.uri.clone(), i.name.clone(), i.subtitle.clone()))
                .collect();
            for (uri, name, subtitle) in &tracks {
                self.session
                    .meta_cache
                    .insert(uri.clone(), (name.clone(), subtitle.clone()));
            }
            let uris: Vec<String> = tracks.into_iter().map(|(u, _, _)| u).collect();
            self.status = format!("starting {}…", item.name);
            if self.browse.section == Section::Liked {
                self.transport.source = PlaySource::Liked;
                self.transport.source_name = "Liked Songs".to_string();
            } else {
                self.transport.source = PlaySource::None;
                self.transport.source_name = self.browse.section.label().to_string();
            }
            if let Err(e) =
                self.svc
                    .engine
                    .play_tracks(uris, Some(item.uri.clone()), 0, self.transport.shuffle)
            {
                self.status = format!("couldn't play: {e:#}");
            }
            return Activated::None;
        }
        // Otherwise it's a context (artist / album / playlist) — open it.
        self.status = format!("opening {}…", item.name);
        Activated::Open(item.uri, item.name)
    }
}
