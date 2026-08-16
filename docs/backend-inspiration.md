# Backend inspiration: DominatorMusic

Reading this created zero adopted code — everything below is a transferable idea,
checked against Tuna TUI's own constraints. The project itself is GPL-3.0, so any
actual code copying would be a licensing problem; these are *patterns*, noted so
a future phase can weigh each against our MIT + yt-dlp base.

**Source:** [DominatorMusic (DominatorStufs)](https://github.com/DominatorStufs/DominatorMusic)
— an Android music player streaming from YouTube Music, fork of Vitune. Inspected 2026-08-16.

## What it does, at a glance

- Playback from **YouTube Music** through *multiple swappable providers*:
  `providers/{innertube, piped, kugou, lrclib, sponsorblock, …}`.
- Lyrics from **lrclib.net** — the same lrclib Tuna TUI already uses.
- Radio via **watch-mix** (`RD` playlists) — the same radio concept Tuna TUI `YtExpander` uses.
- Offline cache, background playback, playlist import, mood/genre discovery.

## Ideas worth borrowing (each with Tuna TUI status)

### 1. Provider-as-trait (already done here)
They ship several transports behind one interface so a dead backend can be
swapped without touching the UI. Tuna TUI's `Expander` (`src/engine/expander.rs`)
is exactly this shape: `YtExpander` (yt-dlp today), and the deleted
`HybridExpander` proved a second implementation could coexist. **Status: done.**

### 2. Direct InnerTube as a yt-dlp alternative (a future option)
Their `providers/innertube` talks to `https://youtubei.googleapis.com/youtubei/v1/…`
directly — search, browse, player, next, with a `JavaScriptChallenge.kt` for the
signature (n-sig) dance yt-dlp does in-process.

Why it matters for Tuna TUI: the standing maintenance surface of the port is yt-dlp
breaking (po-token/bot checks, throttled streams). A direct-InnerTube transport
would drop the CLI + ffmpeg dependency and give typed responses. The cost is
real: the JS challenge solver is the kind of thing that breaks weekly, which is
precisely why we chose a mature binary instead. **Not now — but the Expander
trait means this is a *drop-in* later, not a rewrite.** The `cookies_file` +
retry path stays either way.

### 3. Radio from the *watch response's* radio playlist — **tested, not adoptable, superseded**
Their `NextPage` reads `playlistId` out of the video body (`watch?v=X` →
`RD…`) instead of *assuming* `RD<id>`. Tuna TUI **tested this live (2026-08-16) and
it does not apply to a yt-dlp-based app**: neither yt-dlp's `-J` watch output
nor the watch-page HTML carries the current video's own mix id (the HTML's
every `RD…` is a *related* video's `start_radio` command; the panel content is
only served by an innertube POST). The `RD<videoId>` convention itself is
deterministic and correct.

What the live probes found instead, and what was fixed (bead `Myx-a4e.7`):
- **The real radio defect was pagination, not the id**: an un-capped
  `--flat-playlist` on a mix walks 15+ sequential innertube pages (~20–27 s on
  a healthy network) while the station only keeps 50 rows — the app's 12 s
  deadline fired before a single row arrived. Fixed by capping the fetch to one
  inner-page (`--playlist-end 40`, ~3.7 s).
- **Second latent defect**: fresh/obscure seeds have no mix at all — or their
  player endpoint is bot-gated — so the mix candidates return nothing. Fixed
  with a fallback chain: `RD<id>` → `RDAMVM<id>` → a **search-built
  pseudo-radio** (find the seed's own row via the open search API, then flat-
  search its title). Everything in the fallback rides the search API, which is
  never gated where the player endpoint is.

### 4. lrclib best-match with a duration tolerance (small, high-value)
Their lrclib provider matches `bestMatchingFor` — a duration window around the
track, not an exact `duration=` param. YouTube rows are the *video's* length
(often ±2–10s vs the release), which is why Tuna TUI's exact-duration lrclib query
occasionally misses a synced lyric that clearly exists. Improvement: query lrclib
`/api/search` with the name, then pick the result whose `duration` is within
~10s of the video's, instead of `/api/get?duration=` exact.

### 5. SponsorBlock (a real feature gap)
Their `sponsorblock` provider skips intros/outros/sponsors mid-track. Our engine
is a plain stream with a seek command — segment skipping is *already playable*
once a sponsorblock lookup exists: on `TrackChanged`, fetch segments, and when
`PositionCorrection` crosses a segment boundary, issue `seek`. Behind a config
flag. **Filed as a bead (P3), not this phase.**

### 6. Search suggestions + genre/mood discovery (UI-adjacent)
`SearchSuggestions` (type-ahead) and `DiscoverPage` (mood/genre mixes). Tuna TUI's
flat `ytsearchN` search is deliberately minimal; suggestions would mean a
second autocomplete call per keystroke. **Suggestions filed as a P4 bead;
mood/genre discovery reviewed and skipped** (no first-class yt-dlp endpoint).

### 7. Full gap sweep (2026-08-16, pattern-only)
A structured review of the DominatorMusic codebase produced the complete
adoption list beyond the sections above — all filed as beads so the analysis
isn't lost: **skip-on-error auto-advance** (adopt-now, P2), **SponsorBlock**
(§5, P3), **search suggestions** (§6, P4), and a P4 backlog basket holding
playback speed/pitch (ffmpeg atempo), persistent queue across restarts,
loudness normalization (`normalizedLoudnessDb` → ffmpeg `volume` filter),
per-track blacklist, offline cache via `yt-dlp -x`, and YTM-innertube lyrics
as a second source + local lyric editing (pairs with the lrclib tolerance
work in bead `Myx-a4e.7`). Skipped after review: mood/genre discovery, Piped
account sync, Google Translate integration.

## What NOT to take

- The MultiLine "tabs/player/queue" UI architecture — Tuna TUI's UI is deliberately
  untouched and ratatui-shaped.
- Any actual code — GPL-3.0 provenance; patterns only.