# Performance Audit — tuna-tui v0.4.0 (2026-08-17)

## Executive summary

At idle this app is genuinely lean: redraws are event-driven (2Hz idle gate, 16/33/500ms gates in src/app/frame.rs), channels are bounded or human-cadence, RSS is flat (21–58MB, no leak signal), and the ~543MB VmPeak is uncommitted glibc arena address space, not memory. Playback-path CPU measured ~4.5% max at 20fps; paused idle sits at 1.5–3.7%, dominated by the cpal device callback that no stream-teardown fix can reclaim. The real issues are robustness and redundancy, not hot-path waste: a non-atomic state.json write that a torn write turns into a silent permanent wipe of the whole library (F18), an unconditional 24s full-store clone+write (F21), uncancellable radio chains that keep spawning yt-dlp up to ~40s after the 20s UI timeout and can even fire zombie playback (F13), pause leaving ffmpeg + the googlevideo connection resident indefinitely (F30), and an uncached lrclib request per track (F12). A second cluster of per-frame UI waste (F1 scrollbar Paragraphs, F2 per-row allocations) sits entirely in the project-forbidden UI layer and is report-only. Nothing here blocks shipping; the highest-value fix (F18) is ~10 lines in persist.rs, which project rules explicitly carve out as editable.

## Method

Seven dimension finders audited the repo: UI render loop, engine/audio pipeline, network/yt-dlp, persistence, threads/memory, build/dependencies, plus a runtime probe of the built binary (ldd, /proc thread/RSS/CPU sampling across ~20 min, live ffmpeg/yt-dlp observation, TERM-shutdown checks). Every one of the 31 candidate findings was then verified twice and independently: an adversarial refuter (is the wastage real, is the magnitude right) and a regression-risk assessor (does the proposed fix break functionality/correctness/UX; both re-derived fixes against pinned dependency source). Findings stayed only with a verifiable mechanism; PARTIAL verdicts are flagged inline where a verifier corrected scope or magnitude, and the single impact-grounds refutation (F9) is disclosed in place. Severity, confidence, and regression risk in the headers are as reported.

## Baseline measurements

Runtime probe of `target/release/tuna-tui` on this box succeeded (two live instances; all numbers measured).

- Binary: 7.7MB, stripped, PIE. Direct shared libs: libssl.so.3, libcrypto.so.3, libasound.so.2, libgcc_s, libm, libc — exactly matching .deb/dist/flake packaging (libz/libbrotli*/libzstd in ldd are transitive from system OpenSSL, not Rust crates).
- Threads: 18–19, stable, 1:1 mapped to features (engine worker, watchdog, ffmpeg-pump, per-track tuna-meta, MPRIS zbus, txc-accept, cpal, reqwest client, 4 parked tokio workers, main, input poll); no churn.
- RSS: 30MB fresh / 58MB playing / 21MB paused; flat across a 40s playing soak (33,328→33,472kB) and all 5s windows; dropped 35,868→21,108kB after pause. VmPeak 543,296–560,624kB constant from first sample — virtual only (F31).
- CPU: max ~4.5% playing (20fps), 1.5–3.7% paused (2fps idle redraw): cpal_alsa_out ~1.0%, data-loop.0 ~0.8%, main 0.4%, tokio 0.4%; ffmpeg ~0.7% streaming, ~0.08% backpressured while paused. All sampled threads State S — no spin.
- Hygiene: kill -TERM exited both instances in ~10ms with the ffmpeg child reaped, zero orphan ffmpeg/yt-dlp after exit, lock released; a mid-track googlevideo drop recovered live via the watchdog without user action.

## Confirmed findings

Ordered by severity (medium → low → info), then by regression risk ascending (safest first).

### Medium

**F1 — Scrollbar drawn as one Paragraph widget per track cell, every frame** *(medium, high, low risk)*
- src/ui/library.rs:241 (loop 230–249)
- What happens: one `Paragraph::new(Span::styled(glyph, …))` per overflow row per drawn frame (~900 widget-render calls/s at 30fps in NowPlaying, grows with viewport height); siblings render_progress/visualizer use direct cell writes and call this exact pattern "pure overhead".
- Safe fix: replace only the drawing loop with `buf.cell_mut((sb_x, y))` + `set_symbol`/`set_fg`; leave thumb math (217–229) and `out.hits.scroll` (251–260) untouched. Pixel-identical per ratatui 0.30.2 cell semantics (Paragraph patches only fg; Cell::set_fg preserves bg/modifiers).
- Regression caution: low — must use `set_fg` (patch), never `set_bg` (partial-block glyphs show bg); keep the `y >= inner.bottom()` break and bounds check; drag-to-scroll (input/mouse.rs:20) is untouched.
- (UI-layer — report only, project rules forbid changes)

**F18 — state.json write is non-atomic — torn write silently resets the whole library** *(medium, high, medium risk)*
- src/app/persist.rs:227
- What happens: `std::fs::write` truncate+writes the live file, no temp/rename/fsync, error swallowed; load (214–219) maps any parse failure to `unwrap_or_default()`, and the next 24s save overwrites the corrupt file with an empty store — one mid-write interruption (kill -9, power loss) silently and permanently wipes liked/albums/artists/playlists/history. The file is rewritten thousands of times per day of use.
- Safe fix: write `state.json.tmp` then rename (POSIX atomic; Windows fallback remove+rename or NamedTempFile); unique temp names per save so the overlapping periodic (un-awaited) and quit (awaited) saves can never rename a torn temp; optional `sync_all` inside the existing spawn_blocking; on load distinguish NotFound (→default, first run) from parse error (log the reset via liblog; optionally recover a `state.json.bak` stale by at most one 24s cadence).
- Regression caution: medium — `std::fs::rename` fails over an existing destination on Windows (naive swap freezes persistence there); the load-side recovery is safe only if the missing-file first-run path still returns default.

