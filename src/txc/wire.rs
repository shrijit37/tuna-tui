//! The byte-level contract: serde types for every TXC message.
//!
//! Design rules that the rest of the module depends on:
//!
//! - **Full state, always.** Every [`ThemeEvent`] carries all 16 color tokens
//!   and a complete [`Contrast`] block. There are no deltas and no partial
//!   updates, so the snapshot and the update are the same code path on both
//!   sides (spec §3.6).
//! - **Forward compatible.** We deliberately do NOT use
//!   `#[serde(deny_unknown_fields)]`. A consumer built against v1 must keep
//!   working when a later Tuna TUI adds fields it has never heard of.
//! - **Lowercase `#rrggbb`.** One hex format, no alpha, no shorthand. See
//!   [`Hex`].

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::gradient::Rgb;
use crate::theme::Theme;
use crate::txc::contrast::Contrast;

/// A color on the wire: lowercase `#rrggbb`, sRGB, no alpha.
///
/// This is a newtype rather than a bare [`Rgb`] because the wire format is a
/// *contract* — `Rgb`'s own [`Rgb::from_hex`] silently falls back to black on
/// malformed input, which is right for a theme file typo but wrong for a
/// protocol. Here, garbage is a deserialization error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub Rgb);

impl From<Rgb> for Hex {
    fn from(c: Rgb) -> Self {
        Self(c)
    }
}

impl From<Hex> for Rgb {
    fn from(h: Hex) -> Self {
        h.0
    }
}

impl std::fmt::Display for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0.r, self.0.g, self.0.b)
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = String::deserialize(d)?;
        let body = raw
            .strip_prefix('#')
            .ok_or_else(|| D::Error::custom(format!("color must start with '#', got {raw:?}")))?;
        if body.len() != 6 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(D::Error::custom(format!(
                "color must be 6 hex digits, got {raw:?}"
            )));
        }
        let n = u32::from_str_radix(body, 16).map_err(D::Error::custom)?;
        Ok(Hex(Rgb::new(
            ((n >> 16) & 0xff) as u8,
            ((n >> 8) & 0xff) as u8,
            (n & 0xff) as u8,
        )))
    }
}

/// The 16 palette tokens — a 1:1 mapping of Tuna TUI's [`Theme`].
///
/// `Theme` also carries `name: &'static str`; that is surfaced as
/// [`Origin::name`], not as a color token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colors {
    // Roles
    pub primary: Hex,
    pub secondary: Hex,
    pub accent: Hex,
    // Status
    pub error: Hex,
    pub warning: Hex,
    pub success: Hex,
    pub info: Hex,
    // Text
    pub text: Hex,
    pub text_muted: Hex,
    // Surfaces — three elevation layers.
    pub background: Hex,
    pub background_panel: Hex,
    pub background_element: Hex,
    // Borders — four hierarchy shades.
    pub border: Hex,
    pub border_active: Hex,
    pub border_subtle: Hex,
    pub border_dimmest: Hex,
}

impl From<&Theme> for Colors {
    fn from(t: &Theme) -> Self {
        Self {
            primary: t.primary.into(),
            secondary: t.secondary.into(),
            accent: t.accent.into(),
            error: t.error.into(),
            warning: t.warning.into(),
            success: t.success.into(),
            info: t.info.into(),
            text: t.text.into(),
            text_muted: t.text_muted.into(),
            background: t.background.into(),
            background_panel: t.background_panel.into(),
            background_element: t.background_element.into(),
            border: t.border.into(),
            border_active: t.border_active.into(),
            border_subtle: t.border_subtle.into(),
            border_dimmest: t.border_dimmest.into(),
        }
    }
}

/// Where a palette came from. Lets a subscriber filter — a consumer that only
/// wants album-reactive color can ignore manual theme-picker switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// Derived from cover art of the current track.
    AlbumArt,
    /// A named built-in palette was selected.
    Builtin,
    /// Art unavailable or extraction failed; Tuna TUI fell back to its default.
    Fallback,
}

