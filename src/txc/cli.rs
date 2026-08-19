//! `tuna-tui theme` — the command-line face of TXC.
//!
//! TXC is only useful if reading it is trivial from a shell. A status bar, a
//! prompt, or a `sway` reload hook should not have to link Rust, learn the
//! wire format, or hand-roll NDJSON framing. So the player ships its own
//! subscriber:
//!
//! ```text
//! eval "$(tuna-tui theme get)"          # TUNA_PRIMARY, TUNA_ON_ACCENT, …
//! tuna-tui theme watch --format css     # stream :root {} blocks
//! tuna-tui theme watch --exec 'my-bar-reload'
//! ```
//!
//! ## Why this lives in the library, not in `main.rs`
//!
//! `main.rs` is the player: it starts the yt-dlp → ffmpeg engine and
//! takes over the terminal. None of that may happen for `tuna-tui theme` — the
//! whole point is a fast, scriptable read of a socket. `main.rs` therefore
//! only *dispatches* here (before any engine or auth work) and exits with the
//! code [`run`] returns. Keeping the implementation next to
//! [`subscribe`](crate::txc::subscribe) also means the protocol and its
//! reference consumer cannot drift apart.
//!
//! ## The two pieces of CLI hygiene that actually matter
//!
//! - **`SIGPIPE` is reset to its default.** Rust sets `SIGPIPE` to `SIG_IGN`
//!   for every binary, and [`publish`](crate::txc::publish) deliberately keeps
//!   it that way — a subscriber pressing Ctrl-C must never kill the music
//!   player. A CLI wants the exact opposite: `tuna-tui theme watch | head -1` should
//!   die quietly on the closed pipe like `cat` does, not print an I/O error.
//!   [`run`] flips the disposition back for this process only, which is safe
//!   precisely because the CLI path never binds a publisher.
//! - **stdout is flushed after every message.** stdout is block-buffered when
//!   it is a pipe, and palettes arrive minutes apart, so an unflushed
//!   `tuna-tui theme watch | while read …` looks like a hang for the length of a
//!   whole album.
//!
//! ## Formats
//!
//! [`Format::Sh`] is the complete view: all 16 palette tokens, the four
//! contrast values, and the envelope metadata a consumer needs to decide
//! whether it cares (`TUNA_ORIGIN_KIND`) and how to animate (`TUNA_FADE_MS`).
//! [`Format::Css`] and [`Format::Hex`] emit the 20 *colors* only — a `:root`
//! block has no sensible place for `fade_ms`, and a two-column table is worth
//! more to `cut`/`awk` when every row is a color. Anything that needs the full
//! envelope uses `sh` or [`Format::Json`].

use std::io::{self, Write};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::txc::subscribe::{self, Subscriber};
use crate::txc::wire::{Message, OriginKind, ThemeEvent};

/// Exit code for a usage error (bad flag, missing value, unknown subcommand).
/// Distinct from [`EXIT_RUNTIME`] so a script can tell "I typed it wrong" from
/// "Tuna TUI is not running".
const EXIT_USAGE: i32 = 2;

/// Exit code for a runtime failure — most often "no publisher is listening".
const EXIT_RUNTIME: i32 = 1;

/// How many `--exec` children may be running at once before updates start
/// being skipped.
///
/// See [`ExecRunner`] for why skipping is the right failure mode.
const MAX_INFLIGHT: usize = 4;

/// What to print for each message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `TUNA_PRIMARY='#64e0d0'` — safe to `eval`.
    Sh,
    /// `--tuna-primary: #64e0d0;` inside a `:root { }` block.
    Css,
    /// `<token>\t<hex>`, one per line.
    Hex,
    /// The message as NDJSON.
    Json,
}

/// Which subcommand was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Print the snapshot (the first message on the connection) and exit.
    Get,
    /// Stream every update, reconnecting forever.
    Watch,
}

/// A fully parsed `tuna-tui theme …` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub cmd: Cmd,
    pub format: Format,
    /// `None` means [`crate::txc::socket_path`]. Resolved late so the default
    /// is not baked into a parsed value that tests want to compare.
    pub socket: Option<PathBuf>,
    pub exec: Option<String>,
}

/// The parse outcome. `--help` is a successful *non-run*, not an error, so it
/// gets its own variant rather than being smuggled through `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    Help,
}

/// The `--help` / usage text. Also printed to stderr on a usage error.
pub const USAGE: &str = "\
tuna-tui theme <get|watch> [options]

  get      print the current palette (the snapshot) and exit
  watch    stream every palette update until killed

Options:
  --format <sh|css|hex|json>   output format (default: sh)
  --socket <path>              theme socket (default: $XDG_RUNTIME_DIR/tuna-tui/theme.sock)
  --exec <cmd>                 run `sh -c <cmd>` per update, TUNA_* exported
  -h, --help                   show this help
