//! The TXC subscriber: read NDJSON off the theme socket, forever.
//!
//! This is the consumer half of the protocol. It is deliberately the smallest
//! thing that is *correct*, because every bug this module avoids is one that
//! ships in a real client today.
//!
//! ## Why the framing is [`BufRead::read_line`] and nothing else
//!
//! The obvious implementation — `read()` into a `[u8; N]`, split on `\n` —
//! is wrong, and wrong in a way that only shows up under load. A socket read
//! returns *whatever bytes have arrived*, not a message. A `ThemeEvent` is
//! ~900 bytes of JSON; the moment the publisher's write is split across TCP-
//! or Unix-domain-buffer boundaries, the naive client sees half a JSON object,
//! fails to parse it, and drops a palette (or worse, resynchronizes on the
//! wrong newline). `hyprland-rs` has exactly this bug in its event client.
//!
//! [`BufRead::read_line`] is the fix and costs nothing: it owns the partial-
//! line buffer and blocks until a full line is available. Every read in this
//! module goes through it. See `message_split_across_two_writes_still_parses`.
//!
//! ## Why unknown messages are skipped, not fatal
//!
//! [`Message`] is a closed enum, so a future `{"t":"nowplaying",...}` line is
//! a hard deserialization error — which would kill the stream for a v1 client
//! that has no business caring about a message type invented after it shipped.
//!
//! Rather than pattern-match serde's error *text* (brittle, and it changes
//! between serde releases), this module pre-parses each line into a tiny
//! [`Envelope`] carrying only `t` and `v`. That single cheap pass answers both
//! forward-compatibility questions up front:
//!
//! - `t` is not one of [`KNOWN_TAGS`] → skip the line, keep reading.
//! - `v` is greater than [`PROTOCOL_VERSION`] → we are structurally unable to
//!   trust the payload, so surface an error instead of silently misreading it
//!   (spec: a major bump means a token was removed, renamed, or reframed).
//!
//! Unknown *fields* need no handling at all: [`wire`](crate::txc::wire)
//! intentionally omits `deny_unknown_fields`, and this module must never add
//! it.
//!
//! ## Why `watch` backs off
//!
//! Tuna TUI is a music player; it restarts, and it may not be running when the
//! consumer starts. A bar or prompt daemon that reconnects in a tight loop
//! burns a core for as long as Tuna TUI is closed. [`watch`] therefore retries with
//! capped exponential backoff — fast enough that a Tuna TUI restart is visually
//! instant, slow enough that "Tuna TUI is not installed" costs nothing.

use std::io::{self, BufRead, BufReader};
use std::ops::ControlFlow;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::txc::wire::Message;
use crate::txc::PROTOCOL_VERSION;

/// Every `t` value this build understands, i.e. the serialized names of
/// [`Message`]'s variants.
///
/// Kept as data rather than inferred from serde so the skip decision is made
/// *before* we attempt the real deserialization — see the module docs. The
/// test `known_tags_covers_every_message_variant` keeps it honest.
const KNOWN_TAGS: &[&str] = &["theme", "bye"];

/// First reconnect delay in [`watch`]. Short enough that a `tuna-tui` restart looks
/// instantaneous to a status bar.
const BACKOFF_START: Duration = Duration::from_millis(100);

/// Ceiling on the reconnect delay. Bounds idle cost when Tuna TUI simply is not
/// running, without making a later start feel unresponsive.
const BACKOFF_CAP: Duration = Duration::from_secs(5);

/// The envelope fields common to every TXC message, and the only ones needed
/// to decide whether this line is ours to parse.
///
/// `v` defaults to `0` so a line missing it does not trip the version guard;
/// such a line simply fails the real [`Message`] parse and is skipped as
/// malformed, which is the correct treatment for a truncated writer.
#[derive(Deserialize)]
struct Envelope {
    t: String,
    #[serde(default)]
    v: u32,
}

/// A live connection to the Tuna TUI theme socket.
///
/// Single-shot by design: it does not reconnect. That policy belongs to the
/// caller, and [`watch`] implements the sensible default. Keeping `Subscriber`
/// dumb means a consumer with its own supervision (a systemd unit, an async
/// runtime) can drive it without fighting a retry loop it did not ask for.
#[derive(Debug)]
pub struct Subscriber {
    /// Owns the partial-line buffer. The whole point of this type.
    reader: BufReader<UnixStream>,
    /// Scratch line buffer, reused across reads to avoid a per-message alloc.
    line: String,
    /// Latched once the stream is unusable (version skew), so the [`Iterator`]
    /// impl reports the error exactly once and then terminates instead of
    /// re-erroring forever.
    poisoned: bool,
}

impl Subscriber {
    /// Connect to the publisher at `path`.
    ///
    /// Fails immediately — no retry, no waiting — if nothing is listening.
    /// A missing socket is [`io::ErrorKind::NotFound`]; a stale socket file
    /// left by a crashed publisher is [`io::ErrorKind::ConnectionRefused`].
    /// Both are normal and both are the caller's (or [`watch`]'s) business.
    pub fn connect(path: &Path) -> io::Result<Subscriber> {
        let stream = UnixStream::connect(path)?;
        Ok(Subscriber {
            reader: BufReader::new(stream),
            line: String::new(),
            poisoned: false,
        })
    }

