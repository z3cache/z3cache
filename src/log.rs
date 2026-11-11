//! Diagnostic log (`Z3CACHE_LOG`). Never writes to stdout or stderr.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Opens the log file. A file that cannot be opened disables logging.
pub fn init(path: Option<&Path>) {
    let file = path.and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());
    let _ = LOG.set(file.map(Mutex::new));
}

pub fn write(args: std::fmt::Arguments<'_>) {
    if let Some(Some(file)) = LOG.get() {
        let now = crate::since_epoch();
        let line = format!(
            "{}.{:03} z3cache[{}] {}\n",
            now.as_secs(),
            now.subsec_millis(),
            std::process::id(),
            args
        );
        if let Ok(mut f) = file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Writes a line to the diagnostic log, if one is configured.
#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {
        $crate::log::write(format_args!($($t)*))
    };
}
