//! In-flight locks (DESIGN section 10).
//!
//! When sessions in several worktrees miss the same key at the same moment,
//! one solves it and the others wait for its answer. The lock is an
//! exclusive `flock` on `<dir>/locks/<first two hex digits>/<key hex>`; the
//! operating system releases it if the holder dies. Locks only avoid
//! duplicate work: every answer still comes from a lookup or the solver, so
//! a lock that fails, is lost, or is shared by mistake can at worst cause
//! a query to be solved twice.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::log;
use crate::store::Key;

/// A held lock; released when dropped.
pub struct KeyLock {
    file: File,
    /// The lock had to wait for another holder.
    pub waited: bool,
}

impl Drop for KeyLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The lock directory inside a cache directory.
pub fn lock_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("locks")
}

fn key_path(cache_dir: &Path, key: &Key) -> PathBuf {
    let hex = blake3::Hash::from(*key).to_hex();
    lock_dir(cache_dir).join(&hex[..2]).join(hex.as_str())
}

/// Opens a lock file, creating it and its directory if needed.
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    fs::create_dir_all(path.parent().expect("lock path has a parent"))?;
    OpenOptions::new().create(true).truncate(false).write(true).open(path)
}

/// The result of [`acquire`].
pub enum Acquired {
    Lock(KeyLock),
    /// Another session held the lock for longer than the wait limit.
    TimedOut,
    /// The lock file could not be created or locked.
    Unavailable,
}

/// Takes the lock for `key`, waiting up to `max_wait` for another holder.
/// The wait polls without blocking, backing off from 5 ms to 200 ms, so a
/// holder that hangs or is stopped delays this session by at most
/// `max_wait`. `on_busy` is called once, the first time the lock is found
/// held by someone else (the session logs it). In every case but
/// [`Acquired::Lock`] the caller solves without the lock.
pub fn acquire(cache_dir: &Path, key: &Key, max_wait: Duration, mut on_busy: impl FnMut()) -> Acquired {
    let path = key_path(cache_dir, key);
    let file = match open_lock_file(&path) {
        Ok(f) => f,
        Err(e) => {
            log!("no in-flight lock ({}): {e}", path.display());
            return Acquired::Unavailable;
        }
    };
    let start = Instant::now();
    let mut pause = Duration::from_millis(5);
    let mut waited = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Acquired::Lock(KeyLock { file, waited }),
            Err(TryLockError::WouldBlock) => {
                if !waited {
                    on_busy();
                }
                let elapsed = start.elapsed();
                if elapsed >= max_wait {
                    log!("in-flight lock still held after {elapsed:?}; solving without it");
                    return Acquired::TimedOut;
                }
                waited = true;
                std::thread::sleep(pause.min(max_wait - elapsed));
                pause = (pause * 2).min(Duration::from_millis(200));
            }
            Err(TryLockError::Error(e)) => {
                log!("no in-flight lock ({}): {e}", path.display());
                return Acquired::Unavailable;
            }
        }
    }
}

/// What [`clean`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: u64,
    /// Old enough to remove, but held by a session.
    pub held: u64,
    /// Old enough to remove, but could not be opened or locked to check.
    pub failed: u64,
}

/// Removes lock files last modified more than `older_than` ago that no
/// session holds. A file is removed only while this function holds its
/// lock. A session that opened the file just before, and then locks the
/// removed file, no longer excludes a session that creates a new one: both
/// may solve the same key. That costs time, never correctness.
pub fn clean(cache_dir: &Path, older_than: Duration) -> Cleaned {
    let mut c = Cleaned::default();
    let Ok(shards) = fs::read_dir(lock_dir(cache_dir)) else {
        return c;
    };
    let cutoff = SystemTime::now() - older_than;
    for shard in shards.flatten() {
        let Ok(files) = fs::read_dir(shard.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| t < cutoff)
                .unwrap_or(false);
            if !old {
                continue;
            }
            match lock_existing(&path) {
                Ok(_file) => {
                    if fs::remove_file(&path).is_ok() {
                        c.removed += 1;
                    }
                    // Dropping `_file` releases the lock on the removed file.
                }
                Err(TryLockError::WouldBlock) => c.held += 1,
                Err(TryLockError::Error(e)) => {
                    log!("cannot check lock file {}: {e}", path.display());
                    c.failed += 1;
                }
            }
        }
    }
    c
}