**F21 — Full library store cloned + serialized + written every 24s on a timer, even when nothing changed** *(medium, high, low risk)*
- src/main.rs:641
- What happens: the SYNC_EVERY tick clones the entire Store (uncapped liked/albums/artists/playlists, history 100) plus transport on the UI thread (persist.rs:256) and rewrites identical state.json every 24s; at idle only position/volume legitimately change. Store mutations have exactly 3 call sites — the clone is provably redundant at idle.
- Safe fix: dirty gating — `app.store_dirty` set at the 3 mutator sites (actions.rs:196, 238; app/event.rs:134–136), transport-dirty computed as `playback_started || transport_changed_since_save` (playing keeps the cadence, so position_ms/resume persistence is bit-identical); skip the whole save when both clean; keep the two awaited quit saves unconditional. When dirty, write the exact current full snapshot as today.
- Regression caution: low — never "omit the store / reuse old bytes": load() has no merge and `#[serde(default)] store` would boot an empty library from a partial file; the skip must not suppress refresh_local_queue's label pipeline while playing.

**F6 — Detached tuna-meta thread per track start AND per recovery rebuild — duplicate cover/theme work per recovery** *(medium, high, medium risk)*
- src/engine/mod.rs:1012
- What happens: build_stream spawns a detached tuna-meta thread per track start and per successful recovery rebuild; each re-fetches (httpcache disk read), decodes, and re-themes the unchanged cover. Worse: a recovery re-delivery for the same uri passes `meta_is_current` and re-runs record_played (Home count inflation) + a fresh lrclib fetch. Skip-storm pile-up is realistically ~1–3 threads (serial resolve bounds it), contrary to "unbounded".
- Safe fix: one persistent tuna-meta worker owned by the engine, fed via bounded(16) FIFO channel; build_stream does non-blocking try_send, on Full drops the OLDEST job first (drop-oldest) so the current track's job always lands; preserve FIFO for the meta_is_current ordering guard.
- Regression caution: medium — a blocking send() on a saturated channel stalls the engine's command loop; drop-newest loses the current track's cover/lyrics (the guard cannot help — nothing arrives); optionally skip recovery re-delivery only with a `fresh: bool` threaded through build_stream.

