//! `txc_demo` — an external process that recolors itself live from TXC.
//!
//! This is the end-to-end proof of the Tuna TUI Color Protocol: a ratatui app that
//! knows *nothing* about Tuna TUI beyond the socket path and the wire types. It
//! connects, receives a full-state snapshot, and repaints every pixel it owns
//! from the published palette. Start a track in Tuna TUI and this window changes
//! color with it.
//!
//! ```text
//! cargo run --example txc_demo                 # $XDG_RUNTIME_DIR/tuna-tui/theme.sock
//! cargo run --example txc_demo /tmp/my.sock    # explicit path
//! cargo run --example txc_demo -- --fake       # no Tuna TUI required (see below)
//! ```
//!
//! ## What it is trying to demonstrate
//!
//! Each region on screen exists to falsify a specific claim in the spec:
//!
//! - the **swatch grid** proves all 16 tokens arrive, every time (full state,
//!   no deltas), and shows them rendered *in* their own color;
//! - the **mock widgets** prove the tokens are semantically usable — a real
//!   consumer wires `border`/`border_active`/`text`/`text_muted` and the four
//!   status roles straight into its own chrome, exactly as done here;
//! - the **contrast strip** proves the published `contrast.on_*` values are
//!   actually legible: the labels are drawn only in those colors, so if the
//!   publisher's WCAG math were wrong this strip would be the first thing to
//!   become unreadable;
//! - the **status line** proves the lifecycle rules — reconnect with backoff,
//!   and revert to our own default on `bye` rather than holding a stale album
//!   color forever.
//!
//! ## Architecture: one rule
//!
//! **The UI thread never touches the socket.** A background thread owns the
//! [`Subscriber`] and forwards everything down an [`mpsc`] channel. The render
//! loop only ever does a non-blocking `try_recv`, so a hung publisher, a slow
//! reconnect, or a five-second backoff sleep can never stall a keystroke or a
//! frame. That split is the reason this demo also works as a template: any
//! real consumer wants exactly this shape.
//!
//! The loop deliberately drives its own connect/backoff cycle out of
//! [`Subscriber::connect`] + [`Subscriber::next_message`] instead of calling
//! [`tuna_tui::txc::subscribe::watch`]. `watch` is the right default when you only
//! care about palettes, but it swallows the transitions — and here the
//! *transitions are the thing being demonstrated*, so we need to emit a link
//! state alongside each message.
//!
//! ## `--fake`
//!
//! `--fake` swaps the socket thread for a generator that emits a random-ish
//! palette every ~3s (and a `bye` every sixth one, to exercise the revert
//! rule). Nothing downstream changes: the generator pushes the same
//! [`Message`] values into the same channel, so the render path under `--fake`
//! is byte-for-byte the path a real socket drives. That makes the demo
//! verifiable with no Tuna TUI running.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::{Frame, Terminal};

use tuna_tui::anim::{ease_in_out_cubic, ThemeFade};
use tuna_tui::gradient::{lerp_color, Rgb};
use tuna_tui::theme::{Theme, TOKYONIGHT};
use tuna_tui::txc::contrast::Contrast;
use tuna_tui::txc::subscribe::Subscriber;
use tuna_tui::txc::wire::{ByeEvent, Colors, Hex, Message, Origin, OriginKind, ThemeEvent};
use tuna_tui::txc::{now_ms, socket_path, PROTOCOL_VERSION};

/// Frame budget while a cross-fade is running (~60fps).
const FRAME: Duration = Duration::from_millis(16);

/// Poll timeout when nothing is animating. Long enough to cost nothing, short
/// enough that a palette arriving on the channel is on screen within a blink.
const IDLE: Duration = Duration::from_millis(120);

/// First reconnect delay, mirroring `subscribe`'s own policy so the status
/// line reports what a real consumer would experience.
const BACKOFF_START: Duration = Duration::from_millis(100);

/// Ceiling on the reconnect delay.
const BACKOFF_CAP: Duration = Duration::from_secs(5);

/// How long the fake generator waits between palettes.
const FAKE_INTERVAL: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Source -> UI channel
// ---------------------------------------------------------------------------

/// How the subscriber thread currently relates to the publisher.
///
/// Carried as its own event rather than inferred from message gaps: "no
/// message for 10s" is indistinguishable from "nobody changed the track",
/// so only the thread that owns the socket can honestly report this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    /// Socket is open; the snapshot has been (or is about to be) delivered.
    Connected,
    /// Not connected. `attempt` counts consecutive failures and `delay` is how
    /// long we are about to sleep before the next try.
    Retrying { attempt: u32, delay: Duration },
    /// Synthetic source; there is no socket at all.
    Fake,
}