/// Opens an existing lock file and takes its lock without waiting.
fn lock_existing(path: &Path) -> Result<File, TryLockError> {
    let file = OpenOptions::new().write(true).open(path).map_err(TryLockError::Error)?;
    file.try_lock()?;
    Ok(file)
}

/// Takes `<dir>/locks/gc` without waiting, so that only one `gc` runs at a
/// time. `None` if another holds it or it cannot be created.
pub fn try_gc_lock(cache_dir: &Path) -> Option<File> {
    let file = open_lock_file(&lock_dir(cache_dir).join("gc")).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("z3cache-lock-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn second_holder_waits_until_release() {
        let dir = temp("wait");
        let key = [7u8; 32];
        let first = lock(&dir, &key);
        assert!(!first.waited);
        assert!(key_path(&dir, &key).starts_with(dir.join("locks").join("07")));
        // The waiter reports, through `on_busy`, that it found the lock held;
        // only then is it released.
        let (busy_tx, busy) = std::sync::mpsc::channel();
        let d = dir.clone();
        let waiter = std::thread::spawn(move || {
            match acquire(&d, &key, Duration::from_secs(60), || busy_tx.send(()).unwrap()) {
                Acquired::Lock(l) => l.waited,
                _ => panic!("no lock"),
            }
        });
        busy.recv().unwrap();
        assert!(!waiter.is_finished(), "the second taker must wait");
        drop(first);
        assert!(waiter.join().unwrap());
    }

    fn lock(dir: &Path, key: &Key) -> KeyLock {
        match acquire(dir, key, Duration::from_secs(10), || {}) {
            Acquired::Lock(l) => l,
            _ => panic!("no lock"),
        }
    }

    #[test]
    fn waits_are_bounded() {
        let dir = temp("bounded");
        let key = [3u8; 32];
        let _held = lock(&dir, &key);
        let start = Instant::now();
        let mut busy = 0;
        assert!(matches!(
            acquire(&dir, &key, Duration::from_millis(300), || busy += 1),
            Acquired::TimedOut
        ));
        let took = start.elapsed();
        assert_eq!(busy, 1, "reported once");
        // The bound is the point here; the upper limit only has to catch an
        // unbounded wait.
        assert!(took >= Duration::from_millis(300) && took < Duration::from_secs(30), "{took:?}");
        assert!(matches!(acquire(&dir, &key, Duration::ZERO, || {}), Acquired::TimedOut));
    }

    #[test]
    fn unusable_lock_dir_means_no_lock() {
        let dir = temp("unusable");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("locks"), b"not a directory").unwrap();
        assert!(matches!(acquire(&dir, &[1; 32], Duration::from_secs(1), || {}), Acquired::Unavailable));
    }

    #[test]
    fn clean_removes_only_old_unheld_files() {
        let dir = temp("clean");
        let held = lock(&dir, &[1; 32]);
        drop(lock(&dir, &[2; 32]));
        // Nothing is old yet.
        assert_eq!(clean(&dir, Duration::from_secs(3600)), Cleaned::default());
        // Everything is old: the unheld file goes, the held one stays.
        let c = clean(&dir, Duration::ZERO);
        assert_eq!(c, Cleaned { removed: 1, held: 1, failed: 0 });
        assert!(key_path(&dir, &[1; 32]).exists());
        assert!(!key_path(&dir, &[2; 32]).exists());
        drop(held);
    }

    #[test]
    fn clean_counts_files_it_cannot_check_apart_from_held_ones() {
        let dir = temp("clean-failed");
        drop(lock(&dir, &[1; 32]));
        // A lock file that cannot be opened is not held by anyone.
        let dangling = key_path(&dir, &[2; 32]);
        fs::create_dir_all(dangling.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(dir.join("missing"), &dangling).unwrap();
        let c = clean(&dir, Duration::ZERO);
        assert_eq!(c, Cleaned { removed: 1, held: 0, failed: 1 });
    }

    #[test]
    fn one_gc_at_a_time() {
        let dir = temp("gc");
        let first = try_gc_lock(&dir).unwrap();
        assert!(try_gc_lock(&dir).is_none());
        drop(first);
        assert!(try_gc_lock(&dir).is_some());
    }
}