";

/// Parse the arguments *after* the `theme` subcommand word.
///
/// Pure and total: no I/O, no environment, no defaults resolved. That is what
/// makes it unit-testable, and the reason [`Args::socket`] stays an `Option`.
///
/// Both `--flag value` and `--flag=value` are accepted because both appear in
/// the wild, and rejecting one would be a papercut with no upside.
pub fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    let mut cmd: Option<Cmd> = None;
    let mut format = Format::Sh;
    let mut socket: Option<PathBuf> = None;
    let mut exec: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        // Split `--flag=value` once, up front, so each flag arm below only has
        // to deal with "is the value inline or in the next argv slot".
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };

        /// Take the value for `flag`, from `--flag=value` or the next slot.
        macro_rules! value {
            () => {
                match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        argv.get(i)
                            .cloned()
                            .ok_or_else(|| format!("{flag} needs a value"))?
                    }
                }
            };
        }

        match flag {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--format" => {
                let v = value!();
                format = match v.as_str() {
                    "sh" => Format::Sh,
                    "css" => Format::Css,
                    "hex" => Format::Hex,
                    "json" => Format::Json,
                    other => {
                        return Err(format!(
                            "unknown format {other:?} (expected sh, css, hex or json)"
                        ))
                    }
                };
            }
            "--socket" => socket = Some(PathBuf::from(value!())),
            "--exec" => exec = Some(value!()),
            "get" if cmd.is_none() => cmd = Some(Cmd::Get),
            "watch" if cmd.is_none() => cmd = Some(Cmd::Watch),
            other if other.starts_with('-') => return Err(format!("unknown option {other:?}")),
            other => return Err(format!("unexpected argument {other:?}")),
        }
        i += 1;
    }

    match cmd {
        Some(cmd) => Ok(Parsed::Run(Args {
            cmd,
            format,
            socket,
            exec,
        })),
        None => Err("expected a subcommand: get or watch".to_string()),
    }
}

/// The serialized name of an origin kind, matching the wire encoding.
///
/// Written as an exhaustive match rather than a serde round-trip so that
/// adding a variant to [`OriginKind`] is a compile error here instead of a
/// silent `unknown` in every shell script on the machine.
fn origin_kind_str(kind: OriginKind) -> &'static str {
    match kind {
        OriginKind::AlbumArt => "album_art",
        OriginKind::Builtin => "builtin",
        OriginKind::Fallback => "fallback",
    }
}

/// The 16 palette tokens followed by the four contrast tokens, in a fixed
/// order shared by every format.
///
/// One list, so `sh`, `css` and `hex` can never disagree about which tokens
/// exist or what they are called.
fn color_tokens(ev: &ThemeEvent) -> [(&'static str, String); 20] {
    let c = &ev.colors;
    let k = &ev.contrast;
    [
        ("primary", c.primary.to_string()),
        ("secondary", c.secondary.to_string()),
        ("accent", c.accent.to_string()),
        ("error", c.error.to_string()),
        ("warning", c.warning.to_string()),
        ("success", c.success.to_string()),
        ("info", c.info.to_string()),
        ("text", c.text.to_string()),
        ("text_muted", c.text_muted.to_string()),
        ("background", c.background.to_string()),
        ("background_panel", c.background_panel.to_string()),
        ("background_element", c.background_element.to_string()),
        ("border", c.border.to_string()),
        ("border_active", c.border_active.to_string()),
        ("border_subtle", c.border_subtle.to_string()),
        ("border_dimmest", c.border_dimmest.to_string()),
        ("on_primary", k.on_primary.to_string()),
        ("on_secondary", k.on_secondary.to_string()),
        ("on_accent", k.on_accent.to_string()),
        ("on_background", k.on_background.to_string()),
    ]
}

/// Every `TUNA_*` name/value pair for one palette: the 20 colors plus the
/// envelope metadata.
///
/// This is the single definition of the `sh` surface — [`format_sh`] renders
/// it and `--exec` exports it, so a script reading `$TUNA_FADE_MS` sees exactly
/// what `eval "$(tuna-tui theme get)"` would have set.
///
/// `is_dark` is rendered as `1`/`0` rather than `true`/`false` so the natural
/// shell test — `[ "$TUNA_IS_DARK" = 1 ]` — is also the correct one.
fn env_pairs(ev: &ThemeEvent) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = color_tokens(ev)
        .into_iter()
        .map(|(k, v)| (format!("TUNA_{}", k.to_uppercase()), v))
        .collect();
    out.push((
        "TUNA_IS_DARK".to_string(),
        if ev.is_dark { "1" } else { "0" }.to_string(),
    ));
    out.push((
        "TUNA_ORIGIN_KIND".to_string(),
        origin_kind_str(ev.origin.kind).to_string(),
    ));
    out.push(("TUNA_ORIGIN_NAME".to_string(), ev.origin.name.clone()));
    out.push(("TUNA_FADE_MS".to_string(), ev.fade_ms.to_string()));
    out
}