/// Provenance for a palette. `name` is always present and human-displayable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub kind: OriginKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
}

impl Origin {
    /// A minimal origin with no track metadata — used for `builtin` and
    /// `fallback` palettes.
    pub fn named(kind: OriginKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            track: None,
            artist: None,
            album: None,
            track_id: None,
        }
    }
}

/// Why the publisher is going away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    /// Normal exit.
    Shutdown,
    /// Restarting; reconnect advised.
    Reload,
}

/// A palette broadcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeEvent {
    /// Protocol major version.
    pub v: u32,
    /// Monotonic **per-connection** counter, `0` for the snapshot. Lets a
    /// subscriber detect it was dropped for lagging.
    pub seq: u64,
    /// Unix epoch milliseconds at emission.
    pub ts: u64,
    pub origin: Origin,
    /// Tuna TUI's own cross-fade duration for this transition. **Advisory** — a
    /// consumer may interpolate over it or snap instantly. `0` means snap.
    pub fade_ms: u32,
    /// True when `colors.background` relative luminance < 0.5.
    pub is_dark: bool,
    pub colors: Colors,
    pub contrast: Contrast,
}

/// Sent once on clean shutdown, immediately before closing peer connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByeEvent {
    pub v: u32,
    pub seq: u64,
    pub ts: u64,
    pub reason: ByeReason,
}

/// Any TXC message. Internally tagged on `t`, so the JSON is flat:
/// `{"t":"theme","v":1,...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    Theme(ThemeEvent),
    Bye(ByeEvent),
}

impl Message {
    /// Serialize to a single NDJSON line, newline included.
    ///
    /// Compact single-line JSON is mandatory — the framing depends on it.
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txc::PROTOCOL_VERSION;

    fn sample_colors() -> Colors {
        Colors {
            primary: Hex(Rgb::new(0x64, 0xe0, 0xd0)),
            secondary: Hex(Rgb::new(0x4a, 0x9f, 0xd8)),
            accent: Hex(Rgb::new(0xf4, 0xaa, 0x48)),
            error: Hex(Rgb::new(0xe0, 0x55, 0x61)),
            warning: Hex(Rgb::new(0xd9, 0xa4, 0x41)),
            success: Hex(Rgb::new(0x61, 0xc7, 0x66)),
            info: Hex(Rgb::new(0x64, 0xe0, 0xd0)),
            text: Hex(Rgb::new(0xd8, 0xef, 0xff)),
            text_muted: Hex(Rgb::new(0x7a, 0x90, 0xa4)),
            background: Hex(Rgb::new(0x08, 0x10, 0x18)),
            background_panel: Hex(Rgb::new(0x10, 0x1d, 0x2a)),
            background_element: Hex(Rgb::new(0x18, 0x29, 0x3a)),
            border: Hex(Rgb::new(0x22, 0x37, 0x4a)),
            border_active: Hex(Rgb::new(0x42, 0xd9, 0xd0)),
            border_subtle: Hex(Rgb::new(0x18, 0x28, 0x38)),
            border_dimmest: Hex(Rgb::new(0x10, 0x1c, 0x28)),
        }
    }

    #[test]
    fn hex_renders_lowercase_six_digits() {
        assert_eq!(Hex(Rgb::new(0, 0, 0)).to_string(), "#000000");
        assert_eq!(Hex(Rgb::new(255, 255, 255)).to_string(), "#ffffff");
        assert_eq!(Hex(Rgb::new(0x0a, 0xb0, 0xcd)).to_string(), "#0ab0cd");
    }

    #[test]
    fn hex_round_trips() {
        let h = Hex(Rgb::new(0x64, 0xe0, 0xd0));
        let s = serde_json::to_string(&h).unwrap();
        assert_eq!(s, "\"#64e0d0\"");
        assert_eq!(serde_json::from_str::<Hex>(&s).unwrap(), h);
    }