/// Everything the UI thread can be told by a message source.
#[derive(Debug)]
enum SourceEvent {
    /// A protocol message, verbatim.
    Message(Message),
    /// A link-state transition.
    Link(Link),
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// What the status line should say about the publisher.
#[derive(Debug, Clone, Copy)]
enum Conn {
    Link(Link),
    /// The publisher sent `bye`; we have reverted to our own default palette.
    /// Held until the next successful connect replaces it.
    Bye(tuna_tui::txc::ByeReason),
}

struct App {
    /// This demo's *own* palette, used before the first message and restored
    /// on `bye`. Owning a default is a protocol requirement: a consumer must
    /// never keep painting itself with a dead publisher's album color.
    default_theme: Theme,
    default_contrast: Contrast,

    /// The palette currently on screen — mid-fade while `fade` is running.
    displayed: Theme,
    displayed_contrast: Contrast,

    /// Active cross-fade, if any.
    fade: Option<ThemeFade>,
    /// `Contrast` is not part of `Theme`, so its four colors are interpolated
    /// alongside the fade using the same eased progress. Without this the
    /// legibility labels would pop a frame ahead of the surfaces behind them.
    contrast_from: Contrast,
    contrast_to: Contrast,

    conn: Conn,
    origin: Option<Origin>,
    seq: u64,
    is_dark: bool,
    /// The advisory `fade_ms` from the most recent theme event, echoed in the
    /// status line so it is visible that `0` really did snap.
    last_fade_ms: u32,
    /// Count of theme events applied, to make it obvious the stream is live
    /// even when two consecutive palettes look similar.
    applied: u64,
}

impl App {
    fn new(default_theme: Theme) -> App {
        let default_contrast = Contrast::compute(&Colors::from(&default_theme));
        App {
            default_theme,
            default_contrast,
            displayed: default_theme,
            displayed_contrast: default_contrast,
            fade: None,
            contrast_from: default_contrast,
            contrast_to: default_contrast,
            conn: Conn::Link(Link::Retrying {
                attempt: 0,
                delay: BACKOFF_START,
            }),
            origin: None,
            seq: 0,
            is_dark: tuna_tui::txc::contrast::is_dark(default_theme.background),
            last_fade_ms: 0,
            applied: 0,
        }
    }

    /// Begin (or skip) a transition to `theme`/`contrast`.
    ///
    /// `fade_ms` is *advisory* per the spec, and honoring it is the whole
    /// point of the field: `0` snaps, anything else cross-fades over exactly
    /// that wall-clock duration via [`ThemeFade`].
    fn transition_to(&mut self, theme: Theme, contrast: Contrast, fade_ms: u32) {
        self.last_fade_ms = fade_ms;
        if fade_ms == 0 {
            self.fade = None;
            self.displayed = theme;
            self.displayed_contrast = contrast;
            return;
        }
        self.fade = Some(ThemeFade::new(
            self.displayed,
            theme,
            Duration::from_millis(u64::from(fade_ms)),
        ));
        self.contrast_from = self.displayed_contrast;
        self.contrast_to = contrast;
    }

    /// Fold one protocol message into the UI state.
    fn apply(&mut self, msg: Message) {
        match msg {
            Message::Theme(ev) => {
                self.seq = ev.seq;
                self.is_dark = ev.is_dark;
                self.origin = Some(ev.origin.clone());
                self.applied += 1;
                self.conn = Conn::Link(Link::Connected);
                self.transition_to(theme_from(&ev.colors), ev.contrast, ev.fade_ms);
            }
            Message::Bye(ByeEvent { reason, seq, .. }) => {
                // The revert rule. The publisher is gone, so its album color is
                // no longer meaningful — falling back to our own identity is
                // both honest and visibly demonstrates the requirement.
                self.seq = seq;
                self.origin = None;
                self.conn = Conn::Bye(reason);
                self.is_dark = tuna_tui::txc::contrast::is_dark(self.default_theme.background);
                let (t, c) = (self.default_theme, self.default_contrast);
                self.transition_to(t, c, 400);
            }
        }
    }

