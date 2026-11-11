//! The cache store (DESIGN section 9).
//!
//! Callers treat every error the same way: log it and carry on with the store
//! disabled (AGENTS rule 2). Nothing here may affect an answer.

mod sqlite;

use std::path::Path;

pub use sqlite::{FILE_NAME, SqliteStore};

pub type Key = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The exact bytes the solver wrote for the command.
    pub response: Vec<u8>,
    pub is_query: bool,
    /// Resource-count delta of the query (section 8); `None` for reads.
    pub rlimit: Option<u64>,
    /// Wall time of the original solver run, in microseconds.
    pub solve_us: u64,
    /// Unix seconds; maintained by the store.
    pub last_used_s: i64,
}

/// What the store remembers about a solver binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolverInfo {
    /// BLAKE3 of the executable (without the salt).
    pub id: Key,
    /// The solver's reply to `(get-info :version)`, once seen.
    pub version_response: Option<Vec<u8>>,
}

/// Identifies a solver binary on disk without reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolverStamp {
    pub path: Vec<u8>,
    pub size: u64,
    pub mtime_ns: i64,
}

impl SolverStamp {
    pub fn of(path: &Path) -> std::io::Result<SolverStamp> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        Ok(SolverStamp {
            path: path.as_os_str().as_encoded_bytes().to_vec(),
            size: m.size(),
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// What [`Store::evict`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    /// Store size (database and write-ahead log) before and after, in bytes.
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub deleted: u64,
    pub entries_left: u64,
}

pub trait Store {
    fn get(&mut self, key: &Key) -> Result<Option<Entry>>;
    /// Stores an entry. If the key is already present the existing entry is
    /// kept: both came from the same input.
    fn put(&mut self, key: &Key, entry: &Entry) -> Result<()>;
    /// Session-end bookkeeping in one transaction: refresh `last_used_s` of
    /// `touched` and add `counters` to today's totals.
    fn finish(&mut self, touched: &[Key], counters: &[(String, i64)]) -> Result<()>;
    fn solver(&mut self, stamp: &SolverStamp) -> Result<Option<SolverInfo>>;
    fn put_solver(&mut self, stamp: &SolverStamp, info: &SolverInfo) -> Result<()>;
    fn count(&mut self) -> Result<u64>;
    /// Deletes every entry; returns how many there were.
    fn clear(&mut self) -> Result<u64>;
    /// Deletes one entry; returns whether it existed.
    fn delete(&mut self, key: &Key) -> Result<bool>;
    /// If the database is larger than `cap` bytes, deletes least-recently-used
    /// entries until the space in use is at most 80% of `cap`, then returns
    /// the freed pages to the file system.
    fn evict(&mut self, cap: u64) -> Result<Eviction>;
    /// All counters as (day, name, n).
    fn counters(&mut self) -> Result<Vec<(String, String, i64)>>;
}

pub fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` (UTC) for a Unix time in seconds.
pub fn day(unix_s: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = unix_s.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
        assert_eq!(day(1_794_325_577), "2026-11-10");
    }
}