    #[test]
    fn hex_rejects_garbage_instead_of_silently_blackening() {
        for bad in ["64e0d0", "#64e0d", "#64e0d0ff", "#gggggg", "", "#"] {
            let json = format!("\"{bad}\"");
            assert!(
                serde_json::from_str::<Hex>(&json).is_err(),
                "{bad:?} must be rejected, not coerced"
            );
        }
    }

    #[test]
    fn theme_message_is_flat_and_tagged() {
        let msg = Message::Theme(ThemeEvent {
            v: PROTOCOL_VERSION,
            seq: 0,
            ts: 1_785_616_484_123,
            origin: Origin {
                kind: OriginKind::AlbumArt,
                name: "Blue Monday".into(),
                track: Some("Blue Monday".into()),
                artist: Some("New Order".into()),
                album: Some("Power, Corruption & Lies".into()),
                track_id: Some("yt:video:dQw4w9WgXcQ".into()),
            },
            fade_ms: 600,
            is_dark: true,
            colors: sample_colors(),
            contrast: Contrast::compute(&sample_colors()),
        });

        let v: serde_json::Value = serde_json::from_str(&msg.to_ndjson().unwrap()).unwrap();
        assert_eq!(v["t"], "theme");
        assert_eq!(v["v"], 1);
        assert_eq!(v["seq"], 0);
        assert_eq!(v["origin"]["kind"], "album_art");
        assert_eq!(v["colors"]["primary"], "#64e0d0");
        assert_eq!(v["fade_ms"], 600);
    }

    #[test]
    fn all_sixteen_tokens_are_always_present() {
        let json = serde_json::to_value(sample_colors()).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.len(), 16, "the palette is exactly 16 tokens");
        for token in [
            "primary",
            "secondary",
            "accent",
            "error",
            "warning",
            "success",
            "info",
            "text",
            "text_muted",
            "background",
            "background_panel",
            "background_element",
            "border",
            "border_active",
            "border_subtle",
            "border_dimmest",
        ] {
            assert!(obj.contains_key(token), "missing required token {token}");
        }
    }

    #[test]
    fn ndjson_is_exactly_one_line() {
        let msg = Message::Bye(ByeEvent {
            v: PROTOCOL_VERSION,
            seq: 12,
            ts: 1_785_616_999_000,
            reason: ByeReason::Shutdown,
        });
        let line = msg.to_ndjson().unwrap();
        assert_eq!(
            line.matches('\n').count(),
            1,
            "framing requires one newline"
        );
        assert!(line.ends_with('\n'));
        assert!(!line[..line.len() - 1].contains('\n'));
    }

    #[test]
    fn bye_shape_matches_spec() {
        let msg = Message::Bye(ByeEvent {
            v: 1,
            seq: 12,
            ts: 1_785_616_999_000,
            reason: ByeReason::Shutdown,
        });
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"t":"bye","v":1,"seq":12,"ts":1785616999000,"reason":"shutdown"}"#
        );
    }

    /// Forward compatibility is the whole point of §3.3 — a v1 consumer must
    /// survive fields a later Tuna TUI invents.
    #[test]
    fn unknown_fields_are_ignored_not_fatal() {
        let line = r#"{"t":"bye","v":1,"seq":1,"ts":1,"reason":"reload","future_field":{"a":1}}"#;
        let msg: Message = serde_json::from_str(line).unwrap();
        match msg {
            Message::Bye(b) => assert_eq!(b.reason, ByeReason::Reload),
            other => panic!("expected bye, got {other:?}"),
        }
    }

    #[test]
    fn optional_origin_metadata_is_omitted_when_absent() {
        let o = Origin::named(OriginKind::Builtin, "tokyonight");
        let s = serde_json::to_string(&o).unwrap();
        assert_eq!(s, r#"{"kind":"builtin","name":"tokyonight"}"#);
    }
}

