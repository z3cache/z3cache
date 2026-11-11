//! z3cache: a drop-in replacement for the `z3` binary that caches solver
//! answers. See `docs/DESIGN.md`.

pub mod analyze;
pub mod cli;
pub mod command;
pub mod config;
pub mod lock;
pub mod log;
pub mod profile;
pub mod record;
pub mod rlimit;
pub mod session;
pub mod sexp;
pub mod solver;
pub mod state;
pub mod store;

/// Bump on any change to canonicalisation, command classification or key
/// layout. Old cache entries then become unreachable.
pub const FORMAT_VERSION: u32 = 4;

/// Time since the Unix epoch; zero if the clock is set before it.
pub fn since_epoch() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// A fresh, empty path for a unit test's files: `z3cache-<kind>-<pid>-<name>`
/// in the temporary directory.
#[cfg(test)]
pub(crate) fn test_dir(kind: &str, name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("z3cache-{kind}-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}