**F13 — Radio fallback chain keeps spawning yt-dlp after the 20s UI timeout (uncancellable spawn_blocking); orphaned chains stack** *(medium, high, medium risk)*
- src/main.rs:783
- What happens: the 20s tokio timeout cannot cancel the spawn_blocking closure; an orphaned chain (up to 4 sequential children, 15s deadline each) keeps spawning Python for ~40s after "radio timed out". A slow-but-alive chain's late Ok is still delivered (tx is a clone, rx lives in the loop) and can trigger zombie playback up to ~40s later, racing stacked retries (radio_in_flight clears when the timeout error drains).
- Safe fix: per-request `Arc<AtomicBool>` set in the timeout Err branch, still sending the existing "timed out" Err (so the drain resets radio_in_flight exactly as today); pass into radio_entries/pseudo_radio (default never-set for other callers), poll it in yt_stdout's 50ms try_wait loop and kill+wait the child on cancellation; check between chain steps.
- Regression caution: medium — the flag must be per-request and reset at spawn_radio entry (a stale static leaks across calls and breaks the #[ignore]d live_radio_roundtrip); never short-circuit tx.send (radio_in_flight would stick true and block the next radio request forever); do not bake it into Engine/YtExpander.

**F3 — 24-second UI-thread sync re-formats every queue label and clones the whole store snapshot on the render thread** *(medium, medium, high risk)*
- src/main.rs:632
- What happens: every 24s the tick runs refresh_local_queue (engine queue deep-clone + `format!` per entry via track_label_of) + save_state (whole-Store clone, persist.rs:256) on the render thread. Verifiers corrected two overstatements: refresh is NOT per-track-change (exactly 4 call sites) and is NOT purely redundant — it is the only mechanism that upgrades raw-URI queue rows to "title — artist" as EngineMeta lands one track at a time, and the only re-sync after recovery-removal and resume-restore. (verification PARTIAL/PARTIAL)
- Safe fix (rewritten): keep all protected files untouched. Add `Engine::queue_len() -> usize` (lock + .len(), no clone) in engine; in the tick track last queue len + `meta_cache.len()` with a `usize::MAX` sentinel so the first tick after launch/resume always refreshes; skip refresh only when both unchanged; keep save_state + spawn_blocking every tick untouched.
- Regression caution: high — the original proposal (precompose labels in apply_meta/track_label_of) edits protected src/app/*; the naive len-gate freezes the Queue view at bare URIs during a long track and re-clones the whole list just to get the length.

**F12 — Lyrics fetched unconditionally on every track change with no cache (steady-state network waste)** *(medium, medium, high risk)*
- src/app/event.rs:150
- What happens: apply_meta spawns fetch_lyrics_blocking on every track change regardless of whether the Lyrics view is ever opened; fetch.rs:22–58 does a raw `client.get(&url).send()` — no httpcache, no memo, deterministic key, so repeated tracks re-fetch identical content. One lrclib roundtrip per track in normal queue/radio playback; the cover path is httpcache-cached, lyrics is the lone uncached per-track call.
- Safe fix (verifier-corrected): confine to lib src/lyrics/fetch.rs — session-scoped memo keyed on the exact lrclib URL (or the artist/title/album/duration tuple) behind a Mutex, or write-through httpcache with max_age; do NOT cache the empty no-match result indefinitely (short TTL or session scope preserves picking up newly-added lyrics).
- Regression caution: high — the lazy fetch-on-view-open half needs protected src/input/key.rs and src/ui/lyrics.rs plus a loading state (otherwise a transient false "no lyrics for this track", permanent on timeout) and a current-track guard on the lyrics channel; the safe subset only kills identical re-fetches, not the per-distinct-track request.
- (UI-layer — report only, project rules forbid changes)

**F14 — Un-capped playlist_entries in the drill-in view paginates entire playlists/channels** *(medium, medium, high risk)*
- src/browse.rs:157
- What happens: the drill-in passes search_limit but resolve_kind (yt/mod.rs:225–232) ignores it for playlist/channel; playlist_entries runs `--flat-playlist` with no `--playlist-end` (206–218), so yt-dlp paginates the whole list (bounded by the 15s yt_stdout kill deadline, not "minutes"; fetch runs on the detached tuna-detail thread, so no UI freeze). Same root cause as bead Myx-a4.8, but on the drill-in surface the bead does not name. (verification PARTIAL/PARTIAL)
- Safe fix (verifier-corrected): scope the cap to browse only — new `playlist_entries_capped(url, limit)` appending `--playlist-end` (mirroring radio_entries:147), used ONLY by the drill-in playlist/channel arms with a fixed `DRILLIN_FETCH_LIMIT = 200` — NOT `min(search_limit, …)` (search_limit defaults to 6); leave resolve_kind and the expand path un-capped.
- Regression caution: high — capping inside resolve_kind truncates the PLAY path (expander.rs:71 builds the queue from it), a core playback regression on the Myx-a4e.8 surface; `min(search_limit, 100-200)` would truncate even a 30-track playlist to ~6 rows with no truncation hint.

**F30 — Pause keeps the full stream pipeline alive: ffmpeg subprocess + googlevideo connection + cpal output, ~2-3% CPU while paused** *(medium, medium, high risk)*
- src/engine/mod.rs:608
- What happens: Cmd::Pause only pauses the rodio sink + flags; the ffmpeg child, its connection, and the pump stay resident for the whole pause (observed alive 10+ min after 'p', backpressured). Verifiers corrected the magnitude: the FFT feed already stops (rodio's pausable filter stops pulling the source → collapse to 2fps), and the dominant ~2% (cpal_alsa_out, data-loop.0) survives ANY stream teardown — only ~0.08% CPU + the TCP slot + RSS are reclaimable. (verification PARTIAL/PARTIAL)
- Safe fix (verifier-corrected): keep the device and rodio Player alive; on pause kill only the current stream and stash (uri, url, duration_ms, pos) in a dedicated `paused:` field (NOT `recovery` — run()'s pre-empt re-entry treats recovery as in-flight); on resume `restart_stream` from the SAME already-resolved URL (no network re-resolve), emit Playing, set health/active; record pending seek targets for scrub-while-paused.
- Regression caution: high — rebuild-on-resume makes the 'p'/space hot key network-dependent (1–2s, offline falls to recover_into→give_up_on track skip), rewinds the playhead up to ~1s (restart_stream truncates to whole seconds at mod.rs:952), breaks scrub-while-paused (seek_now early-returns when current is None) and can drop an advance if the done-signal lands during pause; do NOT "gate the FFT on active" (no-op) and do NOT drop the cpal device (device re-acquisition risk).

### Low

**F8 — Natural-EOF path can drop the ffmpeg Child without wait — zombie race** *(low, high, low risk)*
- src/engine/mod.rs:774
- What happens: track_ended calls try_wait once (744); if the child is mid-shutdown the natural path drops the Child with no kill/wait, and an exit microseconds later leaves a zombie until process end (no SIGCHLD reaper anywhere; std Child::Drop does not reap). Rare — the ~0.75s drain backlog usually lets ffmpeg exit first — worst case one zombie per natural track end.
- Safe fix: capture `let exited = cur.child.try_wait().ok().flatten();` ONCE; if None → `kill()` + `wait()`; classify from the captured value. Mirrors the kill+wait already used on the failed/dropped, seek, shutdown, and teardown paths.
- Regression caution: low — classification MUST use the pre-kill value; a post-kill `wait()` reports `code()==None`, flipping every natural end into a failed stream and triggering spurious recover_into rebuilds. `failed` must be computed from the pre-kill try_wait result.
- Extension (late-death EOF, `classify_end`): a clean EOF whose delivered playhead ends more than `EOF_SHORTFALL_MS` (10 s) short of the known `duration_ms` is a truncated stream — the transport died near the end (googlevideo closes mid-stream on this box) and reads as a finished track otherwise ("songs end ~30s early"). Treat it as dropped → `recover_into(pos)`. Unknown `duration_ms` (None) exempts — the resolver-gap behavior is unchanged. Regression caution: low — a *deterministically* short stream (metadata longer than the actual stream) churns the rebuild up to `RECOVERY_ATTEMPTS` (8) then `give_up_on` skips the track; the bounded-churn tradeoff the existing drop path already accepts.

**F15 — Cover decoded at full resolution per track and converted/cloned at full res, though the render target is ~40x20 cells** *(low, high, low risk)*
- src/engine/mod.rs:1106
- What happens: engine_meta loads the largest thumbnail (~1280x720 webp) at full res, derive_theme to_rgb8()s ~2.8MB for color-thief, apply_meta clones the ~3.7MB RGBA into Cover, and cover.rs re-clones at encode time — ~10–15MB memory traffic + full webp decode per track on the detached meta thread, though the art box is hard-capped at 14 rows (~280–336px max). Only two pixel consumers exist (derive_theme, Cover::from_image), so one downscale covers both.
- Safe fix: `img = img.thumbnail(320, 320);` once immediately after decode, feed the small image to both consumers; all changes inside engine_meta (engine-only, not protected). `thumbnail` (image 0.25.10) never upscales and never fails; ratatui-image Resize::Fit(None) never upscales the transmitted image.
- Regression caution: low — only real delta: color-thief tastes box-averaged pixels, so the derived theme tint can shift marginally on fine-grained art (cosmetic); tiny fallback thumbnails pass through unchanged; httpcache/lyrics/MPRIS/TXC/persistence read none of the pixels.

**F19 — ensure_cache_dir_0700 runs create_dir_all + chmod syscalls on every save** *(low, medium, low risk)*
- src/app/persist.rs:223
- What happens: every save re-runs create_dir_all + set_permissions(0o700) (util.rs:64-73) though the boot-time single-instance lock (term.rs:22-23) already created and chmodded the dir; ~3–4 syscalls × ~3600 saves/day on the blocking pool. The per-save ensure is also what self-heals a cache dir deleted mid-session. (refuter PARTIAL on syscall count; warns the naive fix kills the self-heal)
- Safe fix (assessor-confirmed): in save() write directly to `cache_dir().join("state.json")`; on write error, `ensure_cache_dir_0700()` then retry once — preserves the self-heal, removes the steady-state stat/chmod.
- Regression caution: low — boot-only caching (the naive variant) silently drops every save including the awaited quit save if the dir is deleted mid-session (the `let _ =` swallows the ENOENT).

**F22 — session.meta_cache HashMap grows unbounded for the whole session** *(low, medium, low risk)*
- src/app/event.rs:131
- What happens: apply_meta inserts one (uri → title/artist) per distinct track; zero removal sites exist (grep: 4 references, no clear/retain/remove); realistic ~150–250B/entry; a long radio session accumulates gently, never reclaimed, reset only at launch.
- Safe fix (verifier-corrected): after refresh_local_queue in the 24s tick, `let keep: HashSet<&str> = queue_uris.iter().map(String::as_str).collect(); meta_cache.retain(|uri,_| keep.contains(uri.as_str()));` — O(entries+queue), no state.rs field change; run it AFTER the refresh so queue_uris is current.
- Regression caution: low — HashSet membership, never Vec::contains (quadratic on UI thread at pathological sizes); naive clear()-at-cap bursts every queue row to bare URIs simultaneously; pruned tracks show bare URIs until replayed — the existing fallback contract of track_label_of.
- (UI-layer — report only, project rules forbid changes)

**F23 — image crate pulls all 15 default decoders + rayon + full AVIF (ravif) and OpenEXR stacks for two used formats** *(low, high, low risk)*
- Cargo.toml:20
- What happens: `image = "0.25"` defaults pull rayon + avif/bmp/dds/exr/ff/gif/hdr/ico/jpeg/png/pnm/qoi/tga/tiff/webp — verified resolving today (ravif, exr, tiff, gif, qoi in Cargo.lock); the only live decodes are `load_from_memory` on YouTube thumbnails (jpeg/webp; png needed by ratatui-image), and only tuna-tui enables image defaults (ratatui-image and quantette use default-features=false), so the trim materializes. cover.rs:85 `image::open` is dead (no callers).
- Safe fix: `image = { version = "0.25", default-features = false, features = ["jpeg", "png", "webp"] }`; no code changes. Disabled formats fail decode with graceful Err → the existing None path (no cover/theme), identical to today's corrupt-thumbnail behavior.
- Regression caution: low — drops ravif/exr/tiff/gif/qoi/rayon from the resolved graph; moxcms (ICC) is non-optional and stays; rayon gates only opt-in parallel iterators nothing calls (imageops::resize stays deterministic). Run `cargo build --all-features` + clippy after.

**F2 — Per-row heap allocations in the list renderer (Vec<Span>, format!, discarded uri/name clones)** *(low, high, medium risk)*
- src/ui/library.rs:199
- What happens: per visible row per frame — a fresh Vec<Span> (179), `format!(" {label}")` (199) even though truncate already returns Cow::Borrowed in the common case, and context_target(item) (169) heap-clones uri+name twice, discarded by `.is_some()`. ~60–120 small allocs/frame, up to ~3.6k/s at 30fps — negligible CPU, per-frame garbage.
- Safe fix: two minimal edits — `let playable_ctx = !item.is_header && !item.is_track && !item.is_play;` (provably equivalent; invariant already asserted at src/main_tests/playlist.rs:131) and two spans `Span::styled(" ", style)` + `Span::styled(label.as_ref(), style)` — the literal `Span::styled(label, …)` moves the Cow and breaks `label.chars().count() + 1` at line 201 (use `as_ref()`).
- Regression caution: medium — the render path has no unit tests (manual UAT only), and the flag form must stay in lockstep with context_target's predicate (a future predicate change silently diverges render vs Enter/P-play); the Vec<Span> alloc itself is inherent and correctly left alone.
- (UI-layer — report only, project rules forbid changes)

**F7 — FFT/visualizer computed during playback even when no one renders it** *(low, medium, medium risk)*
- src/audio/visualizer.rs:105
- What happens: FfmpegSource::fold feeds the visualizer on every chunk (ffmpeg_source.rs:146) regardless of view — Queue/Lyrics views compute ~344 1024-pt FFTs/s + band fill + decay locks with zero consumers; ~0.3–0.7% of a core (assessor estimate; the "~1%" headline is the same order). Paused it stops for free via rodio backpressure. Verification PARTIAL: the assessor rejected the naively-proposed flag wiring, not the issue.
- Safe fix (verifier-corrected): `pub enabled: bool` on VisBands, default true (preserves the fft_tee_keeps_feeding_* oracle tests); at the TOP of feed_interleaved BEFORE `sample_buf.extend` return early when disabled (buffer stays at its <FFT_SIZE residue); set the flag in the main.rs tick from the SAME expression that gates rendering (`view == RightView::NowPlaying`) via try_lock, before draw. Do NOT touch updated_at (stale → decay≈0 → fresh frame replaces immediately).
- Regression caution: medium — gating after the extend grows sample_buf unboundedly and burst-lags re-enable; touching updated_at while disabled leaves stale peaks stick-high ~1s on re-enable; a flag defaulting false or lagging the view by a tick reintroduces the frozen-spectrum bug class (Myx-a4.14); residual is a ~93ms static spectrum on re-entering NowPlaying.

### Info

**F28 — txc in default features costs nothing for the binary — keep it (non-finding, documented)** *(info, medium, none)*
- Cargo.toml:47
- What happens: default = streaming+txc; txc = dep:serde+dep:serde_json, both already under streaming, so no new crates resolve. One verifier notes the txc module itself is ~2.9k live lines in the binary (theme subcommand + album-reactive color socket) — a shipped feature, not wastage, and the assessor REFUTED the item as a defensible non-finding. Recommendation stands: no change.
- Safe fix: no change; optionally a clarifying comment on line 47 documenting the zero marginal dependency cost.
- Regression caution: none — "no change" cannot regress; do NOT read this as license to drop txc (silently removes `tuna-tui theme` + color publishing from the shipped binary, breaks txc_demo and default-feature consumers).

**F5 — No-cover NowPlaying path re-emits a wiped art box at 30fps** *(info, high, low risk)*
- src/ui/nowplaying.rs:81
- What happens: the `None => wipe_area(f, art_rect)` arm has no repaint guard; wipe_area (src/ui/mod.rs:167-177) flags ~300–390 cells AlwaysUpdate, and ratatui's diff emits AlwaysUpdate cells even when byte-identical — so the blank box is re-sent ~30x/s while playing in NowPlaying before EngineMeta lands, at startup-resume, and for the WHOLE track when a cover fetch fails (meta.image None).
- Safe fix: `None if repaint != ArtRepaint::Idle => wipe_area(f, art_rect), None => {}` — keeping the wipe reachable on Draw/Wipe; apply_meta schedules Draw on every track change, so the first frame clears a stale previous-track cover.
- Regression caution: low — never a bare `None => {}` (that drops the wipe from the Draw state and leaves the old image on screen for the whole next track); prefer `None => {}` over hold_area (hold marks cells Skip, pinning terminal-default blank unnecessarily).
- (UI-layer — report only, project rules forbid changes)

**F9 — Worker and watchdog poll cadences keep waking while completely idle** *(info, high, low risk)*
- src/engine/mod.rs:491
- What happens: the worker polls recv_timeout(100ms) unconditionally (tick is a full no-op when current is None) — 10 wakeups/s of timeout + nothing; watchdog sleeps fixed 5s even before the first track (~0.2/s). The refuter REFUTED on impact grounds (negligible-by-policy: ~0.001% of a core, standard event-loop polling); the assessor CONFIRMED the mechanism and that the fix loses no wakeup. Kept at info as an accurate observation with a sound fix.
- Safe fix: `let idle = self.current.is_none() && self.recovery.is_none();` then `recv_timeout(if idle { LONG } else { TICK })` with LONG ≤ STALL_AFTER; watchdog: `has_played` AtomicBool set in restart_stream, long sleep until the first play.
- Regression caution: low — key the block on `current.is_none()`, never on `!playing` (a paused track is current=Some and must keep the EOF poll); the watchdog's long sleep must key on has_played, never `!h.playing` (false during recovery spells — would delay stall detection of a just-recovered re-stall).

**F11 — Shuffle advance allocates the full other-index Vec per track** *(info, high, low risk)*
- src/engine/mod.rs:686
- What happens: the shuffle branch builds `Vec<usize>` of all non-cursor indices per advance (O(n), ~1.6KB at 200 tracks, µs) — the only per-track allocation in the queue logic, on a cold path (once per track transition). Never-pick-current invariant holds today including n==2.
- Safe fix: rejection loop — `loop { let i = rand::rng().random_range(0..n); if i != self.state.cursor { break i; } }`; keep the push-history-then-assign-cursor sequencing (Cmd::Prev pops it).
- Regression caution: low — the cheaper map variant (`random_range(0..n-1)` + `i >= cursor → i+1`) diverges for one advance in the degenerate cursor==len state give_up_on can leave; the rejection loop is identical in every reachable state including that one.

**F24 — tokio rt-multi-thread runtime: 3 of 4 worker threads permanently idle** *(info, high, low risk)*
- src/main.rs:134
- What happens: the entire async workload is one block_on'd boot future + one cooperative radio task; no channel/task fan-out anywhere, so three workers park for the whole session. A current_thread runtime runs identical code on one thread.
- Safe fix: `Builder::new_current_thread().enable_all()`; optionally trim tokio features to `["rt", "macros", "time"]` (drop rt-multi-thread, sync — unused); keep block_on, spawn_blocking, interval, and the two recv_async drains.
- Regression caution: low — the spawned radio task only advances between polls of boot (fine — no synchronous blocking exists on the UI thread today; engine calls are non-blocking sends); quit-path spawn_blocking await parks the single driver briefly, worst case delivering an in-flight radio result a tick late.

**F25 — engine_meta channel is unbounded while each message carries a full ~3.7MB image** *(info, medium, low risk)*
- src/main.rs:297
- What happens: flume::unbounded EngineMeta ships a full DynamicImage per message; a recovery storm (up to 8 rebuilds) or skip burst can queue several multi-MB images under a momentarily busy UI. Bounded by habit, not by the channel. (verification PARTIAL — the "24s save clone stalls the UI" example was wrong: that spawn_blocking is fire-and-forget.)
- Safe fix: `flume::bounded::<EngineMeta>(4)` + drop-oldest — pass a cloned receiver into the engine; the meta thread try_sends in a loop, on Full try_recv's the oldest and resends the new message.
- Regression caution: low — NEVER a blocking send on the bounded channel (parks one detached thread per saturated send, each holding an image — a thread pile-up regression); drop-oldest, not drop-newest (preserves the current track); the pending_meta/meta_is_current guard means a dropped message is invisible (cover falls back to defaults for that track only).

**F26 — liblog does open/write/close per call, and call sites format eagerly even when TUNA_LOG is unset** *(info, high, low risk)*
- src/liblog.rs:44
- What happens: with TUNA_LOG set, each call re-runs ensure_cache_dir + open + writeln + close (~4–5 syscalls); all ~30 call sites pass `format!(...)` arguments, evaluated before the env-var early return, so the String alloc happens even when TUNA_LOG is unset. Negligible at current rates (no per-frame/per-position sites exist; refuter corrected "hundreds of syscalls per track" to ~20–40 with TUNA_LOG set, a few allocs otherwise); latent if a hot site ever appears.
- Safe fix: keep the TUNA_LOG gate FIRST, then `OnceLock<Option<Mutex<File>>>` open-once append mode (0o600); the Mutex serializes multi-syscall writeln across UI/engine/media threads; optionally a liblog! macro for the non-protected call sites.
- Regression caution: low — the env gate must stay ahead of the OnceLock: liblog runs inside migrate_legacy_paths (config.rs:122-123) BEFORE the cache migration, and the cache dir must not exist before that point (util.rs:53-73 documents this contract); do NOT edit the src/app/event.rs:8 call site (protected) without a waiver — its single per-engine-event format! is not worth a policy violation.

**F27 — reqwest native-tls links system OpenSSL (libssl3 runtime dep) — rustls would drop it, at packaging churn cost** *(info, medium, low risk)*
- Cargo.toml:29
- What happens: verified via ldd + Cargo.lock — the openssl/native-tls graph is the sole cause; libz/libbrotli*/libzstd come from system OpenSSL, not Rust crates; all three reqwest consumers (covers, lyrics, engine meta) are live paths. Works and is documented across .deb, dist-workspace, flake, CI, and AUR. (verification PARTIAL: `rustls-tls` does not exist in reqwest 0.13 — it is `rustls` or `rustls-no-provider`; and there are EIGHT packaging surfaces, not three.)
- Safe fix: leave as-is is the recommendation. If pursued: one coordinated commit — `features = ["blocking", "json", "rustls-no-provider"]` + explicit `rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }` (ring needs only cc, no cmake), then update deb depends, dist-workspace, flake.nix, ci.yml, release.yml (x3), and both AUR PKGBUILDs in lockstep; verify ldd empty of ssl + live thumbnail/lrclib smoke.
- Regression caution: low — the as-written feature name fails cargo build; the aws-lc route requires adding cmake to five surfaces; residual runtime differences are trust-root loading (rustls-platform-verifier parses the system CA bundle) and rustls 0.23's prefer-post-quantum handshakes — both need the live smoke test.

**F31 — VmPeak ~530-560MB vs RSS 21-58MB: address-space high-water at startup, no committed growth** *(info, high, low risk)*
- runtime probe (both instances)
- What happens: VmPeak 543,296–560,624kB constant from the first sample (~30s after boot) against flat RSS; no #[global_allocator] anywhere → default glibc malloc, whose non-main arenas reserve 64MB VMAs each, plus ~19 threads × 8MB stacks ≈ the observed peak. RSS actually dropped after pause and stayed flat through a 40s soak — no leak, no committed growth.
- Safe fix: none required; if monitoring noise (top/ps ~540MB) matters, `MALLOC_ARENA_MAX=2` in the launch env only (a wrapper/.desktop/systemd Environment line), never in code.
- Regression caution: low — do-nothing is zero risk; the tunable is read at process start and changes no code path (allocation rates here are invisible to arena contention); explicitly do NOT switch to jemalloc or add a global_allocator for an info-grade cosmetic observation.

**F10 — Per-chunk Vec allocation in the pump + per-sample f32 copy in fold (constant-rate, bounded)** *(info, medium, medium risk)*
- src/engine/ffmpeg_source.rs:99
- What happens: the pump allocates a fresh 16KB Vec per read (~10.8/s, ≤8 in flight = 128KB peak via bounded(8)); fold copies each sample s16→scratch→f32 (~176K conversions/s, ~0.02% of a core — the original "~3.6M/s / few percent" is ~20–40x overstated; PARTIAL/PARTIAL). The reusable decode buffer (6b55f36) is genuinely used on every fold path; the audio callback stays allocation-free.
- Safe fix: make NO change — the finding's own recommendation, endorsed by both verifiers.
- Regression caution: medium — this file sits on four load-bearing invariants: the empty-Vec EOF marker (lose it → track never ends, documented in-file), the bounded(8) cross-thread backpressure (the pacing that prevents the Myx-a4.14 visualizer freeze), the non-blocking fold (try_recv in the audio callback), and the pending<PREBUFFER_SAMPLES bounded pull. A consumer-owned pool with no backpressure reintroduces the freeze; do NOT fold in the pump-side s16→f32 variant (moves the FFT feed off the audio thread, changes visualizer timing).

**F16 — meta_cache grows unbounded across the session** *(info, high, medium risk)*
- src/app/state.rs:132
- What happens: `HashMap<String, (String, String)>`, insert-only (event.rs:131-133), never evicted; realistic ~150–250B/entry (refuter corrected the estimate up, reinforcing the negligible conclusion); per-process only, reset at every launch.
- Safe fix: optional FIFO cap (~500) — sibling `VecDeque` pushed ONLY on new keys (`if !contains_key { push_back }` before the insert), then `while len > CAP { pop_front + remove }`; keep the field type and the &self read path (track_label_of) unchanged; no new dependency (no lru crate).
- Regression caution: medium — touches protected src/app/state.rs and src/app/event.rs → needs sign-off, or defer entirely (equally safe); push-on-EVERY-insert inflates the deque and spuriously evicts entries including, pathologically, the current track; a read-touch LRU would force track_label_of to &mut self for no gain.
- (UI-layer — report only, project rules forbid changes)

**F17 — No cap on concurrent yt-dlp subprocesses across app surfaces** *(info, medium, medium risk)*
- src/browse.rs:98
- What happens: no semaphore anywhere in src/yt; every surface (engine per-track resolve, search thread, detail thread, radio chains) funnels a fresh Command through yt_stdout — realistic overlap reaches 3–4 (engine resolves are serial, radio calls sequential), each child a fresh ~50–80MB Python with ~300–500ms startup, each bounded in time by the 15s deadline but unbounded in count. (verification PARTIAL: the fix needs fail-open acquire)
- Safe fix: `static YTDLP_PERMIT: Semaphore = Semaphore::new(2);` acquired at the top of yt_stdout with a bounded, FAIL-OPEN wait (up to the child's own 15s deadline, 50ms sleep poll; on budget exhaustion spawn anyway with a log); the RAII permit holds only across the child's life and drops on every existing early-return path.
- Regression caution: medium — an unbounded blocking acquire lets the engine worker exceed the 15s bound build_stream guarantees today; an acquire-timeout that returns None manufactures a spurious resolve failure the engine treats as a stream drop and burns a recovery retry; fail-open degrades to today's behavior under pathological contention.

**F20 — Action-menu builds and toggles do O(n) linear scans over library vectors** *(info, medium, medium risk)*
- src/actions.rs:72
- What happens: menu open runs one linear contains() (the four cited calls are mutually-exclusive match arms — the "four per open" evidence was overstated, PARTIAL/PARTIAL), plus linear toggle/retain and add_to_playlist scans on the input thread; tens of µs at realistic sizes, sub-ms even at thousands of rows. Verdicts agree: real but not measurable, and the per-save full-Store clone dwarfs the scans.
- Safe fix: do nothing now — the audit's own recommendation, confirmed by both verifiers.
- Regression caution: medium — if ever pursued: `#[serde(skip)]` side-sets rebuilt in SavedState::load, synced at the 4 mutation sites, ordered Vecs stay the single source of truth; a naively-added serialized HashSet field makes existing state.json fail to deserialize and load()'s unwrap_or_default silently yields an EMPTY library on upgrade.

**F29 — Unconditional deps (image, ratatui-image, souvlaki, color-thief, tui-textarea-2) make txc-only builds compile the full UI/image/zbus stack** *(info, high, medium risk)*
- Cargo.toml:16
- What happens: `--no-default-features --features txc` (the advertised protocol-only mode) still compiles image's full format zoo, ratatui-image, color-thief, tui-textarea-2, and souvlaki→zbus — empirically verified with a clean cargo check (551 nodes). Only cover.rs and reactive.rs are unconditional image consumers, and every real caller of them is streaming-gated.
- Safe fix: mark all five deps `optional = true` and wire them into the existing `streaming` feature (which stays in default, so the shipped binary is untouched); gate the module declarations in src/lib.rs — `#[cfg(feature = "streaming")] pub mod cover;` / `pub mod reactive;` — NOT inside the protected files (mirrors lib.rs:22-34); add `required-features = ["streaming"]` to theme_demo and explicitly declare the auto-discovered dump_theme example.
- Regression caution: medium — miswiring (a `ui` feature not in default) silently drops cover/theme from the shipped binary (loud compile errors in engine/mod.rs would still fire); without the example annotations, `--all-targets` breaks in the very txc-only mode being optimized; txc-only library consumers must now enable streaming for cover/reactive (intended surface reduction).

## Dropped / disputed findings

| ID | Title | Severity (as reported) | Why dropped |
|---|---|---|---|
| F4 | MPRIS position sync performs a DBus round-trip on the UI thread once per second | info | Mechanism false for this dependency configuration: souvlaki 0.8.3 is pinned to the zbus backend (no use_dbus), where set_playback is a non-blocking std::sync::mpsc send (zbus.rs:117-140); the DBus work runs on souvlaki's own service thread. The suggested async APIs do not exist in 0.8.3, and coalescing to 2–3s would only coarsen MPRIS position updates for desktop widgets — a pure UX regression with nothing to save. Code already optimal. |

## Quick wins

In order — cheapest, highest value first:

1. **F18** — atomic state.json save (temp+rename, unique temp names) in persist.rs: ~10 lines, converts "silent total library loss on any torn write" into an impossibility; persist.rs is explicitly editable.
2. **F21** — dirty-gate the 24s save in main.rs (two flags set at the 3 store-mutator + transport sites): kills the idle full-store clone+serialize+write, low risk, no protected files.
3. **F23** — `image = { default-features = false, features = ["jpeg", "png", "webp"] }`: a one-line Cargo.toml edit that drops ravif/OpenEXR/tiff/gif/qoi/rayon from the build graph.
4. **F8** — reap-on-None in track_ended with pre-kill classification: ~6 lines closing the zombie window, using a kill+wait pattern already present on four other paths.
5. **F11** — rejection loop for shuffle advance: ~5 lines, removes the only per-track allocation in queue logic, behaviorally identical in every reachable state.

## Healthy areas

Grouped by dimension (from the clean-area sweep).

- **UI render loop**: event-driven redraw with dirty/animation/idle gates — no dirt, no terminal write (src/app/frame.rs); O(viewport) rendering independent of list length; diff-based presentation wrapped in synchronized updates (main.rs:623-627); per-frame scratch buffers reused across frames (FrameOut); truncate borrows (Cow::Borrowed) in the common case.
- **Engine/audio**: FFT never runs while paused (rodio backpressure, not a flag); pump thread terminates on every path and no leaked pump exists; seek reuses the resolved URL (no `-J` per seek); EOF handling does one try_wait with no repeat work; recovery capped at 8 attempts with 5–120s backoff and cannot stack; PositionCorrection at 1/s — the pre-port ~4k/s storm is gone; watchdog excludes paused streams and weak-sender-retires.
- **Network/yt-dlp**: exactly one `-J` spawn per operation with borrow-only reads and no dump cloning; yt_stdout hygiene (--retries 1, 15s deadline, concurrent stdout+stderr drains preventing the pipe-deadlock, stdin nulled); httpcache is disk-only, OnceLock dir, age-based 30-day sweep covering dumps AND cover bytes, atomic temp+rename writes; one shared warmed blocking reqwest client; radio happy path capped via --playlist-end; search is one flat call per Enter.
- **Persistence**: store mutations never trigger synchronous saves (24s debounce is sound — only its unconditional firing is wasteful); history hard-capped at 100; writes off-thread with awaited quit saves; single-pass one-time load; fs2 single-instance lock held once for the session, zero per-write lock overhead.
- **Threads/memory**: every spawned thread has a bounded life and a retirement path (disconnect, weak-sender upgrade, EOF, self-reap); no spin loops anywhere — every poll sleeps or blocks; mutexes are short-hold with try_lock on the audio path; channel bounds are correct (pump bounded(8), txc bounded(64), everything else human/1s cadence); RSS flat with no monotonic growth across ~20 min of sampling; TERM shutdown clean twice with zero orphans.
- **Build/dependencies**: release profile complete (lto, codegen-units=1, strip, panic=abort); ratatui-image, rodio (playback only), and color-thief already minimal; target-gated deps stay out of the Linux build; zero hidden compression crates (Cargo.lock has no brotli/zstd/libz-sys); link footprint matches the packaging declarations exactly; CI gates fmt + clippy -D warnings + all-features tests with rust-cache.
- **Runtime probe**: max 4.5% CPU playing / 1.5–3.7% paused with the idle redraw at 2fps; stream-drop recovery verified working live (watchdog re-resolved mid-track, no user action); startup resume as designed (~30MB RSS, playback within seconds of boot); all threads State S at every sampled state.

## Caveats

Not measured or only partially measured — treat the related claims accordingly:

- **Live network behavior** was not exercised: throttling, bot checks, and real yt-dlp latency under genuine links were not probed; the F13/F14/F17 impact estimates assume the observed local process behavior.
- **Playback-path CPU** was measured only on this box with the ALSA backend (~4.5% max); no comparison across backends (PipeWire/pulse), codecs, sample rates, or bitrates exists.
- **Long-soak leak testing** was limited to ~20 minutes of cumulative sampling (40s continuous windows); the monotonic growth of F16/F22 (meta_cache) is source-projected, not soak-verified, and the tuna-meta pile-up of F6 was reasoned, not reproduced.
- **Radio fallback latency** on fresh/obscure seeds was not observed live (no "radio timed out" path occurred in the probe), so F13's ~40s orphan window is derived from the 4×15s chain arithmetic, not measured end-to-end.
- **Cross-platform** was not measured: Windows (rename-over-existing semantics for F18, cfg(windows) build) and macOS (winit, Darwin packaging) were source-analyzed only; F27's packaging matrix (deb/dist/flake/CI/AUR) is code-verified but never built after the proposed changes.