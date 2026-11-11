//! The store as a session uses it, and what a session counts (DESIGN
//! sections 9 and 12).

use std::path::Path;

use crate::config::{Config, Mode};
use crate::log;
use crate::solver::hash_executable;
use crate::store::{self, Entry, Key, SolverInfo, SolverStamp, SqliteStore, Store};

#[derive(Default)]
pub(super) struct Counts {
    pub(super) hits: i64,
    pub(super) misses: i64,
    pub(super) stores: i64,
    pub(super) saved_us: i64,
    pub(super) solver_starts: i64,
    pub(super) replay_mismatches: i64,
    /// Misses that had to wait for another session's in-flight lock.
    pub(super) lock_waits: i64,
    /// Hits found by the lookup made after taking the lock (also in `hits`;
    /// an audited answer is neither).
    pub(super) lock_hits: i64,
    /// Misses that gave up waiting for a lock and solved without it.
    pub(super) lock_timeouts: i64,
    pub(super) audits: i64,
    pub(super) audit_contradictions: i64,
    pub(super) audit_instabilities: i64,
    pub(super) audit_failures: i64,
}

impl Counts {
    /// Each count with the counter name it is added to.
    fn named(&self) -> [(&'static str, i64); 13] {
        [
            ("hits", self.hits),
            ("misses", self.misses),
            ("stores", self.stores),
            ("saved_us", self.saved_us),
            ("solver_starts", self.solver_starts),
            ("replay_mismatches", self.replay_mismatches),
            ("lock_waits", self.lock_waits),
            ("lock_hits", self.lock_hits),
            ("lock_timeouts", self.lock_timeouts),
            ("audits", self.audits),
            ("audit_contradictions", self.audit_contradictions),
            ("audit_instabilities", self.audit_instabilities),
            ("audit_failures", self.audit_failures),
        ]
    }
}

/// What a session adds to the store's counters at exit.
pub(super) fn session_counters(counts: &Counts, errored: bool, bypass: Option<&str>) -> Vec<(String, i64)> {
    let mut counters = vec![("sessions".to_string(), 1), ("errored_sessions".to_string(), i64::from(errored))];
    counters.extend(counts.named().map(|(name, n)| (name.to_string(), n)));
    if let Some(reason) = bypass {
        counters.push(("bypassed_sessions".into(), 1));
        counters.push((format!("bypass: {reason}"), 1));
    }
    counters
}

/// The store as a session uses it. Any error disables it for the rest of the
/// session (rule 2), and writes are skipped unless the mode is `rw`.
pub(super) struct SessionStore {
    store: Option<SqliteStore>,
    writable: bool,
}

impl SessionStore {
    pub(super) fn open(cfg: &Config) -> SessionStore {
        let open = |dir: &Path| match cfg.mode {
            // ro and enforce never write: no file, no migration (section 11).
            Mode::Ro | Mode::Enforce => SqliteStore::open_read_only(dir),
            _ => SqliteStore::open(dir),
        };
        let store = match &cfg.dir {
            Some(dir) => match open(dir) {
                Ok(s) => Some(s),
                Err(e) => {
                    log!("store disabled: cannot open {}: {e}", dir.display());
                    None
                }
            },
            None => {
                log!("store disabled: no cache directory (set Z3CACHE_DIR)");
                None
            }
        };
        SessionStore { store, writable: cfg.mode == Mode::Rw }
    }

    pub(super) fn is_open(&self) -> bool {
        self.store.is_some()
    }

    pub(super) fn can_write(&self) -> bool {
        self.writable && self.is_open()
    }

    /// The store, if it is open and may be written. Errors from it are the
    /// caller's to handle.
    pub(super) fn writing(&mut self) -> Option<&mut SqliteStore> {
        self.store.as_mut().filter(|_| self.writable)
    }

    /// Runs `op` on the store if it is open; an error disables the store.
    fn with<T>(&mut self, op: impl FnOnce(&mut SqliteStore) -> store::Result<T>) -> Option<T> {
        match op(self.store.as_mut()?) {
            Ok(v) => Some(v),
            Err(e) => {
                log!("store disabled: {e}");
                self.store = None;
                None
            }
        }
    }

    /// Like [`SessionStore::with`], for a write: skipped unless writable.
    fn write<T>(&mut self, op: impl FnOnce(&mut SqliteStore) -> store::Result<T>) -> Option<T> {
        if !self.writable {
            return None;
        }
        self.with(op)
    }

    pub(super) fn get(&mut self, key: &Key) -> Option<Entry> {
        self.with(|s| s.get(key)).flatten()
    }

    /// Returns whether the entry was written.
    pub(super) fn put(&mut self, key: &Key, entry: &Entry) -> bool {
        self.write(|s| s.put(key, entry)).is_some()
    }

    pub(super) fn delete(&mut self, key: &Key) {
        self.write(|s| s.delete(key));
    }

    fn solver(&mut self, stamp: &SolverStamp) -> Option<SolverInfo> {
        self.with(|s| s.solver(stamp)).flatten()
    }

    /// Records a solver's identity. It is only a memo: failing to write it
    /// costs hashing the binary again, or asking for its version, in a later
    /// session, so the store stays in use.
    pub(super) fn put_solver(&mut self, stamp: &SolverStamp, info: &SolverInfo) {
        if let Some(st) = self.writing()
            && let Err(e) = st.put_solver(stamp, info)
        {
            log!("cannot record solver identity: {e}");
        }
    }
}

/// Finds the solver's identity, from the store if it has been seen before.
pub(super) fn identify(real: &Path, stamp: Option<&SolverStamp>, store: &mut SessionStore) -> Option<SolverInfo> {
    if let Some(info) = stamp.and_then(|stamp| store.solver(stamp)) {
        return Some(info);
    }
    let id = match hash_executable(real) {
        Ok(id) => id,
        Err(e) => {
            log!("cannot hash {}: {e}; caching disabled", real.display());
            return None;
        }
    };
    let info = SolverInfo { id, version_response: None };
    if let Some(stamp) = stamp {
        store.put_solver(stamp, &info);
    }
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_shows_every_session_counter() {
        for (name, _) in session_counters(&Counts::default(), true, Some("reason")) {
            if name.starts_with("bypass: ") {
                continue;
            }
            assert!(crate::cli::COUNTERS.iter().any(|(n, _)| *n == name), "{name} is not shown by stats");
        }
    }
}