/// Wrap `s` in single quotes, POSIX-safely.
///
/// **This is a security boundary, not a formatting nicety.** `TUNA_ORIGIN_NAME`
/// is a track title straight out of YouTube metadata — attacker-influenced
/// text that a user is about to `eval`. Inside single quotes every byte is
/// literal except `'` itself, so the entire escape is: close the quote, emit a
/// backslash-escaped quote, reopen. `Don't Stop` becomes `'Don'\''t Stop'`.
fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// `TUNA_PRIMARY='#64e0d0'` lines, `eval`-safe. No trailing newline.
pub fn format_sh(ev: &ThemeEvent) -> String {
    env_pairs(ev)
        .into_iter()
        .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `:root { }` block of `--tuna-*` custom properties. No trailing newline.
pub fn format_css(ev: &ThemeEvent) -> String {
    let mut out = String::from(":root {\n");
    for (k, v) in color_tokens(ev) {
        out.push_str(&format!("  --tuna-{}: {v};\n", k.replace('_', "-")));
    }
    out.push('}');
    out
}

/// `<token>\t<hex>` lines. No trailing newline.
pub fn format_hex(ev: &ThemeEvent) -> String {
    color_tokens(ev)
        .into_iter()
        .map(|(k, v)| format!("{k}\t{v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one message in `format`, without a trailing newline.
///
/// A `bye` has no palette, so in the three text formats it becomes a comment
/// in that format's own comment syntax: a consumer piping into `eval`, a CSS
/// file, or `awk` keeps working, and a human tailing the stream still sees
/// that the publisher went away.
///
/// `json` is the message re-encoded with [`Message::to_ndjson`] — the same
/// encoder the publisher uses, so for any message this build understands the
/// line is byte-identical to what came off the socket. (Fields invented by a
/// future Tuna TUI are dropped, because the subscriber parsed them away long before
/// this function ran; use a newer client if you need them.)
pub fn format_message(msg: &Message, format: Format) -> String {
    match (msg, format) {
        (_, Format::Json) => msg
            .to_ndjson()
            .unwrap_or_default()
            .trim_end_matches('\n')
            .to_string(),
        (Message::Theme(ev), Format::Sh) => format_sh(ev),
        (Message::Theme(ev), Format::Css) => format_css(ev),
        (Message::Theme(ev), Format::Hex) => format_hex(ev),
        (Message::Bye(b), Format::Css) => {
            format!("/* tuna-tui: publisher going away ({:?}) */", b.reason)
        }
        (Message::Bye(b), Format::Sh | Format::Hex) => {
            format!("# tuna-tui: publisher going away ({:?})", b.reason)
        }
    }
}

/// Fire-and-forget runner for `--exec`.
///
/// **Chosen behavior: never wait during `watch`.** The command is spawned with
/// the `sh`-format values in its environment and is not awaited, because
/// blocking the reader would let a slow hook stall the stream and eventually
/// get this subscriber evicted by the publisher for lagging (see
/// `publish::PEER_QUEUE`).
///
/// Not waiting at all would leak zombies, so finished children are reaped with
/// a non-blocking `try_wait` on each update. If [`MAX_INFLIGHT`] children are
/// still running the update is *skipped* with a warning rather than piling on:
/// a hook that cannot keep up with track changes is misconfigured, and
/// unbounded fan-out would turn that into a fork bomb.
///
/// `get` is the exception — it waits, since the process is about to exit and
/// an orphaned child would be killed mid-run.
struct ExecRunner {
    cmd: String,
    inflight: Vec<Child>,
}

impl ExecRunner {
    fn new(cmd: String) -> Self {
        Self {
            cmd,
            inflight: Vec::new(),
        }
    }

    fn run(&mut self, pairs: &[(String, String)], wait: bool) {
        // Reap first: this is what keeps `inflight` an accurate count rather
        // than a list of zombies.
        self.inflight
            .retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_)) | Err(_)));
        if !wait && self.inflight.len() >= MAX_INFLIGHT {
            eprintln!(
                "tuna-tui theme: --exec still busy ({MAX_INFLIGHT} running); skipping update"
            );
            return;
        }

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&self.cmd)
            // The child inherits our stdout on purpose: `--exec 'echo $TUNA_PRIMARY'`
            // should print where the user is looking. stdin is closed so a hook
            // that reads by accident fails fast instead of stealing the terminal.
            .stdin(Stdio::null());