    /// Advance the running fade. Returns `true` while animating, which is what
    /// selects the 60fps poll timeout over the cheap idle one.
    fn tick(&mut self) -> bool {
        let Some(fade) = &self.fade else {
            return false;
        };
        let eased = ease_in_out_cubic(fade.progress());
        self.displayed = fade.current();
        self.displayed_contrast = lerp_contrast(self.contrast_from, self.contrast_to, eased);
        if fade.is_done() {
            // Snap to the exact target so rounding in the lerp cannot leave the
            // final frame a channel or two off the published color.
            self.displayed = fade.target();
            self.displayed_contrast = self.contrast_to;
            self.fade = None;
            return false;
        }
        true
    }
}

/// Rebuild a [`Theme`] from wire [`Colors`].
///
/// `Theme::name` is `&'static str` and the wire has no equivalent field (the
/// human-readable name lives in [`Origin::name`], which the header renders
/// separately), so a fixed marker is used here.
fn theme_from(c: &Colors) -> Theme {
    Theme {
        name: "txc",
        primary: c.primary.into(),
        secondary: c.secondary.into(),
        accent: c.accent.into(),
        error: c.error.into(),
        warning: c.warning.into(),
        success: c.success.into(),
        info: c.info.into(),
        text: c.text.into(),
        text_muted: c.text_muted.into(),
        background: c.background.into(),
        background_panel: c.background_panel.into(),
        background_element: c.background_element.into(),
        border: c.border.into(),
        border_active: c.border_active.into(),
        border_subtle: c.border_subtle.into(),
        border_dimmest: c.border_dimmest.into(),
    }
}

/// Interpolate the contrast block, so `on_*` foregrounds travel with the
/// surfaces they sit on. See [`App::contrast_from`].
fn lerp_contrast(a: Contrast, b: Contrast, t: f32) -> Contrast {
    let m = |x: Hex, y: Hex| Hex(lerp_color(x.0, y.0, t));
    Contrast {
        on_primary: m(a.on_primary, b.on_primary),
        on_secondary: m(a.on_secondary, b.on_secondary),
        on_accent: m(a.on_accent, b.on_accent),
        on_background: m(a.on_background, b.on_background),
    }
}

// ---------------------------------------------------------------------------
// Message sources
// ---------------------------------------------------------------------------

/// Own the socket forever, forwarding messages and link transitions.
///
/// Runs on its own thread; every blocking call in the protocol lives inside
/// this function and nowhere else. Sends are best-effort: a send error means
/// the UI thread has exited, which is the thread's cue to stop.
fn socket_source(path: PathBuf, tx: Sender<SourceEvent>) {
    let mut backoff = BACKOFF_START;
    let mut attempt = 0u32;
    loop {
        match Subscriber::connect(&path) {
            Ok(mut sub) => {
                backoff = BACKOFF_START;
                attempt = 0;
                if tx.send(SourceEvent::Link(Link::Connected)).is_err() {
                    return;
                }
                // Drains until EOF or a broken link — both mean "reconnect".
                // A snapshot arrives first on every connect, so there is
                // nothing to resume and no missed-delta problem.
                while let Ok(Some(msg)) = sub.next_message() {
                    if tx.send(SourceEvent::Message(msg)).is_err() {
                        return;
                    }
                }
            }
            Err(_) => {
                // Not an error worth reporting as one: Tuna TUI simply may not be
                // running yet, and waiting for it is the expected behavior.
            }
        }
        attempt = attempt.saturating_add(1);
        if tx
            .send(SourceEvent::Link(Link::Retrying {
                attempt,
                delay: backoff,
            }))
            .is_err()
        {
            return;
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(BACKOFF_CAP);
    }
}

/// The `--fake` source: synthesize protocol messages on a timer.
///
/// Emits a fresh hue every [`FAKE_INTERVAL`], alternating dark and light
/// backgrounds so `is_dark` and the contrast strip both get exercised, and
/// slipping in a `bye` every sixth cycle to show the revert-to-default rule.
/// Every value goes through the real [`Message`] types, so the UI cannot tell
/// this apart from a socket.
fn fake_source(tx: Sender<SourceEvent>) {
    if tx.send(SourceEvent::Link(Link::Fake)).is_err() {
        return;
    }
    let mut rng = Xorshift::seeded();
    let mut seq = 0u64;
    let mut n = 0u32;
    loop {
        // A `bye` on the sixth cycle, then straight back to publishing —
        // exactly the shape of a Tuna TUI restart from the consumer's side.
        let msg = if n > 0 && n.is_multiple_of(6) {
            Message::Bye(ByeEvent {
                v: PROTOCOL_VERSION,
                seq,
                ts: now_ms(),
                reason: tuna_tui::txc::ByeReason::Reload,
            })
        } else {
            let dark = n.is_multiple_of(2);
            let colors = fake_palette(rng.next_f32() * 360.0, dark);
            Message::Theme(ThemeEvent {
                v: PROTOCOL_VERSION,
                seq,
                ts: now_ms(),
                origin: fake_origin(n),
                // Every fourth palette snaps, so `fade_ms: 0` is observably
                // honored and not just "a very fast fade".
                fade_ms: if n % 4 == 3 { 0 } else { 600 },
                is_dark: tuna_tui::txc::contrast::is_dark(colors.background.into()),
                contrast: Contrast::compute(&colors),
                colors,
            })
        };
        if tx.send(SourceEvent::Message(msg)).is_err() {
            return;
        }
        seq += 1;
        n += 1;
        std::thread::sleep(FAKE_INTERVAL);
    }
}

/// Plausible-looking provenance for the fake stream, cycling through all three
/// [`OriginKind`]s so the origin panel is exercised in each of its shapes.
fn fake_origin(n: u32) -> Origin {
    const TRACKS: &[(&str, &str, &str)] = &[
        ("Blue Monday", "New Order", "Power, Corruption & Lies"),
        ("Midnight City", "M83", "Hurry Up, We're Dreaming"),
        ("Teardrop", "Massive Attack", "Mezzanine"),
        ("Nightcall", "Kavinsky", "OutRun"),
    ];
    match n % 3 {
        0 => {
            let (track, artist, album) = TRACKS[(n as usize / 3) % TRACKS.len()];
            Origin {
                kind: OriginKind::AlbumArt,
                name: track.to_string(),
                track: Some(track.to_string()),
                artist: Some(artist.to_string()),
                album: Some(album.to_string()),
                track_id: Some(format!("yt:video:fake{n:04}")),
            }
        }
        1 => Origin::named(OriginKind::Builtin, "tokyonight"),
        _ => Origin::named(OriginKind::Fallback, "tuna-tui default"),
    }
}

/// Build a full 16-token palette around one hue, the way a real derivation
/// would: one accent family, three elevation layers, four border shades.
fn fake_palette(hue: f32, dark: bool) -> Colors {
    let h = |deg: f32, s: f32, l: f32| Hex(hsl(hue + deg, s, l));
    if dark {
        Colors {
            primary: h(0.0, 0.62, 0.62),
            secondary: h(35.0, 0.50, 0.56),
            accent: h(180.0, 0.68, 0.62),
            error: Hex(hsl(2.0, 0.62, 0.60)),
            warning: Hex(hsl(38.0, 0.68, 0.58)),
            success: Hex(hsl(142.0, 0.48, 0.56)),
            info: Hex(hsl(205.0, 0.62, 0.62)),
            text: h(0.0, 0.16, 0.92),
            text_muted: h(0.0, 0.14, 0.58),
            background: h(0.0, 0.30, 0.07),
            background_panel: h(0.0, 0.28, 0.11),
            background_element: h(0.0, 0.26, 0.16),
            border: h(0.0, 0.24, 0.24),
            border_active: h(0.0, 0.62, 0.62),
            border_subtle: h(0.0, 0.24, 0.17),
            border_dimmest: h(0.0, 0.22, 0.12),
        }
    } else {
        Colors {
            primary: h(0.0, 0.60, 0.38),
            secondary: h(35.0, 0.48, 0.42),
            accent: h(180.0, 0.60, 0.36),
            error: Hex(hsl(2.0, 0.60, 0.42)),
            warning: Hex(hsl(38.0, 0.62, 0.36)),
            success: Hex(hsl(142.0, 0.48, 0.32)),
            info: Hex(hsl(205.0, 0.58, 0.38)),
            text: h(0.0, 0.28, 0.12),
            text_muted: h(0.0, 0.16, 0.42),
            background: h(0.0, 0.36, 0.95),
            background_panel: h(0.0, 0.34, 0.90),
            background_element: h(0.0, 0.32, 0.84),
            border: h(0.0, 0.26, 0.72),
            border_active: h(0.0, 0.60, 0.38),
            border_subtle: h(0.0, 0.26, 0.80),
            border_dimmest: h(0.0, 0.24, 0.86),
        }
    }
}

/// HSL -> sRGB. Hue in degrees (wrapped), saturation/lightness in `0..=1`.
///
/// Hand-rolled because the crate has no color-space dependency and this demo
/// must not add one; the fake generator is the only caller.
fn hsl(hue_deg: f32, s: f32, l: f32) -> Rgb {
    let h = hue_deg.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let q = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(q(r), q(g), q(b))
}

/// A three-line xorshift, seeded from the clock.
///
/// The `rand` crate is optional and gated behind the `streaming` feature, and
/// this example must build under `--features txc` alone — so the fake source
/// brings its own bits. Statistical quality is irrelevant here; "a different
/// hue each time" is the entire requirement.
struct Xorshift(u64);

impl Xorshift {