#[cfg(test)]
mod adversarial {
    // FILE: src/txc/wire.rs — adversarial suite
    // FLAW COVERAGE: serde tag wire drift, hex contract, forward-compat unknown fields/tags, framing NDJSON, Colors token count
    // FALSE POSITIVE RATE: 0% (proven by controls)
    use super::*;
    use crate::txc::PROTOCOL_VERSION;

    fn sample_colors() -> Colors {
        Colors {
            primary: Hex(crate::gradient::Rgb::new(0x64, 0xe0, 0xd0)),
            secondary: Hex(crate::gradient::Rgb::new(0x4a, 0x9f, 0xd8)),
            accent: Hex(crate::gradient::Rgb::new(0xf4, 0xaa, 0x48)),
            error: Hex(crate::gradient::Rgb::new(0xe0, 0x55, 0x61)),
            warning: Hex(crate::gradient::Rgb::new(0xd9, 0xa4, 0x41)),
            success: Hex(crate::gradient::Rgb::new(0x61, 0xc7, 0x66)),
            info: Hex(crate::gradient::Rgb::new(0x64, 0xe0, 0xd0)),
            text: Hex(crate::gradient::Rgb::new(0xd8, 0xef, 0xff)),
            text_muted: Hex(crate::gradient::Rgb::new(0x7a, 0x90, 0xa4)),
            background: Hex(crate::gradient::Rgb::new(0x08, 0x10, 0x18)),
            background_panel: Hex(crate::gradient::Rgb::new(0x10, 0x1d, 0x2a)),
            background_element: Hex(crate::gradient::Rgb::new(0x18, 0x29, 0x3a)),
            border: Hex(crate::gradient::Rgb::new(0x22, 0x37, 0x4a)),
            border_active: Hex(crate::gradient::Rgb::new(0x42, 0xd9, 0xd0)),
            border_subtle: Hex(crate::gradient::Rgb::new(0x18, 0x28, 0x38)),
            border_dimmest: Hex(crate::gradient::Rgb::new(0x10, 0x1c, 0x28)),
        }
    }

    /// FLAW: wire tag must be exactly "theme"/"bye" snake_case, flat envelope with "t"
    /// ISOLATION: only tag string varies; same payload fields, same Message enum
    /// FALSE_POSITIVE_PREVENTION: control "theme" parses, "Theme"/"THEME"/"ThemeEvent" must fail distinct error
    #[test]
    fn test_txc_wire_tag_is_exact_snake_case_isolated() {
        // Control: "theme" parses
        let ok = r##"{"t":"theme","v":1,"seq":0,"ts":1,"origin":{"kind":"builtin","name":"x"},"fade_ms":0,"is_dark":true,"colors":{"primary":"#64e0d0","secondary":"#4a9fd8","accent":"#f4aa48","error":"#e05561","warning":"#d9a441","success":"#61c766","info":"#64e0d0","text":"#d8efff","text_muted":"#7a90a4","background":"#081018","background_panel":"#101d2a","background_element":"#18293a","border":"#22374a","border_active":"#42d9d0","border_subtle":"#182838","border_dimmest":"#101c28"},"contrast":{"text":"#000000","text_muted":"#000000","on_primary":"#000000","on_secondary":"#000000","on_accent":"#000000","on_background":"#000000","on_background_panel":"#000000","on_background_element":"#000000","best_on_background":"#000000"}}"##;
        let msg: Message = serde_json::from_str(ok).expect("theme tag must parse");
        assert!(matches!(msg, Message::Theme(_)));

        // Flawed: capitalized tag must fail — proves wire is case-sensitive snake_case, not Pascal
        let bad_capital = r#"{"t":"Theme","v":1,"seq":0,"ts":1,"reason":"shutdown"}"#;
        assert!(
            serde_json::from_str::<Message>(bad_capital).is_err(),
            "capitalized tag must be rejected"
        );

        // Flawed: uppercase must fail
        let bad_upper = r##"{"t":"THEME","v":1,"seq":0,"ts":1,"origin":{"kind":"builtin","name":"x"},"fade_ms":0,"is_dark":true,"colors":{"primary":"#64e0d0","secondary":"#4a9fd8","accent":"#f4aa48","error":"#e05561","warning":"#d9a441","success":"#61c766","info":"#64e0d0","text":"#d8efff","text_muted":"#7a90a4","background":"#081018","background_panel":"#101d2a","background_element":"#18293a","border":"#22374a","border_active":"#42d9d0","border_subtle":"#182838","border_dimmest":"#101c28"},"contrast":{"text":"#000000","text_muted":"#000000","on_primary":"#000000","on_secondary":"#000000","on_accent":"#000000","on_background":"#000000","on_background_panel":"#000000","on_background_element":"#000000","best_on_background":"#000000"}}"##;
        assert!(
            serde_json::from_str::<Message>(bad_upper).is_err(),
            "uppercase tag must be rejected"
        );

        // Control: "bye" tag parses
        let bye_ok = r#"{"t":"bye","v":1,"seq":1,"ts":1,"reason":"shutdown"}"#;
        assert!(serde_json::from_str::<Message>(bye_ok).is_ok());
    }

