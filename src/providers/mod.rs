//! Providers — the normalization seam between YouTube backends and the core.
//!
//! `contracts` holds the canonical DTOs; `ytdlp` is the one real adapter
//! today. Plain functions, no router: the types are the contract, and the
//! InnerTube client (Myx-mh7.1/.2) will slot in as a sibling producer of the
//! same shapes.

pub mod contracts;
pub mod ytdlp;
pub mod ytmusic;

pub use contracts::{AlbumRef, ArtistRef, AudioStream, PlaybackInfo, Song, Thumbnail};
