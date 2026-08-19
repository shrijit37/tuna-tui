//! The publisher half of TXC: a Unix socket that fans one palette out to
//! however many subscribers happen to be listening — including none.
//!
//! [`mod@crate::txc`] defines *what* goes on the wire. This module owns *how*
//! it gets there, and every design decision below exists to protect one
//! invariant:
//!
//! > **Publishing color must never be able to stall playback.**
//!
//! Tuna TUI is a music player first. A subscriber is an untrusted, unprivileged
//! stranger that Tuna TUI never asked for and cannot audit; it may be a shell
//! script that connects and then blocks on a `read` forever. If a wedged
//! consumer could apply back-pressure to the audio/UI thread, TXC would be a
//! liability rather than a feature. So the publisher is *structurally*
//! incapable of blocking:
//!
//! - [`Publisher::publish`] only ever calls [`SyncSender::try_send`]. It has
//!   no blocking call in it at all — not on the socket, not on a channel.
//! - Every peer owns a **bounded** queue ([`PEER_QUEUE`] slots) and a
//!   dedicated writer thread. The writer thread is the only code that touches
//!   the socket, so all the blocking lives out there where it is harmless.
//! - A full queue is not something to wait on, it is a verdict: that
//!   subscriber is too slow, and it gets **dropped**. Lagging is the
//!   consumer's problem, and it is a detectable one — `seq` is
//!   per-connection, so a reconnecting client sees `seq` restart at `0` and
//!   knows it missed frames.
//!
//! ## Why a socket rather than a FIFO
//!
//! A FIFO has exactly one reader; a second `cat` steals bytes from the first.
//! `UnixListener` gives each subscriber an independent byte stream, which is
//! what makes fan-out — and therefore per-connection `seq` and
//! snapshot-on-connect — possible at all.
//!
//! ## Snapshot on connect
//!
//! A subscriber that starts mid-track must not have to wait for the next song
//! to paint itself. So the first line on every accepted connection is a
//! complete `theme` message with `seq: 0` carrying current state, published
//! from inside the same lock that registers the peer — so a `publish` racing
//! an `accept` can neither duplicate nor skip a frame for that peer.
//!
//! If nothing has been published yet there *is* no current state. The
//! connection is held open and silent rather than being fed a fabricated
//! palette: a consumer painting itself with a fake theme for one frame is a
//! visible bug, whereas a brief wait is invisible.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::theme::Theme;
use crate::txc::contrast::{is_dark, Contrast};
use crate::txc::wire::{ByeEvent, ByeReason, Colors, Message, Origin, ThemeEvent};
use crate::txc::{now_ms, PROTOCOL_VERSION};

/// Per-peer queue depth. A subscriber that falls this far behind is dropped.
///
/// 64 is chosen to be enormous relative to the real event rate — palettes
/// change on track boundaries, i.e. once every few minutes — so overflow does
/// not mean "briefly busy", it means "not reading at all". Making it larger
/// would only delay that diagnosis while buying a wedged consumer more of
/// Tuna TUI's memory.
const PEER_QUEUE: usize = 64;

/// Upper bound on how long a writer thread may sit in one socket write.
///
/// The kernel socket buffer absorbs a healthy consumer's worth of lag for
/// free, so hitting this means the peer has stopped draining entirely. Note
/// this timeout can never be observed by [`Publisher::publish`] — it lives on
/// the writer thread — it exists so that peer threads and [`Publisher::shutdown`]
/// terminate in bounded time instead of leaking a thread per zombie client.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the publisher hands a writer thread.
///
/// Deliberately *not* pre-serialized bytes: `seq` is per-connection, so only
/// the writer knows the right value. Shipping the shared [`ThemeEvent`] behind
/// an [`Arc`] means one palette build per publish regardless of how many
/// subscribers exist, with the cheap per-peer part (stamping `seq`, encoding)
/// done off the publishing thread.
enum Outbound {
    Theme(Arc<ThemeEvent>),
    Bye(ByeReason),
}

/// The publisher's view of one connected subscriber.
struct Peer {
    /// Identity for self-reaping — a writer thread removes *its own* entry on
    /// exit, and an index would be invalidated by other peers leaving.
    id: u64,
    tx: SyncSender<Outbound>,
    /// `None` once taken by [`Publisher::shutdown`] to be joined.
    handle: Option<JoinHandle<()>>,
}

/// Everything guarded by one lock.
///
/// Peers and last-published state share a single mutex on purpose: registering
/// a peer and reading the snapshot it must receive have to be atomic with
/// respect to `publish`, otherwise a connection landing mid-broadcast could
/// miss a frame or get it twice.
struct Inner {
    peers: Vec<Peer>,
    /// Most recent broadcast, replayed to every new connection. `None` until
    /// the first [`Publisher::publish`].
    last: Option<Arc<ThemeEvent>>,
    next_id: u64,
}