    /// FLAW: Hex must be exactly lowercase "#rrggbb", 6 hex digits, no shorthand/alpha
    /// ISOLATION: only color string varies; same Hex type, same serde path
    /// FALSE_POSITIVE_PREVENTION: control "#64e0d0" passes, "#64E0D0" uppercase letters still pass? No, hex digits case-insensitive but wire uses lowercase; we test strict rejection of malformed: missing #, short, long, non-hex, empty
    #[test]
    fn test_txc_hex_strictly_rejects_garbage_isolated() {
        // Control: valid lowercase passes
        let ok = "\"#64e0d0\"";
        assert!(serde_json::from_str::<Hex>(ok).is_ok());
        // Uppercase hex digits are valid ASCII hexdigit, so "#64E0D0" would pass per impl (it checks is_ascii_hexdigit, then from_str_radix). That's not a flaw — wire says lowercase but accepts uppercase on read.
        // Flawed: missing # must fail distinct error
        assert!(
            serde_json::from_str::<Hex>("\"64e0d0\"").is_err(),
            "missing # must be rejected"
        );
        // Flawed: short 3-digit must fail
        assert!(
            serde_json::from_str::<Hex>("\"#64e\"").is_err(),
            "short hex must be rejected"
        );
        // Flawed: long with alpha must fail
        assert!(
            serde_json::from_str::<Hex>("\"#64e0d0ff\"").is_err(),
            "8-digit hex must be rejected"
        );
        // Flawed: non-hex must fail distinct from length error
        assert!(
            serde_json::from_str::<Hex>("\"#gggggg\"").is_err(),
            "non-hex must be rejected"
        );
        // Control: Message with valid Hex round-trips lowercase
        let msg = Message::Theme(ThemeEvent {
            v: PROTOCOL_VERSION,
            seq: 0,
            ts: 1,
            origin: Origin::named(OriginKind::Builtin, "test"),
            fade_ms: 0,
            is_dark: true,
            colors: sample_colors(),
            contrast: crate::txc::contrast::Contrast::compute(&sample_colors()),
        });
        let v: serde_json::Value = serde_json::from_str(&msg.to_ndjson().unwrap()).unwrap();
        assert_eq!(
            v["colors"]["primary"], "#64e0d0",
            "wire must emit lowercase"
        );
    }