    /// Block until the next message the caller can understand.
    ///
    /// Returns `Ok(None)` on clean EOF (the publisher closed the connection).
    ///
    /// Lines that are blank, malformed JSON, or tagged with an unrecognized
    /// `t` are skipped silently and reading continues — none of those are
    /// worth dropping a working connection over. The one non-EOF failure is
    /// protocol version skew, which is fatal for this connection by design:
    /// a higher major version means the payload may no longer mean what this
    /// build thinks it means, and guessing is worse than reporting.
    pub fn next_message(&mut self) -> io::Result<Option<Message>> {
        if self.poisoned {
            return Ok(None);
        }
        loop {
            self.line.clear();
            // read_line, not read() — see module docs. Returning 0 is the only
            // signal for a clean, mid-stream-free EOF.
            if self.reader.read_line(&mut self.line)? == 0 {
                return Ok(None);
            }

            // Tolerate `\r\n` and keep-alive blank lines.
            let raw = self.line.trim();
            if raw.is_empty() {
                continue;
            }

            // Pass 1: envelope only. Unknown tags and version skew are decided
            // here so serde's closed-enum error never has to be interpreted.
            let Ok(env) = serde_json::from_str::<Envelope>(raw) else {
                continue; // not even a TXC-shaped object; skip.
            };
            if env.v > PROTOCOL_VERSION {
                self.poisoned = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "TXC protocol version {} is newer than this client's v{PROTOCOL_VERSION}; \
                         refusing to guess at message {:?}",
                        env.v, env.t
                    ),
                ));
            }
            if !KNOWN_TAGS.contains(&env.t.as_str()) {
                continue; // a message type invented after we shipped.
            }

            // Pass 2: the real thing. A known tag with an unusable payload is
            // a publisher bug, not a reason to disconnect; skip it too.
            match serde_json::from_str::<Message>(raw) {
                Ok(msg) => return Ok(Some(msg)),
                Err(_) => continue,
            }
        }
    }
}

impl Iterator for Subscriber {
    type Item = io::Result<Message>;

    /// Yields until clean EOF. A version-skew error is yielded once and then
    /// the iterator ends, so `for msg in sub` cannot spin on a poisoned link.
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_message() {
            Ok(Some(msg)) => Some(Ok(msg)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// Connect, stream every message to `on_message`, and reconnect forever.
///
/// This is the loop essentially every consumer wants: a status bar does not
/// care that Tuna TUI restarted, it cares about the current palette. Because TXC
/// sends full state as a snapshot on connect, reconnecting is *sufficient* —
/// there is no resume protocol and no missed-delta problem to solve.
///
/// Every disconnect reason (EOF, publisher `bye`, socket error, version skew)
/// funnels into the same capped exponential backoff, starting at
/// [`BACKOFF_START`] and doubling to [`BACKOFF_CAP`]. The delay resets on each
/// successful connection so a flapping publisher does not permanently degrade
/// to the ceiling.
///
/// Returns `Ok(())` only when `on_message` returns [`ControlFlow::Break`].
/// The [`io::Result`] return type is kept for the caller's convenience and for
/// room to grow a fatal-error path without a breaking signature change.
pub fn watch<F>(path: &Path, mut on_message: F) -> io::Result<()>
where
    F: FnMut(Message) -> ControlFlow<()>,
{
    let mut backoff = BACKOFF_START;
    loop {
        // A failed connect is not an error here: the publisher may simply not
        // be up yet, and waiting for it is the entire contract of `watch`.
        if let Ok(mut sub) = Subscriber::connect(path) {
            backoff = BACKOFF_START;
            // Drains until EOF or a broken connection — both mean "reconnect",
            // which is why every non-message outcome falls out of this loop.
            while let Ok(Some(msg)) = sub.next_message() {
                if on_message(msg).is_break() {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = crate::util::backoff_step(backoff, BACKOFF_CAP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradient::Rgb;
    use crate::txc::contrast::Contrast;
    use crate::txc::wire::{ByeEvent, ByeReason, Colors, Hex, Origin, OriginKind, ThemeEvent};
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    /// Generous by design: these tests must not be timing-flaky on a loaded
    /// CI box. Nothing waits this long in the happy path.
    const GENEROUS: Duration = Duration::from_secs(10);

    /// Unique socket paths within a single test binary run.
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A short, unique, absolute socket path.
    ///
    /// `sun_path` is ~108 bytes, so this stays terse on purpose — a long
    /// `TMPDIR` plus a verbose name silently truncates and produces baffling
    /// "No such file or directory" binds.
    fn temp_sock() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("txcs{pid}-{n}.sock"))
    }

    /// A raw NDJSON publisher stand-in.
    ///
    /// Deliberately *not* built on `txc::publish` — this fixture must be able
    /// to emit bytes a correct publisher never would (future tags, malformed
    /// JSON, half a message) and must not couple these tests to another
    /// module's schedule.
    struct Fixture {
        path: PathBuf,
        server: Option<JoinHandle<()>>,
    }

    impl Fixture {
        /// Bind the listener *synchronously* so the socket exists before this
        /// returns; the writer thread then runs `serve` on each accepted
        /// connection. This ordering is what lets tests connect without a
        /// "wait for the server to come up" sleep.
        fn spawn<F>(conns: usize, serve: F) -> Fixture
        where
            F: Fn(usize, &mut UnixStream) + Send + 'static,
        {
            let path = temp_sock();
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("bind test socket");
            let server = std::thread::spawn(move || {
                for i in 0..conns {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            serve(i, &mut stream);
                            let _ = stream.flush();
                            // Dropping `stream` here is the clean EOF / the
                            // mid-test disconnect, depending on the test.
                        }
                        Err(_) => return,
                    }
                }
            });