/// State shared between the owner, the accept thread, and every writer thread.
struct Shared {
    inner: Mutex<Inner>,
    /// Set by [`Publisher::shutdown`]; makes shutdown idempotent and tells the
    /// accept thread to stop taking new connections.
    closed: AtomicBool,
    path: PathBuf,
}

impl Shared {
    /// Lock without ever propagating poison.
    ///
    /// A panic elsewhere must not escalate into "Tuna TUI can no longer publish
    /// color", let alone into a second panic on the audio thread. The data
    /// behind this lock is a peer list and a palette — there is no invariant a
    /// poisoned guard could be protecting us from.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop a peer by id, e.g. when its writer thread has died.
    fn reap(&self, id: u64) {
        self.lock().peers.retain(|p| p.id != id);
    }
}

/// A running TXC publisher: one listening socket, one accept thread, and one
/// writer thread per subscriber.
///
/// Dropping it is equivalent to [`Publisher::shutdown`] with
/// [`ByeReason::Shutdown`], so the socket file cannot outlive the process that
/// created it under normal exit.
pub struct Publisher {
    shared: Arc<Shared>,
}

impl Publisher {
    /// Bind `path` and start accepting subscribers.
    ///
    /// Parent directories are created, and any file already sitting at `path`
    /// is unlinked first. That unlink is not optional: a Unix socket inode
    /// survives the process that made it, so after a crash (or a `SIGKILL`)
    /// the stale file would make every subsequent `bind` fail with
    /// `AddrInUse` — a one-time crash would otherwise disable color publishing
    /// permanently, with a manual `rm` as the only cure.
    pub fn bind(path: &Path) -> std::io::Result<Publisher> {
        ignore_sigpipe();

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Best-effort: `NotFound` is the normal case, and any other failure
        // will resurface as a clearer error from `bind` below.
        let _ = std::fs::remove_file(path);

        let listener = UnixListener::bind(path)?;

        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                peers: Vec::new(),
                last: None,
                next_id: 0,
            }),
            closed: AtomicBool::new(false),
            path: path.to_path_buf(),
        });

        let accept_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("txc-accept".into())
            .spawn(move || accept_loop(listener, accept_shared))?;

        Ok(Publisher { shared })
    }

    /// Broadcast `theme` to every current subscriber.
    ///
    /// Non-blocking by construction, and near-free when nobody is listening:
    /// with no peers there is nothing to serialize, so the cost is a palette
    /// copy and a comparison. Identical consecutive palettes are dropped
    /// entirely (see the dedupe note inline).
    ///
    /// The full [`ThemeEvent`] is assembled here rather than by the caller so
    /// that `v`, `ts`, `contrast` and `is_dark` are guaranteed consistent with
    /// `colors` — the whole point of shipping derived contrast is that exactly
    /// one implementation computes it.
    pub fn publish(&self, origin: Origin, theme: &Theme, fade_ms: u32) {
        if self.shared.closed.load(Ordering::Acquire) {
            return;
        }

        let colors = Colors::from(theme);
        let mut inner = self.shared.lock();

        // Dedupe on the palette itself, not on the origin: consumers exist to
        // render color, and re-emitting a byte-identical palette would make
        // every one of them redo a cross-fade for no visible change. A repeat
        // is common in practice — two tracks off the same album, or a manual
        // switch back to the theme already showing.
        if inner.last.as_ref().is_some_and(|l| l.colors == colors) {
            return;
        }

        let event = Arc::new(ThemeEvent {
            v: PROTOCOL_VERSION,
            // Placeholder: the authoritative value is stamped per connection
            // by each writer thread, because `seq` counts what *that*
            // subscriber received, not what Tuna TUI sent.
            seq: 0,
            ts: now_ms(),
            origin,
            fade_ms,
            is_dark: is_dark(theme.background),
            colors,
            contrast: Contrast::compute(&colors),
        });

        // Stored even with zero peers: this is what the next connection's
        // snapshot will be built from.
        inner.last = Some(Arc::clone(&event));

        // `try_send` only — a full queue means "this peer is not draining",
        // and the correct response is to disconnect it, never to wait.
        inner.peers.retain(|p| {
            match p.tx.try_send(Outbound::Theme(Arc::clone(&event))) {
                Ok(()) => true,
                // Full: too slow, evicted. Disconnected: its writer thread has
                // already died and is on its way to reaping itself. Both mean
                // "stop tracking this peer", and neither means "wait".
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Number of subscribers currently registered.
    ///
    /// A peer is removed on overflow or on the first failed write, so this can
    /// lag a client's disconnect by one publish — the kernel only reports a
    /// broken pipe once we actually try to use it.
    pub fn subscriber_count(&self) -> usize {
        self.shared.lock().peers.len()
    }

    /// Send `bye` to every peer, wait for it to reach the wire, then close.
    ///
    /// Idempotent — safe to call from both an explicit shutdown path and
    /// [`Drop`].
    ///
    /// The join is what makes `bye` meaningful. Handing the message to a
    /// writer thread and immediately returning would race process exit, and a
    /// `bye` that never leaves the buffer is worse than no `bye` at all: the
    /// consumer's reconnect logic can distinguish a clean goodbye from a
    /// crash, and it can only do that if the goodbye actually arrives. The
    /// join is bounded by [`WRITE_TIMEOUT`] because a wedged consumer must not
    /// be able to delay Tuna TUI's exit indefinitely either.
    pub fn shutdown(&self, reason: ByeReason) {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        // Unblock the accept thread by connecting to ourselves; it re-checks
        // `closed` after each accept and exits. Done *before* the unlink, so
        // the path still resolves.
        let _ = UnixStream::connect(&self.shared.path);
        let _ = std::fs::remove_file(&self.shared.path);

        // Take the peers out from under the lock before joining. A writer
        // thread reaps itself on exit, which needs this same lock — joining
        // while holding it would deadlock instantly.
        let peers = std::mem::take(&mut self.shared.lock().peers);

        for peer in &peers {
            // A failure here means the peer is already gone or hopelessly
            // backed up; either way it gets no goodbye, which is fine.
            let _ = peer.tx.try_send(Outbound::Bye(reason));
        }
        // Dropping every sender closes the channels, so each writer thread
        // sees `Disconnected` right after its `bye` and returns.
        for mut peer in peers {
            drop(peer.tx);
            if let Some(h) = peer.handle.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.shutdown(ByeReason::Shutdown);
    }
}

/// Accept subscribers until the publisher is closed.
///
/// Registration and snapshot delivery happen under one lock so that the
/// snapshot is exactly the state a concurrent `publish` either preceded or
/// followed — never a torn mix of the two.
fn accept_loop(listener: UnixListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.closed.load(Ordering::Acquire) {
            return;
        }
        let Ok(stream) = stream else {
            // Transient per-connection errors (EMFILE, ECONNABORTED) must not
            // kill the listener; a dead accept thread means color silently
            // stops working for the rest of the session.
            continue;
        };
        // Bounds every write on this peer, so a client that connects and never
        // reads costs one blocked thread for at most `WRITE_TIMEOUT`.
        if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
            continue;
        }

        let (tx, rx) = sync_channel::<Outbound>(PEER_QUEUE);

        let mut inner = shared.lock();
        let id = inner.next_id;
        inner.next_id += 1;

        // Snapshot first, before the peer is visible to `publish`, so it can
        // never be ordered after a live update. Silent when nothing has been
        // published yet — see the module docs.
        if let Some(last) = inner.last.as_ref() {
            // Fresh channel, capacity 64: this cannot fail.
            let _ = tx.try_send(Outbound::Theme(Arc::clone(last)));
        }

        let peer_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name(format!("txc-peer-{id}"))
            .spawn(move || {
                peer_loop(stream, rx);
                peer_shared.reap(id);
            });

        match handle {
            Ok(handle) => inner.peers.push(Peer {
                id,
                tx,
                handle: Some(handle),
            }),
            // Out of threads: drop the connection rather than registering a
            // peer nobody will ever service.
            Err(_) => continue,
        }
    }
}

/// One subscriber's writer thread: the only place that touches a socket.
///
/// Owns that connection's `seq` counter, which is why it serializes rather
/// than receiving finished bytes. Any write error — overwhelmingly
/// `BrokenPipe` from a client that just exited — is an ordinary disconnect,
/// so it returns and lets the caller reap the registration.
fn peer_loop(mut stream: UnixStream, rx: Receiver<Outbound>) {
    let mut seq: u64 = 0;
    while let Ok(item) = rx.recv() {
        let msg = match item {
            Outbound::Theme(event) => Message::Theme(ThemeEvent {
                seq,
                ..(*event).clone()
            }),
            Outbound::Bye(reason) => Message::Bye(ByeEvent {
                v: PROTOCOL_VERSION,
                seq,
                ts: now_ms(),
                reason,
            }),
        };
        let Ok(line) = msg.to_ndjson() else {
            // Unreachable for our own types; skipping beats poisoning the
            // stream with a half-written frame.
            continue;
        };
        // `flush` matters even though `UnixStream`'s is a no-op today: it is
        // the contract that the bytes are gone, and it keeps this correct if a
        // buffered writer is ever introduced here.
        if stream.write_all(line.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
        seq += 1;