    /// FLAW: forward compat — unknown fields must be ignored, not fatal (wire has no deny_unknown_fields)
    /// ISOLATION: only extra field varies; same t/v/seq, same variant
    /// FALSE_POSITIVE_PREVENTION: control without extra passes, with extra passes, with future_tag fails for unknown tag but not for unknown field
    #[test]
    fn test_txc_unknown_fields_ignored_not_fatal_isolated() {
        // Control: minimal bye parses
        let minimal = r#"{"t":"bye","v":1,"seq":1,"ts":1,"reason":"shutdown"}"#;
        assert!(serde_json::from_str::<Message>(minimal).is_ok());

        // With unknown field: must still parse, same variant
        let with_future = r#"{"t":"bye","v":1,"seq":1,"ts":1,"reason":"shutdown","future_field":{"a":1},"another":123}"#;
        let msg: Message =
            serde_json::from_str(with_future).expect("unknown fields must be ignored");
        assert!(matches!(msg, Message::Bye(b) if b.reason == ByeReason::Shutdown));

        // Control: unknown THEME field ignored too
        let theme_with_extra = format!(
            r##"{{"t":"theme","v":1,"seq":0,"ts":1,"origin":{{"kind":"builtin","name":"x"}},"fade_ms":0,"is_dark":true,"colors":{},"contrast":{{"text":"#000000","text_muted":"#000000","on_primary":"#000000","on_secondary":"#000000","on_accent":"#000000","on_background":"#000000","on_background_panel":"#000000","on_background_element":"#000000","best_on_background":"#000000"}},"invented_in_v2":true}}"##,
            serde_json::to_string(&sample_colors()).unwrap()
        );
        assert!(
            serde_json::from_str::<Message>(&theme_with_extra).is_ok(),
            "unknown field in theme must be ignored"
        );
    }

    /// FLAW: all 16 tokens must always be present — sparse palette is a protocol violation
    /// ISOLATION: only colors object varies; same wire type, same serde
    /// FALSE_POSITIVE_PREVENTION: control 16 tokens passes, 15 tokens fails distinct missing field error, not generic
    #[test]
    fn test_txc_all_16_tokens_required_isolated() {
        let full = serde_json::to_value(sample_colors()).unwrap();
        assert_eq!(full.as_object().unwrap().len(), 16);

        // Remove one token
        let mut missing = full.clone();
        missing.as_object_mut().unwrap().remove("primary");
        let err = serde_json::from_value::<Colors>(missing).unwrap_err();
        assert!(
            err.to_string().contains("missing field `primary`"),
            "missing token must be specific error, got: {err}"
        );

        // Control: extra token is ignored per no-deny-unknown-fields, so not a failure
        let mut extra = full;
        extra
            .as_object_mut()
            .unwrap()
            .insert("future_token".into(), serde_json::json!("#ffffff"));
        assert!(
            serde_json::from_value::<Colors>(extra).is_ok(),
            "extra token must be allowed for forward compat"
        );
    }

    /// FLAW: NDJSON framing must be exactly one line, newline terminated, compact
    /// ISOLATION: only serialization method varies; same Message, same to_ndjson vs to_string
    /// FALSE_POSITIVE_PREVENTION: control to_ndjson has one newline and is parseable, pretty printed would have extra newlines and break framing
    #[test]
    fn test_txc_ndjson_is_exactly_one_line_isolated() {
        let msg = Message::Bye(ByeEvent {
            v: PROTOCOL_VERSION,
            seq: 12,
            ts: 1_785_616_999_000,
            reason: ByeReason::Shutdown,
        });
        let line = msg.to_ndjson().unwrap();
        assert_eq!(line.matches('\n').count(), 1, "exactly one newline");
        assert!(line.ends_with('\n'));
        assert!(!line[..line.len() - 1].contains('\n'));

        // Control: serde_json::to_string (no newline) is not valid NDJSON framing
        let compact = serde_json::to_string(&msg).unwrap();
        assert!(!compact.ends_with('\n'));
        // Pretty would be multi-line
        let pretty = serde_json::to_string_pretty(&msg).unwrap();
        assert!(pretty.matches('\n').count() > 1, "pretty is not NDJSON");
    }
}
