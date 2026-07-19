//! The `log` bridge and the optional `TUNA_LOG` debug file.

/// Temporary-but-useful diagnostics for startup/library failures. Kept out of
/// the TUI because alternate-screen rendering hides stderr.
/// Forwards the `log` crate output (engine, media controls) into `tuna-tui.log`;
/// without a logger installed it goes nowhere.
pub struct TunaLog;

impl log::Log for TunaLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        liblog(format!(
            "{} {}: {}",
            record.level(),
            record.target(),
            record.args()
        ));
    }
    fn flush(&self) {}
}

/// Any value of `TUNA_LOG` turns logging on; the value only picks how loud
/// the engine is. `debug`/`trace` open it up, `warn` quiets it back down.
pub fn install_tuna_log() {
    let Ok(level) = std::env::var("TUNA_LOG") else {
        return;
    };
    let filter = match level.to_ascii_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        _ => log::LevelFilter::Info,
    };
    if log::set_boxed_logger(Box::new(TunaLog)).is_ok() {
        log::set_max_level(filter);
    }
}

/// Optional debug log — silent unless `TUNA_LOG` is set. Writes to
/// ~/.cache/tuna-tui/tuna-tui.log (user-owned dir 0700, file 0600) instead of a
/// world-writable fixed /tmp path (audit H5). The file is opened once, on the
/// first call, and kept for the session (audit F26): no open/close syscall pair
/// per call, and the env gate below stays ahead of the OnceLock because liblog
/// runs inside `config::migrate_legacy_paths` — before the cache migration —
/// where the cache dir must not be created (or raced) yet (util.rs contract).
/// If that first open finds no cache dir it yields None and is never retried;
/// acceptable for this debug path.
static FILE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
    std::sync::OnceLock::new();

pub fn liblog(msg: impl AsRef<str>) {
    use std::io::Write;
    // Env gate first: must run before the OnceLock is ever touched, because
    // this function is reachable from migrate_legacy_paths (config.rs) ahead of
    // the cache migration.
    if std::env::var_os("TUNA_LOG").is_none() {
        return;
    }
    let file = FILE.get_or_init(|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(crate::util::cache_dir()?.join("tuna-tui.log"))
            .ok()
            .map(std::sync::Mutex::new)
    });
    if let Some(f) = file.as_ref() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = writeln!(
            f.lock().unwrap_or_else(|p| p.into_inner()),
            "{ts:.3} {}",
            msg.as_ref()
        );
    }
}
