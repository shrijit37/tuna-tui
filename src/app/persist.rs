//! The session snapshot on disk (~/.cache/tuna-tui/state.json).

use crate::*;

/// Persisted across sessions (~/.cache/tuna-tui/state.json).
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedState {
    pub(crate) volume: u8,
    #[serde(default)]
    pub(crate) shuffle: bool,
    #[serde(default)]
    pub(crate) repeat: bool,
    #[serde(default)]
    pub(crate) last_played: Option<LastPlayed>,
    pub(crate) queue: Vec<String>,
    #[serde(default)]
    pub(crate) queue_uris: Vec<String>,
    #[serde(default)]
    pub(crate) source: PlaySource,
    #[serde(default)]
    pub(crate) source_name: String,
    #[serde(default)]
    pub(crate) store: Store,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct LastPlayed {
    pub(crate) uri: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_ms: u32,
    pub(crate) position_ms: u32,
}

/// A saved library row: the display triple plus the uri, captured at save time
/// (rows already carry name and subtitle) so the local library renders without
/// any network fetch.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct LibEntry {
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) uri: String,
}

/// A saved playlist: the browse row for the Playlists section plus the tracks
/// added to it locally (empty for a just-saved external playlist, whose contents
/// still come from YouTube on drill-in).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Playlist {
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) uri: String,
    #[serde(default)]
    pub(crate) tracks: Vec<LibEntry>,
}

/// One played track's history slot, feeding Home (Recently Played + Top Tracks).
/// `count` orders "top", `last_ms` (epoch seconds) breaks ties and keeps the
/// file diff clean on re-record.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct PlayedEntry {
    pub(crate) uri: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) count: u32,
    pub(crate) last_ms: u64,
}

/// The local library: everything the old Spotify API used to own that now lives
/// in `state.json`. Like/follow/save writes land here; the browse sections are
/// rendered straight from it.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Store {
    pub(crate) liked: Vec<LibEntry>,
    pub(crate) albums: Vec<LibEntry>,
    pub(crate) artists: Vec<LibEntry>,
    pub(crate) playlists: Vec<Playlist>,
    /// Most recent first.
    pub(crate) history: Vec<PlayedEntry>,
}

/// How many history slots are kept (Home's top list draws from this).
pub(crate) const HISTORY_CAP: usize = 100;

/// Which local store a toggle targets — one-to-one with `ActionKind`'s library
/// writes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreKind {
    Liked,
    Album,
    Artist,
    Playlist,
}

impl Store {
    pub(crate) fn contains(&self, kind: StoreKind, uri: &str) -> bool {
        match kind {
            StoreKind::Liked => self.liked.iter().any(|e| e.uri == uri),
            StoreKind::Album => self.albums.iter().any(|e| e.uri == uri),
            StoreKind::Artist => self.artists.iter().any(|e| e.uri == uri),
            StoreKind::Playlist => self.playlists.iter().any(|p| p.uri == uri),
        }
    }

    /// Toggle `entry` in the store. Returns the new state (true = now saved).
    pub(crate) fn toggle(
        &mut self,
        kind: StoreKind,
        name: String,
        subtitle: String,
        uri: String,
    ) -> bool {
        let entry = LibEntry {
            name,
            subtitle,
            uri,
        };
        match kind {
            StoreKind::Liked => toggle_into(&mut self.liked, entry),
            StoreKind::Album => toggle_into(&mut self.albums, entry),
            StoreKind::Artist => toggle_into(&mut self.artists, entry),
            StoreKind::Playlist => {
                let saved = self.playlists.iter().any(|p| p.uri == entry.uri);
                if saved {
                    self.playlists.retain(|p| p.uri != entry.uri);
                } else {
                    self.playlists.push(Playlist {
                        name: entry.name,
                        subtitle: entry.subtitle,
                        uri: entry.uri,
                        tracks: Vec::new(),
                    });
                }
                !saved
            }
        }
    }

    /// Append a track to the named saved playlist (the "Add to Playlist" menu).
    /// `None` when no saved playlist matches — the caller's "no playlists" status.
    pub(crate) fn add_to_playlist(
        &mut self,
        uri: &str,
        name: String,
        track: LibEntry,
    ) -> Option<String> {
        let p = self.playlists.iter_mut().find(|p| p.uri == uri)?;
        if p.tracks.iter().any(|t| t.uri == track.uri) {
            return Some(format!("already in {name}"));
        }
        p.tracks.push(track);
        Some(format!("added to {name}"))
    }

    /// A saved playlist whose contents have grown locally wins over the
    /// network copy on drill-in.
    pub(crate) fn playlist_tracks(&self, uri: &str) -> Option<&[LibEntry]> {
        self.playlists
            .iter()
            .find(|p| p.uri == uri)
            .filter(|p| !p.tracks.is_empty())
            .map(|p| p.tracks.as_slice())
    }

    /// Record one completed track change in the rolling history.
    /// Skip entries without a title — they're nothing to surface.
    pub(crate) fn record_played(&mut self, uri: &str, title: &str, artist: &str) {
        if title.is_empty() || uri.is_empty() {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match self.history.iter_mut().find(|h| h.uri == uri) {
            Some(h) => {
                h.count = h.count.saturating_add(1);
                h.last_ms = now;
                h.title = title.to_string();
                h.artist = artist.to_string();
            }
            None => {
                self.history.insert(
                    0,
                    PlayedEntry {
                        uri: uri.to_string(),
                        title: title.to_string(),
                        artist: artist.to_string(),
                        count: 1,
                        last_ms: now,
                    },
                );
                self.history.truncate(HISTORY_CAP);
            }
        }
    }
}

fn toggle_into(list: &mut Vec<LibEntry>, entry: LibEntry) -> bool {
    let saved = list.iter().any(|e| e.uri == entry.uri);
    if saved {
        list.retain(|e| e.uri != entry.uri);
    } else {
        list.push(entry);
    }
    !saved
}

impl SavedState {
    pub(crate) fn path() -> Option<std::path::PathBuf> {
        Some(tuna_tui::home_dir()?.join(".cache/tuna-tui/state.json"))
    }
    pub(crate) fn load() -> SavedState {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub(crate) fn save(&self) {
        // The dir is created (0700) by the shared helper — same one-time cost
        // as the old create_dir_all, strictly more private on unix.
        let Some(dir) = tuna_tui::util::ensure_cache_dir_0700() else {
            return;
        };
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(dir.join("state.json"), json);
        }
    }
}

/// Snapshot the current session (volume, last track, position, queue).
///
/// Building the snapshot is cheap (strings + the store clone); the caller
/// writes it off the UI thread — serializing a few hundred KB and fs-writing
/// on `save()` must not freeze the render loop.
pub(crate) fn save_state(app: &App) -> SavedState {
    let last_played = app.playback.now.as_ref().map(|now| LastPlayed {
        uri: now.uri.clone(),
        title: now.title.clone(),
        artist: now.artist.clone(),
        album: now.album.clone(),
        duration_ms: now.duration_ms,
        position_ms: app.playback.position_ms(),
    });

    SavedState {
        volume: app.transport.volume,
        shuffle: app.transport.shuffle,
        repeat: app.transport.repeat,
        last_played,
        queue: app.transport.queue.clone(),
        queue_uris: app.transport.queue_uris.clone(),
        source: app.transport.source.clone(),
        source_name: app.transport.source_name.clone(),
        store: app.store.clone(),
    }
}
