//! Tier 4: many proxy processes on one store (DESIGN section 18), with the
//! stub solver.

mod common;

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

const PROXY_ARGS: &[&str] = &["-smt2", "-in"];

fn spawn(cache: &Path, log: &Path, input: &[u8], extra: &[(&str, &OsStr)]) -> Child {
    let mut cmd = Command::new(z3cache());
    cmd.args(PROXY_ARGS)
        .env("Z3CACHE_REAL_Z3", stub())
        .env("Z3CACHE_DIR", cache)
        .env("Z3STUB_LOG", log)
        .env_remove("Z3CACHE_MODE")
        .env_remove("Z3CACHE_RECORD")
        .env_remove("Z3CACHE_STORE")
        .env_remove("Z3CACHE_VERIFY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    child
}

fn counter(cache: &Path, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(cache.join("cache-v1.sqlite3")).unwrap();
    conn.query_row("SELECT coalesce(sum(n), 0) FROM counters WHERE name = ?1", [name], |r| r.get(0))
        .unwrap()
}

fn integrity_ok(cache: &Path) {
    let conn = rusqlite::Connection::open(cache.join("cache-v1.sqlite3")).unwrap();
    let r: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(r, "ok");
}

#[test]
fn concurrent_misses_on_one_key_are_solved_once() {
    // Handshake: the holder's stub holds its answer behind a gate, so the
    // holder keeps the lock until every waiter has logged that it found the
    // lock busy; only then is the gate opened.
    let dir = temp_dir("dedup");
    let cache = dir.join("cache");
    let log = dir.join("stub.log");
    let gate = dir.join("gate");
    let input = b"(assert a)\n(check-sat)\n";
    let answer = format!("gate:{}:unsat", gate.display());
    let holder = spawn(&cache, &log, input, &[("Z3STUB_ANSWERS", OsStr::new(&answer))]);
    wait_until("the holder is solving under its lock", || file_contains(&log, "(check-sat)"));
    let waiters: Vec<(Child, std::path::PathBuf)> = (0..7)
        .map(|i| {
            let wlog = dir.join(format!("waiter{i}.log"));
            (spawn(&cache, &log, input, &[("Z3CACHE_LOG", wlog.as_os_str())]), wlog)
        })
        .collect();
    wait_until("every waiter found the lock busy", || {
        waiters.iter().all(|(_, wlog)| file_contains(wlog, LOCK_BUSY))
    });
    std::fs::write(&gate, b"").unwrap();
    let mut children = vec![holder];
    children.extend(waiters.into_iter().map(|(c, _)| c));
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unsat\n");
        assert!(out.status.success());
    }
    let solves = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|l| *l == "(check-sat)")
        .count();
    assert_eq!(solves, 1, "the in-flight lock must let one session solve");
    assert_eq!(counter(&cache, "misses"), 1);
    assert_eq!(counter(&cache, "hits"), 7);
    assert_eq!(counter(&cache, "lock_waits"), 7);
    assert_eq!(counter(&cache, "lock_hits"), 7);
}

#[test]
fn a_failing_holder_does_not_stop_the_waiters() {
    // The holder's answer is sat, which is not stored: each waiter solves.
    let dir = temp_dir("dedup-sat");
    let cache = dir.join("cache");
    let log = dir.join("stub.log");
    let slow_sat = ("Z3STUB_ANSWERS", OsStr::new("sleep:200:sat"));
    let children: Vec<Child> = (0..4)
        .map(|_| spawn(&cache, &log, b"(assert a)\n(check-sat)\n", &[slow_sat]))
        .collect();
    for c in children {
        let out = c.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "sat\n");
    }
    assert_eq!(counter(&cache, "misses"), 4);
    assert_eq!(counter(&cache, "hits"), 0);
}

fn many_queries(offset: usize, n: usize) -> Vec<u8> {
    let mut s = String::from("(declare-const x Int)\n");
    for i in offset..offset + n {
        s.push_str(&format!("(push)(assert (> x {i}))(check-sat)(pop)\n"));
    }
    s.into_bytes()
}

