//! The context actions overlay: what an entry does and what's on the menu.

/// What an action-menu entry does when activated.
///
/// The library-write variants carry the display triple (name/subtitle) so the
/// local store can capture a row without a network round-trip — the old `id`
/// fields went away with the Spotify API.
#[derive(Clone)]
pub(crate) enum ActionKind {
    ToggleLike {
        uri: String,
        name: String,
        subtitle: String,
    },
    StartRadio {
        uri: String,
        name: String,
    },
    Queue {
        uri: String,
    },
    AddToPlaylistMenu {
        track_uri: String,
        track_name: String,
        track_subtitle: String,
    },
    AddToPlaylist {
        playlist_uri: String,
        track: crate::app::LibEntry,
    },
    ToggleFollowArtist {
        uri: String,
        name: String,
    },
    ToggleSaveAlbum {
        uri: String,
        name: String,
        subtitle: String,
    },
    FollowPlaylist {
        uri: String,
        name: String,
        subtitle: String,
    },
    Play {
        uri: String,
        /// Carried so the play path can set `source_name` — without it the
        /// Queue view's PLAYING FROM header and the persisted resume source
        /// go stale.
        name: String,
    },
    Open {
        uri: String,
        name: String,
    },
    CopyLink {
        uri: String,
    },
}

pub(crate) struct ActionItem {
    pub(crate) label: String,
    pub(crate) kind: ActionKind,
}

pub(crate) struct ActionMenu {
    pub(crate) title: String,
    pub(crate) items: Vec<ActionItem>,
    pub(crate) selected: usize,
}

impl ActionMenu {
    /// An empty menu for rows with no actions — the safe fallback for an
    /// unparseable or unknown-scheme uri.
    pub(crate) fn empty() -> Self {
        Self {
            title: String::new(),
            items: Vec::new(),
            selected: 0,
        }
    }
}
