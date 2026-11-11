//! SQLite implementation of [`Store`]: one file shared by every worktree.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use super::{Entry, Eviction, Key, Result, SolverInfo, SolverStamp, Store, StoreError, day, now_s};

pub const FILE_NAME: &str = "cache-v1.sqlite3";

/// `PRAGMA user_version` of the current schema. A file with another version
/// is emptied and recreated: the cache is disposable.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE solver (
  path             BLOB    NOT NULL,
  size             INTEGER NOT NULL,
  mtime_ns         INTEGER NOT NULL,
  id               BLOB    NOT NULL,
  version_response BLOB,
  PRIMARY KEY (path, size, mtime_ns)
);
CREATE TABLE entry (
  key         BLOB    PRIMARY KEY,
  response    BLOB    NOT NULL,
  is_query    INTEGER NOT NULL,
  rlimit      INTEGER,
  solve_us    INTEGER NOT NULL,
  created_s   INTEGER NOT NULL,
  last_used_s INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX entry_lru ON entry (last_used_s);
CREATE TABLE counters (
  day  TEXT    NOT NULL,
  name TEXT    NOT NULL,
  n    INTEGER NOT NULL,
  PRIMARY KEY (day, name)
);
";

pub struct SqliteStore {
    conn: Connection,
    path: PathBuf,
}

impl SqliteStore {
    /// Opens (creating if needed) the store in `dir`.
    pub fn open(dir: &Path) -> Result<SqliteStore> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let conn = Connection::open(&path)?;
        conn.busy_timeout(Duration::from_millis(2000))?;
        // Only takes effect on a new, empty file.
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError(format!("journal mode is {mode}, not wal")));
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        let mut store = SqliteStore { conn, path };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Opens an existing store for reading only: no directory or file is
    /// created, nothing is written, and a store with another schema version
    /// is an error rather than being replaced. Used by `ro` and `enforce`
    /// sessions and by `stats`.
    pub fn open_read_only(dir: &Path) -> Result<SqliteStore> {
        let path = dir.join(FILE_NAME);
        if !path.is_file() {
            return Err(StoreError(format!("no cache at {}", path.display())));
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_millis(2000))?;
        let store = SqliteStore { conn, path };
        let version = store.user_version()?;
        if version != SCHEMA_VERSION {
            return Err(StoreError(format!(
                "cache schema version {version}, expected {SCHEMA_VERSION}"
            )));
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size of the database file and its write-ahead log, in bytes.
    pub fn file_bytes(&self) -> u64 {
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut wal = self.path.clone().into_os_string();
        wal.push("-wal");
        size(&self.path) + size(Path::new(&wal))
    }

    /// Size of the database as SQLite sees it (all pages, including free
    /// ones and those still only in the write-ahead log). This, not the file
    /// size, is compared with the cap: the log can briefly hold several
    /// copies of a page.
    pub fn db_bytes(&self) -> Result<u64> {
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((pages.max(0) * size) as u64)
    }

    /// Returns free pages to the file system. The pragma frees one page per
    /// step, so it must be stepped to completion.
    fn incremental_vacuum(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA incremental_vacuum")?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Bytes in pages that hold data (not on the free list).
    pub fn used_bytes(&self) -> Result<u64> {
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: i64 = self.conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok(((pages - free).max(0) * size) as u64)
    }

    fn user_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn ensure_schema(&mut self) -> Result<()> {
        if self.user_version()? == SCHEMA_VERSION {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            tx.execute_batch(
                "DROP TABLE IF EXISTS entry;
                 DROP TABLE IF EXISTS solver;
                 DROP TABLE IF EXISTS counters;",
            )?;
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn u64_to_sql(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Store for SqliteStore {
    fn get(&mut self, key: &Key) -> Result<Option<Entry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT response, is_query, rlimit, solve_us, last_used_s FROM entry WHERE key = ?1",
        )?;
        let entry = stmt
            .query_row(params![&key[..]], |r| {
                Ok(Entry {
                    response: r.get(0)?,
                    is_query: r.get::<_, i64>(1)? != 0,
                    rlimit: r.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u64),
                    solve_us: r.get::<_, i64>(3)?.max(0) as u64,
                    last_used_s: r.get(4)?,
                })
            })
            .optional()?;
        Ok(entry)
    }

    fn put(&mut self, key: &Key, entry: &Entry) -> Result<()> {
        let now = now_s();
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO entry
               (key, response, is_query, rlimit, solve_us, created_s, last_used_s)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        )?;
        stmt.execute(params![
            &key[..],
            &entry.response,
            i64::from(entry.is_query),
            entry.rlimit.map(u64_to_sql),
            u64_to_sql(entry.solve_us),
            now
        ])?;
        Ok(())
    }

    fn finish(&mut self, touched: &[Key], counters: &[(String, i64)]) -> Result<()> {
        if touched.is_empty() && counters.is_empty() {
            return Ok(());
        }
        let now = now_s();
        let today = day(now);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut touch = tx.prepare_cached("UPDATE entry SET last_used_s = ?1 WHERE key = ?2")?;
            for key in touched {
                touch.execute(params![now, &key[..]])?;
            }
            let mut add = tx.prepare_cached(
                "INSERT INTO counters (day, name, n) VALUES (?1, ?2, ?3)
                 ON CONFLICT (day, name) DO UPDATE SET n = n + excluded.n",
            )?;
            for (name, n) in counters {
                if *n != 0 {
                    add.execute(params![today, name, n])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn solver(&mut self, stamp: &SolverStamp) -> Result<Option<SolverInfo>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, version_response FROM solver
             WHERE path = ?1 AND size = ?2 AND mtime_ns = ?3",
        )?;
        let row = stmt
            .query_row(
                params![&stamp.path, u64_to_sql(stamp.size), stamp.mtime_ns],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(id, version_response)| {
            Some(SolverInfo {
                id: id.try_into().ok()?,
                version_response,
            })
        }))
    }

    fn put_solver(&mut self, stamp: &SolverStamp, info: &SolverInfo) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO solver (path, size, mtime_ns, id, version_response)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        stmt.execute(params![
            &stamp.path,
            u64_to_sql(stamp.size),
            stamp.mtime_ns,
            &info.id[..],
            info.version_response
        ])?;
        Ok(())
    }

    fn count(&mut self) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT count(*) FROM entry", [], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    fn clear(&mut self) -> Result<u64> {
        let n = self.conn.execute("DELETE FROM entry", [])?;
        self.incremental_vacuum()?;
        Ok(n as u64)
    }

    fn delete(&mut self, key: &Key) -> Result<bool> {
        let n = self.conn.execute("DELETE FROM entry WHERE key = ?1", params![&key[..]])?;
        Ok(n > 0)
    }

    fn evict(&mut self, cap: u64) -> Result<Eviction> {
        let bytes_before = self.file_bytes();
        let target = cap / 10 * 8;
        let mut deleted = 0u64;
        if self.db_bytes()? > cap {
            // Batches sized from the average entry, each its own transaction,
            // so that sessions writing at the same time are never held up
            // for long and eviction stops close to the target.
            loop {
                let used = self.used_bytes()?;
                if used <= target {
                    break;
                }
                let entries = self.count()?.max(1);
                let per_entry = (used / entries).max(1);
                let batch = ((used - target) / per_entry + 1).min(1000);
                let n = self.conn.execute(
                    "DELETE FROM entry WHERE key IN
                       (SELECT key FROM entry ORDER BY last_used_s, created_s LIMIT ?1)",
                    params![i64::try_from(batch).unwrap_or(1000)],
                )?;
                if n == 0 {
                    break;
                }
                deleted += n as u64;
            }
            self.incremental_vacuum()?;
            // Shrinks the write-ahead log if no reader needs it; a busy
            // result is fine.
            let _ = self
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        }
        Ok(Eviction {
            bytes_before,
            bytes_after: self.file_bytes(),
            deleted,
            entries_left: self.count()?,
        })
    }

    fn counters(&mut self) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT day, name, n FROM counters ORDER BY day, name")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("z3cache-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn entry(response: &[u8]) -> Entry {
        Entry {
            response: response.to_vec(),
            is_query: true,
            rlimit: Some(300),
            solve_us: 1234,
            last_used_s: 0,
        }
    }

    #[test]
    fn put_get_and_first_writer_wins() {
        let dir = temp("putget");
        let mut s = SqliteStore::open(&dir).unwrap();
        let key = [1u8; 32];
        assert_eq!(s.get(&key).unwrap(), None);
        s.put(&key, &entry(b"unsat\n")).unwrap();
        s.put(&key, &entry(b"other\n")).unwrap();
        let got = s.get(&key).unwrap().unwrap();
        assert_eq!(got.response, b"unsat\n");
        assert_eq!(got.rlimit, Some(300));
        assert_eq!(got.solve_us, 1234);
        assert!(got.last_used_s > 0);
        assert_eq!(s.count().unwrap(), 1);
        // Visible to another connection immediately.
        let mut other = SqliteStore::open(&dir).unwrap();
        assert!(other.get(&key).unwrap().is_some());
        assert_eq!(s.clear().unwrap(), 1);
        assert_eq!(other.get(&key).unwrap(), None);
    }

    #[test]
    fn delete_one() {
        let dir = temp("delete");
        let mut s = SqliteStore::open(&dir).unwrap();
        s.put(&[1; 32], &entry(b"unsat\n")).unwrap();
        assert!(s.delete(&[1; 32]).unwrap());
        assert!(!s.delete(&[1; 32]).unwrap());
        assert_eq!(s.get(&[1; 32]).unwrap(), None);
    }

    #[test]
    fn evict_removes_least_recently_used_first() {
        let dir = temp("evict");
        let mut s = SqliteStore::open(&dir).unwrap();
        let big = vec![b'x'; 4000];
        for i in 0..400u32 {
            let mut key = [0u8; 32];
            key[..4].copy_from_slice(&i.to_le_bytes());
            s.put(&key, &entry(&big)).unwrap();
            // Older entries were used longer ago.
            s.conn
                .execute("UPDATE entry SET last_used_s = ?1 WHERE key = ?2", params![i64::from(i), &key[..]])
                .unwrap();
        }
        let size = s.db_bytes().unwrap();
        // Under the cap: nothing happens.
        let none = s.evict(size * 2).unwrap();
        assert_eq!(none.deleted, 0);
        let cap = size / 2;
        let e = s.evict(cap).unwrap();
        assert!(e.deleted > 0);
        assert!(s.used_bytes().unwrap() <= cap / 10 * 8);
        assert_eq!(e.entries_left, 400 - e.deleted);
        // The newest entry survived, the oldest did not.
        let mut newest = [0u8; 32];
        newest[..4].copy_from_slice(&399u32.to_le_bytes());
        assert!(s.get(&newest).unwrap().is_some(), "{e:?} used {}", s.used_bytes().unwrap());
        assert!(s.get(&[0u8; 32]).unwrap().is_none());
    }

    #[test]
    fn counters_accumulate() {
        let dir = temp("counters");
        let mut s = SqliteStore::open(&dir).unwrap();
        s.finish(&[], &[("hits".into(), 2), ("misses".into(), 1)]).unwrap();
        s.finish(&[], &[("hits".into(), 3), ("zero".into(), 0)]).unwrap();
        let c = s.counters().unwrap();
        let get = |name: &str| c.iter().find(|r| r.1 == name).map(|r| r.2);
        assert_eq!(get("hits"), Some(5));
        assert_eq!(get("misses"), Some(1));
        assert_eq!(get("zero"), None);
    }

    #[test]
    fn solver_memo() {
        let dir = temp("solver");
        let mut s = SqliteStore::open(&dir).unwrap();
        let stamp = SolverStamp {
            path: b"/bin/z3".to_vec(),
            size: 10,
            mtime_ns: 5,
        };
        assert_eq!(s.solver(&stamp).unwrap(), None);
        let mut info = SolverInfo {
            id: [9; 32],
            version_response: None,
        };
        s.put_solver(&stamp, &info).unwrap();
        assert_eq!(s.solver(&stamp).unwrap(), Some(info.clone()));
        info.version_response = Some(b"(:version \"4.16.0\")\n".to_vec());
        s.put_solver(&stamp, &info).unwrap();
        assert_eq!(s.solver(&stamp).unwrap(), Some(info));
        let moved = SolverStamp {
            mtime_ns: 6,
            ..stamp
        };
        assert_eq!(s.solver(&moved).unwrap(), None);
    }

    #[test]
    fn other_schema_version_is_replaced() {
        let dir = temp("schema");
        {
            let mut s = SqliteStore::open(&dir).unwrap();
            s.put(&[1; 32], &entry(b"unsat\n")).unwrap();
            s.conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        }
        let mut s = SqliteStore::open(&dir).unwrap();
        assert_eq!(s.count().unwrap(), 0);
        assert_eq!(s.user_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn read_only_open_never_writes() {
        let dir = temp("readonly");
        // Missing: an error, and nothing is created.
        assert!(SqliteStore::open_read_only(&dir).is_err());
        assert!(!dir.exists());
        {
            let mut s = SqliteStore::open(&dir).unwrap();
            s.put(&[1; 32], &entry(b"unsat\n")).unwrap();
        }
        // After every writer has closed (no -wal or -shm left), reads work.
        let mut ro = SqliteStore::open_read_only(&dir).unwrap();
        assert_eq!(ro.get(&[1; 32]).unwrap().unwrap().response, b"unsat\n");
        assert!(ro.put(&[2; 32], &entry(b"unsat\n")).is_err());
        drop(ro);
        // Another schema version: an error, and the file is left alone.
        {
            let s = SqliteStore::open(&dir).unwrap();
            s.conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        }
        assert!(SqliteStore::open_read_only(&dir).is_err());
        let raw = Connection::open(dir.join(FILE_NAME)).unwrap();
        let v: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 999);
        let n: i64 = raw.query_row("SELECT count(*) FROM entry", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn garbage_file_is_an_error_not_a_panic() {
        let dir = temp("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), vec![0x55u8; 8192]).unwrap();
        assert!(SqliteStore::open(&dir).is_err());
    }
}
