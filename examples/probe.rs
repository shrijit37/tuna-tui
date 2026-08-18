//! Phase-2 probe: start the local engine (yt-dlp → ffmpeg → rodio) and drive
//! playback — proving tuna-tui is a real player against real YouTube.
//!
//!   cargo run --example probe -- yt:video:dQw4w9WgXcQ
//!   cargo run --example probe -- yt:playlist:PL8dPuuaLjXtOAKed_MxxWBNaPno5h3Zs8
//!
//! Once running, type transport commands + Enter:
//!   play | pause | p (toggle) | next | prev | load <uri> | seek <ms> | quit

use std::io::BufRead;
use std::sync::Arc;

use tuna_tui::engine::{self, EngineEvent};

fn main() -> anyhow::Result<()> {
    println!("tuna-tui-probe: opening audio device…");
    let (tx, rx) = flume::unbounded::<EngineEvent>();
    let (meta_tx, _meta_rx) = flume::unbounded::<engine::EngineMeta>();
    let expander: Arc<dyn tuna_tui::engine::Expander> = Arc::new(engine::YtExpander);
    let engine = engine::run(tx, meta_tx, 50, 2, expander)?;
    println!("tuna-tui-probe: engine live; yt-dlp + ffmpeg pipelines ready.");

    // If a URI was passed, start playing it immediately.
    if let Some(uri) = std::env::args().nth(1) {
        println!("▶ starting playback: {uri}");
        if let Err(err) = engine.play_context(uri, false) {
            eprintln!("failed to start playback: {err:#}");
        }
    } else {
        println!("(pass a yt: URI as an arg to start playback)");
    }
    println!("commands: play | pause | p | next | prev | seek <ms> | load <uri> | quit\n");

    // Read stdin commands on a blocking thread, forward over a channel.
    let (cmd_tx, cmd_rx) = flume::unbounded::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
    });

    let engine = std::sync::Arc::new(engine);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(20));
        while let Ok(ev) = rx.try_recv() {
            match ev {
                EngineEvent::TrackChanged { uri } => println!("♫ track changed → {uri}"),
                EngineEvent::Playing { uri, position_ms } => {
                    println!("▶ playing   {uri} @ {position_ms}ms")
                }
                EngineEvent::Paused { uri, position_ms } => {
                    println!("⏸ paused    {uri} @ {position_ms}ms")
                }
                EngineEvent::Stopped => println!("⏹ stopped"),
                EngineEvent::LoadFailed { uri, message } => {
                    println!("✗ load failed {uri}: {message}")
                }
                EngineEvent::Reconnecting => println!("⟳ stream lost, reconnecting"),
                EngineEvent::Reconnected => println!("⟳ reconnected"),
                EngineEvent::PositionCorrection { uri, position_ms } => {
                    println!("↔ position  {uri} @ {position_ms}ms")
                }
            }
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            let cmd = cmd.trim();
            let result = match cmd.split_once(' ') {
                Some(("load", uri)) => engine.play_context(uri.trim().to_string(), false),
                Some(("seek", ms)) => {
                    let ms: u32 = ms.trim().parse().unwrap_or(0);
                    engine.seek(ms)
                }
                _ => match cmd {
                    "play" => engine.play(),
                    "pause" => engine.pause(),
                    "p" | "toggle" => engine.toggle(),
                    "next" | "n" => engine.next(),
                    "prev" | "b" => engine.prev(),
                    "quit" | "q" => return Ok(()),
                    "" => Ok(()),
                    other => {
                        println!("? unknown command: {other}");
                        Ok(())
                    }
                },
            };
            if let Err(err) = result {
                eprintln!("command failed: {err:#}");
            }
        }
    }
}
