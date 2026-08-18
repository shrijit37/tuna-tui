//! The session snapshot on disk (~/.cache/tuna-tui/state.json).

use crate::*;

/// Persisted across sessions (~/.cache/tuna-tui/state.json).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
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

    /// Load the saved session. The on-disk rules live in [`Self::load_from`];
    /// this wrapper only resolves the home path, so the rules stay testable
    /// without mutating `$HOME` in parallel test threads.
    pub(crate) fn load() -> SavedState {
        Self::path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Read `path` and recover. A missing file (first run) and any read error
    /// silently yield a default session — never a log line. A corrupt file
    /// logs the reset and falls back to the `.bak` the save dance keeps (stale
    /// by at most one save), then to a default session.
    fn load_from(path: &std::path::Path) -> SavedState {
        let Ok(text) = std::fs::read_to_string(path) else {
            return SavedState::default(); // first run, or unreadable — never logged
        };
        match serde_json::from_str(&text) {
            Ok(state) => state,
            Err(_) => {
                let bak = path.with_extension("json.bak");
                tuna_tui::liblog::liblog(format!(
                    "{} corrupt; recovering from {bak:?}",
                    path.display()
                ));
                std::fs::read_to_string(&bak)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default()
            }
        }
    }

    pub(crate) fn save(&self) {
        // F19: the cache dir already exists at boot (the single-instance lock
        // created it). Write straight to it and only recreate the dir when a
        // write fails — the mid-session-deleted-dir self-heal survives without
        // per-save create_dir_all + chmod syscalls. Errors stay swallowed.
        let Some(path) = Self::path() else {
            return;
        };
        if self.save_to(&path) {
            return;
        }
        tuna_tui::util::ensure_cache_dir_0700();
        let _ = self.save_to(&path);
    }

    /// Write `path` atomically (unique tmp + fsync + rename) with a `.bak`
    /// dance: the previous state.json becomes state.json.bak before the new
    /// file takes its place, so a torn or corrupt state.json has a recovery
    /// copy. Every move is best-effort — on failure the previous file remains
    /// at state.json.bak and false is returned (the retry in [`Self::save`]
    /// and the next periodic save both pick it up).
    fn save_to(&self, path: &std::path::Path) -> bool {
        let Ok(json) = serde_json::to_string(self) else {
            return false;
        };
        let bak = path.with_extension("json.bak");
        #[cfg(windows)]
        let _ = std::fs::remove_file(&bak); // rename cannot replace on Windows
        let _ = std::fs::rename(path, &bak); // best-effort; no-op on first run
        tuna_tui::util::write_atomic(path, json.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tuna-tui-persist-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A distinct, non-default state: the volume and one liked row differ per
    /// call, so "equals one of the stores" is checkable via its serialized
    /// form (SavedState has no PartialEq — PlaySource lives in an off-limits
    /// file and serde output is field-ordered and deterministic).
    fn state(volume: u8, uri: &str) -> SavedState {
        SavedState {
            volume,
            store: Store {
                liked: vec![LibEntry {
                    name: uri.into(),
                    subtitle: String::new(),
                    uri: uri.into(),
                }],
                ..Store::default()
            },
            ..SavedState::default()
        }
    }

    fn json(state: &SavedState) -> String {
        serde_json::to_string(state).unwrap()
    }

    #[test]
    fn missing_file_returns_default() {
        // HARD requirement (F18): the first-run path must stay a silent
        // default — the load-side recovery must not fire on a missing file.
        let dir = scratch("missing");
        let path = dir.join("state.json");
        assert_eq!(
            json(&SavedState::load_from(&path)),
            json(&SavedState::default())
        );
    }

    #[test]
    fn torn_write_recovers_from_bak() {
        let dir = scratch("bak");
        let path = dir.join("state.json");
        let a = state(7, "yt:video:aaa");
        let b = state(9, "yt:video:bbb");
        assert!(a.save_to(&path));
        assert!(b.save_to(&path));
        // A mid-write interruption leaves garbage in state.json; the load must
        // fall back to the .bak (the state before the latest save), not to an
        // empty default library.
        std::fs::write(&path, "{\"volume\": 255, torn").unwrap();
        assert_eq!(json(&SavedState::load_from(&path)), json(&a));
    }

    #[test]
    fn no_tmp_residue_after_save() {
        let dir = scratch("noresidue");
        let path = dir.join("state.json");
        assert!(state(7, "yt:video:aaa").save_to(&path));
        assert!(state(9, "yt:video:bbb").save_to(&path));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["state.json", "state.json.bak"]);
        // The visible file is the latest save, intact.
        assert_eq!(
            json(&SavedState::load_from(&path)),
            json(&state(9, "yt:video:bbb"))
        );
    }

    #[test]
    fn concurrent_saves_never_leave_torn_state() {
        // The periodic (un-awaited) and quit (awaited) saves overlap on the
        // same file; unique temp names mean the winner is always exactly one
        // complete store, never a blend of both.
        let dir = scratch("concurrent");
        let path = dir.join("state.json");
        let a = state(1, "yt:video:aaa");
        let b = state(2, "yt:video:bbb");
        let writer_a = {
            let path = path.clone();
            let a = a.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    assert!(a.save_to(&path));
                }
            })
        };
        let writer_b = {
            let path = path.clone();
            let b = b.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    assert!(b.save_to(&path));
                }
            })
        };
        writer_a.join().unwrap();
        writer_b.join().unwrap();
        let final_json = json(&SavedState::load_from(&path));
        assert!(
            final_json == json(&a) || final_json == json(&b),
            "final file must be exactly one complete store"
        );
    }

    #[test]
    fn save_does_not_create_the_dir() {
        // F19: the parent is no longer created eagerly on save — a missing
        // cache dir makes save_to fail cleanly and is only recreated by the
        // caller's one-shot retry path (exercised in the e2e recipe).
        let dir = scratch("nodir");
        let doomed = dir.join("gone/deeper");
        let path = doomed.join("state.json");
        assert!(!state(7, "yt:video:aaa").save_to(&path));
        assert!(!doomed.exists());
    }
}