#[test]
fn killing_sessions_mid_write_leaves_a_valid_store() {
    let dir = temp_dir("kill9");
    let cache = dir.join("cache");
    let log = dir.join("stub.log");
    // Create the store first so that kills land during entry writes.
    spawn(&cache, &log, &many_queries(0, 1), &[]).wait().unwrap();
    for round in 0..3 {
        let mut children: Vec<Child> = (0..8)
            .map(|i| spawn(&cache, &log, &many_queries(1000 * (round * 8 + i), 300), &[]))
            .collect();
        for (i, c) in children.iter_mut().enumerate() {
            std::thread::sleep(Duration::from_millis(5 + 7 * i as u64));
            unsafe { libc::kill(c.id() as i32, libc::SIGKILL) };
        }
        for mut c in children {
            let _ = c.wait();
        }
    }
    integrity_ok(&cache);
    // The store still works: a new entry is stored and then hit.
    let input = many_queries(999_999, 1);
    let first = spawn(&cache, &log, &input, &[]).wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&first.stdout), "unsat\n");
    let hits = counter(&cache, "hits");
    let second = spawn(&cache, &log, &input, &[]).wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&second.stdout), "unsat\n");
    assert_eq!(counter(&cache, "hits"), hits + 1);
    integrity_ok(&cache);
}

#[test]
fn gc_while_sessions_run() {
    let dir = temp_dir("gc-concurrent");
    let cache = dir.join("cache");
    let log = dir.join("stub.log");
    spawn(&cache, &log, &many_queries(0, 1), &[]).wait().unwrap();
    let small = ("Z3CACHE_MAX_SIZE", OsStr::new("96K"));
    let children: Vec<Child> = (0..6)
        .map(|i| spawn(&cache, &log, &many_queries(1000 * i, 200), &[small]))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut gcs = 0;
    let mut outputs = Vec::new();
    let mut pending: Vec<Child> = children;
    while !pending.is_empty() {
        assert!(Instant::now() < deadline, "sessions did not finish");
        let gc = run(&z3cache(), &["gc"], b"", &[("Z3CACHE_DIR", cache.as_os_str()), small]);
        assert_eq!(gc.code, 0, "{}", gc.text());
        gcs += 1;
        let mut still = Vec::new();
        for mut c in pending {
            if c.try_wait().unwrap().is_some() {
                outputs.push(c.wait_with_output().unwrap());
            } else {
                still.push(c);
            }
        }
        pending = still;
    }
    assert!(gcs > 1);
    for out in outputs {
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "unsat\n".repeat(200));
    }
    integrity_ok(&cache);
    let gc = run(&z3cache(), &["gc"], b"", &[("Z3CACHE_DIR", cache.as_os_str()), small]);
    assert_eq!(gc.code, 0);
    let conn = rusqlite::Connection::open(cache.join("cache-v1.sqlite3")).unwrap();
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    assert!(pages * size <= 96 * 1024, "{} bytes after gc", pages * size);
}

#[test]
fn hits_found_after_waiting_for_a_lock_are_audited_too() {
    // M2 review finding 3: the waiter's stub contradicts the holder's. Same
    // handshake as above: the holder answers only once the waiter has
    // logged that it is waiting for the lock.
    let dir = temp_dir("audit-after-lock");
    let cache = dir.join("cache");
    let log = dir.join("stub.log");
    let gate = dir.join("gate");
    let wlog = dir.join("waiter.log");
    let input = b"(assert a)\n(check-sat)\n";
    let answer = format!("gate:{}:unsat", gate.display());
    let holder = spawn(&cache, &log, input, &[("Z3STUB_ANSWERS", OsStr::new(&answer))]);
    wait_until("the holder is solving under its lock", || file_contains(&log, "(check-sat)"));
    let waiter = spawn(
        &cache,
        &log,
        input,
        &[
            ("Z3CACHE_VERIFY", OsStr::new("1")),
            ("Z3STUB_ANSWERS", OsStr::new("sat")),
            ("Z3CACHE_LOG", wlog.as_os_str()),
        ],
    );
    wait_until("the waiter found the lock busy", || file_contains(&wlog, LOCK_BUSY));
    std::fs::write(&gate, b"").unwrap();
    let w = waiter.wait_with_output().unwrap();
    let h = holder.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&h.stdout), "unsat\n");
    assert_eq!(String::from_utf8_lossy(&w.stdout), "sat\n", "the audit's answer");
    assert_eq!(counter(&cache, "lock_waits"), 1);
    assert_eq!(counter(&cache, "audits"), 1);
    assert_eq!(counter(&cache, "audit_contradictions"), 1);
    // An audited answer is neither a hit nor a lock hit (rule 10).
    assert_eq!(counter(&cache, "lock_hits"), 0);
    assert_eq!(counter(&cache, "hits"), 0);
}
