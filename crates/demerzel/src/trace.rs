//! The debug trace: one log file per run, behind `DEMERZEL_LOG`.
//!
//! The variable names a level — `error`, `warn`, `info`, `debug`, or
//! `trace` — and turns the trace on. Unset, nothing is written, and
//! every `log` macro across the crates costs one level check. The
//! file lands under the data directory:
//! `$XDG_DATA_HOME/demerzel/logs/<run-start RFC3339>-<pid>.log`. It is
//! the one sink for every mode: the full-screen interface cannot
//! write to stderr behind the alternate screen, so a line on stderr
//! names the path and the file holds the trace.
//!
//! Line shape: `<RFC3339 millis> <LEVEL> <target> <message>`, one
//! event per line. A message with a line break lands escaped, so a
//! reader can split the file on newlines. Lines at `info` and above
//! flush as they land; `debug` and `trace` lines buffer, and
//! [`flush`] empties the buffer at exit.
//!
//! The file is mode 0600: at `trace` it holds the request bodies and
//! the conversation. It never holds a credential — no bearer token,
//! no API key, no auth-store content — and the emitters keep that
//! promise; `docs/config.md` states it and names what each level
//! carries.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// The environment variable that turns the trace on and names its
/// level.
pub const ENV_LOG: &str = "DEMERZEL_LOG";

/// The level names the variable takes, for the error message.
const LEVEL_NAMES: &str = "error, warn, info, debug, trace";

static SINK: OnceLock<FileSink> = OnceLock::new();

/// The file sink: one line per event, flushed per line at `info` and
/// above. The lock serializes writers; the engine and the interfaces
/// emit from more than one thread.
struct FileSink {
    level: LevelFilter,
    file: Mutex<BufWriter<File>>,
}

impl Log for FileSink {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut line = format!(
            "{} {} {} {}",
            humantime::format_rfc3339_millis(SystemTime::now()),
            record.level(),
            record.target(),
            record.args()
        );
        // One event per line: a body with a line break is escaped,
        // not split.
        if line.contains(['\n', '\r']) {
            line = line.replace('\r', "\\r").replace('\n', "\\n");
        }
        line.push('\n');
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // A write failure has nowhere to report; the trace is a
        // diagnostic, and the run goes on without it.
        let _ = file.write_all(line.as_bytes());
        if record.level() <= Level::Info {
            let _ = file.flush();
        }
    }

    fn flush(&self) {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _ = file.flush();
    }
}

/// Read `DEMERZEL_LOG`. When it names a level, open the run's log
/// file, install the sink, and return the path; when it is unset or
/// empty, return `None` and install nothing. A value that is not a
/// level is an error, so a typo does not run silent.
pub fn init() -> Result<Option<PathBuf>, String> {
    let Some(value) = std::env::var_os(ENV_LOG) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    if value.is_empty() {
        return Ok(None);
    }
    let level = match value.as_ref() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        other => {
            return Err(format!(
                "{ENV_LOG} must be one of {LEVEL_NAMES}, not `{other}`"
            ));
        }
    };
    let dir = demerzel_core::paths::data_dir()
        .map_err(|error| error.to_string())?
        .join("logs");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let path = dir.join(format!(
        "{}-{}.log",
        humantime::format_rfc3339_seconds(SystemTime::now()),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let sink = FileSink {
        level,
        file: Mutex::new(BufWriter::new(file)),
    };
    if SINK.set(sink).is_err() {
        return Err("the debug trace is already open".to_string());
    }
    let sink = SINK.get().expect("set above");
    log::set_logger(sink).map_err(|error| error.to_string())?;
    log::set_max_level(level);
    log::info!(
        target: "demerzel",
        "demerzel {} pid={} level={}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        value
    );
    Ok(Some(path))
}

/// The stderr line that names the log file. The interfaces print it
/// after the ready marker; a one-shot run prints it first.
pub fn path_line(path: &std::path::Path) -> String {
    format!("log file: {}", path.display())
}

/// Write out the buffered `debug` and `trace` lines. Call it before
/// the process exits; a no-op when the trace is off.
pub fn flush() {
    if let Some(sink) = SINK.get() {
        sink.flush();
    }
}
