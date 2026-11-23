//! The debug log (`--log <file>`). Nothing may print while the UI is up, so everything the
//! app and the libraries have to say goes through the `log` crate into this file, or
//! nowhere when no file was given.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

struct FileLogger {
    file: Mutex<File>,
    started: Instant,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let elapsed = self.started.elapsed().as_secs_f64();
        let line = format!("{elapsed:>9.3} {:<5} {}: {}\n", record.level(), record.target(), record.args());
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Send the log to `path` (appended to, created if missing). Without a path the log is
/// dropped. Call once, before anything logs; a second call does nothing.
pub fn init(path: Option<&Path>) -> io::Result<()> {
    let Some(path) = path else { return Ok(()) };
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    // The logger lives as long as the process; leaking it is how `log` wants one.
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger { file: Mutex::new(file), started: Instant::now() }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
        log::info!("bakedit {} started, pid {}", env!("CARGO_PKG_VERSION"), std::process::id());
    }
    Ok(())
}
