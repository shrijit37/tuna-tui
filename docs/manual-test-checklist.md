# Tuna TUI — manual pass checklist

Handoff for the **user manual pass on the landed build** (CLAUDE.md
"What's left"). This is a human-driven UAT pass over the streaming engine,
UI, and recovery paths that automated tests cannot cover: real YouTube
streams, real audio, real network conditions. Run it in a terminal on the
box where you actually listen (foot/kitty recommended — `Shift+Enter` needs
a CSI-u terminal).

## Prerequisites

* ( ) Binary from the server/CI build (`target/release/tuna-tui` — not a
      locally compiled one) — `--version` prints `tuna-tui 0.4.0`
* ( ) `yt-dlp` and `ffmpeg` on `PATH` (`yt-dlp --version`, `ffmpeg -version`)
* ( ) Working network; headphones/audio device connected (tests are audible)
* ( ) `~/.config/tuna-tui/` does not hold a stale `config.toml` (or accept
      defaults); if this is the first run after the rebrand, expect the
      one-time `myx → tuna-tui` migration (verify with `TUNA_LOG=1`)

## Pass items

Each item: do the steps, check the expected result, mark ✅/❌ in the box.
File a bead for any ❌ (repro steps + observed vs expected).

### 1. Search → play
* ( ) `/`, type a mainstream track name, `Enter` — results appear in the
      left pane (flat `ytsearchN:` list, no drill-in)
* ( ) `Enter` on a result → track starts; Now Playing shows title/artist,
      cover art fades in, theme recolors
* ( ) `Space` / `p` pause and resume; `n` / `b` next / previous
* ( ) `q` quits cleanly (or `Ctrl+C` twice)

### 2. Scrub / seek
* ( ) `Shift+←` / `Shift+→` seek ±5 s (expect ~1 s re-buffer — seek restarts
      the stream with `-ss`; the playhead display catches up from delivered
      frames)

### 3. Volume
* ( ) `+` / `=` and `-` / `_` step volume ±5; footer reflects it; audible
      change; no underruns at low volume (pTron/BT latency fix active)

### 4. Queue
* ( ) `→` opens the Queue view — it mirrors the playing list (track order,
      current highlight)
* ( ) `a` on a track → actions menu → "Add to queue" appends; Queue shows it
* ( ) `←` cycles back to Now Playing

### 5. Mid-track quit → resume
* ( ) Start a track, let it play ≥30 s, seek somewhere, quit with `q`
* ( ) Restart `tuna-tui` — same context resumes near the saved position
      (state.json seek restore; `~/.cache/tuna-tui/state.json` updated)

### 6. Visualizer + theme fade on a real track
* ( ) Spectrum visualizer animates in sync with music (fed from *served*
      samples, not delivered ones — should not freeze on buffering)
* ( ) Theme fade / cover-derived colors settle within a few seconds of
      track start

### 7. Lyrics on a real track
* ( ) Right pane → Lyrics view shows timed lyrics for a mainstream track
      (lrclib.net; known gap: exact-duration matching can miss — YouTube
      durations drift from releases. Record the track title if it misses —
      bead Myx-a4e.7)

### 8. Radio — mainstream and obscure seeds
* ( ) `a` → radio from a mainstream hit: stations start quickly
* ( ) Radio from a fresh/obscure track: expect the fallback chain
      (flat-extract → RDAMVM → search-built pseudo-radio); an *empty*
      station for a fresh seed is usually a bot gate, not a missing mix —
      note the seed and result
* ( ) Stop radio mid-chain (Esc/Esc or 20 s timeout) — no zombie playback
      fires later (Myx-3sm: orphaned chains must not spawn audio)

### 9. Drop recovery (the engine's real job)
* ( ) Play a track, kill the network (airplane mode / `wifi off`) for
      ~10–20 s, restore it — playback should recover (watchdog re-resolves,
      ~5 s poll with backoff) rather than dying; brief silence is expected,
      a dead UI is not

### 10. Daily drivers
* ( ) `Tab` / `Shift+Tab` (or `[`/`]`) rotate library sections; `o` cycles
      sort; `z` zen mode hides the library; `P` plays a highlighted
      playlist/album directly; `Shift+Enter` plays the selection outright
* ( ) Media keys (play/pause, next, prev) and MPRIS (`playerctl status`
      shows tuna-tui) — souvlaki surface
* ( ) Like/follow/save from the actions menu persists across restart
      (store in state.json)

## Outcome

- Summary: `N/M` passed; list failures and their beads.
- Note anything the checklist does not cover (new keys, layout changes).
- Close the manual-pass bead (Myx-a4e.6 track) or file follow-ups per the
  session protocol.
