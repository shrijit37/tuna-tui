//! Unit tests for the hand-rolled player argv scan ([`parse_player_args_from`]).

use crate::parse_player_args_from;

/// Drive the parser with a fixed argv (no `argv[0]` — we start at arg 1).
fn p(args: &[&str]) -> (Vec<String>, Option<u8>) {
    parse_player_args_from(args.iter().map(|s| s.to_string()))
}

#[test]
fn space_separated_value_is_stripped_uri_survives() {
    let (rest, buffer) = p(&["--buffer-duration", "7", "yt:track:abc"]);
    assert_eq!(buffer, Some(7));
    assert_eq!(rest, vec!["yt:track:abc"]);
}

#[test]
fn inline_value_is_stripped_regardless_of_flag_position() {
    let (rest, buffer) = p(&["yt:track:abc", "--buffer-duration=7"]);
    assert_eq!(buffer, Some(7));
    assert_eq!(rest, vec!["yt:track:abc"]);
}

#[test]
fn uri_after_a_valueless_flag_survives_as_the_positional() {
    // `tuna-tui --buffer-duration <url>`: the flag is missing its value and
    // the next argv is the startup URI. The URI is not a u8, so it must stay
    // a positional — the old code consumed and silently dropped it, booting
    // without the requested URI.
    let (rest, buffer) = p(&["--buffer-duration", "https://youtube.com/watch?v=abc123"]);
    assert_eq!(buffer, None, "no parseable value → config fallback");
    assert_eq!(rest, vec!["https://youtube.com/watch?v=abc123"]);
}

#[test]
fn non_numeric_space_separated_value_stays_a_positional() {
    // A typo'd value ("abc") is not consumed: it surfaces as a bad startup
    // argument instead of vanishing — recoverable, never a lockout.
    let (rest, buffer) = p(&["--buffer-duration", "abc"]);
    assert_eq!(buffer, None);
    assert_eq!(rest, vec!["abc"]);
}

#[test]
fn non_numeric_inline_value_falls_back_to_config() {
    // The `=` form has no ambiguity: the value slot exists, so an unparseable
    // one is dropped and the config file decides.
    let (rest, buffer) = p(&["--buffer-duration=abc"]);
    assert_eq!(buffer, None);
    assert_eq!(rest, Vec::<String>::new());
}

#[test]
fn flag_at_end_without_a_value_falls_back_to_config() {
    let (rest, buffer) = p(&["--buffer-duration"]);
    assert_eq!(buffer, None);
    assert_eq!(rest, Vec::<String>::new());
}

#[test]
fn theme_subcommand_survives_a_leading_flag() {
    let (rest, buffer) = p(&["--buffer-duration", "5", "theme", "set", "apocalypse"]);
    assert_eq!(buffer, Some(5));
    assert_eq!(rest, vec!["theme", "set", "apocalypse"]);
}

#[test]
fn out_of_range_value_falls_back_to_config() {
    // 300 does not fit a u8; the flag is consumed but the knob falls back.
    let (rest, buffer) = p(&["--buffer-duration", "300"]);
    assert_eq!(buffer, None);
    assert_eq!(rest, Vec::<String>::new());
}
