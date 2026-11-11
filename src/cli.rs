//! Command-line tools (DESIGN section 12), selected only when invoked as `z3cache`.
//!
//! Reports follow AGENTS rule 10: a hit is counted only when a response was
//! written from a stored entry without consulting the solver.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::analyze;
use crate::config::{Config, find_real_solver};
use crate::lock;
use crate::profile::Z3;
use crate::solver::hash_executable;
use crate::store::{FILE_NAME, SolverStamp, SqliteStore, Store, day, now_s, u64_to_sql};

pub const SUBCOMMANDS: &[&str] = &["analyze", "clear", "doctor", "gc", "help", "stats"];

pub fn is_subcommand(arg: &OsString) -> bool {
    SUBCOMMANDS
        .iter()
        .any(|s| arg.as_encoded_bytes() == s.as_bytes())
}

const USAGE: &str = "\
usage:
  z3cache stats [--json]            what the cache has answered, and its size
  z3cache clear                     delete every cached entry
  z3cache gc                        evict to Z3CACHE_MAX_SIZE and remove stale lock files
  z3cache doctor                    check the real solver, the cache directory, and one query
  z3cache analyze <record-dir>...   report on sessions recorded with Z3CACHE_RECORD
  z3cache help                      show this message

Invoked with z3's arguments (or as `z3`), z3cache acts as the solver.
";

/// Runs a subcommand and returns the exit code. `args[0]` is the subcommand.
pub fn run(args: &[OsString]) -> i32 {
    let sub = args[0].to_string_lossy();
    let rest: Vec<String> = args[1..]
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match &*sub {
        "analyze" => analyze_cmd(&args[1..]),
        "stats" => match rest.as_slice() {
            [] => stats(false),
            [j] if j == "--json" => stats(true),
            _ => usage_error(),
        },
        "clear" if rest.is_empty() => clear(),
        "doctor" if rest.is_empty() => doctor(),
        "help" => {
            print!("{USAGE}");
            0
        }
        "gc" if rest.is_empty() => gc(),
        _ => usage_error(),
    }
}

fn usage_error() -> i32 {
    eprint!("{USAGE}");
    2
}

fn analyze_cmd(dirs: &[OsString]) -> i32 {
    if dirs.is_empty() {
        return usage_error();
    }
    let mut recordings = Vec::new();
    for dir in dirs {
        let dir = PathBuf::from(dir);
        match analyze::load(&dir) {
            Ok(r) => recordings.push(r),
            Err(e) => {
                eprintln!("z3cache: cannot read {}: {e}", dir.display());
                return 1;
            }
        }
    }
    print!("{}", analyze::render(&analyze::summarize(&recordings)));
    0
}

fn cache_dir(cfg: &Config) -> Result<&Path, String> {
    cfg.dir
        .as_deref()
        .ok_or_else(|| "no cache directory: set Z3CACHE_DIR".to_string())
}

fn store_exists(dir: &Path) -> bool {
    dir.join(FILE_NAME).exists()
}

fn open_store(cfg: &Config) -> Result<SqliteStore, String> {
    let dir = cache_dir(cfg)?;
    SqliteStore::open(dir).map_err(|e| format!("cannot open the cache in {}: {e}", dir.display()))
}

/// Counter names shown by `stats`, with their labels.
pub(crate) const COUNTERS: &[(&str, &str)] = &[
    ("sessions", "sessions"),
    ("hits", "hits (answered from the cache)"),
    ("misses", "misses (answered by the solver)"),
    ("stores", "entries stored"),
    ("saved_us", "solver time saved"),
    ("solver_starts", "solver starts"),
    ("replay_mismatches", "replay mismatches"),
    ("errored_sessions", "sessions with solver errors"),
    ("bypassed_sessions", "bypassed sessions"),
    ("lock_waits", "waits for another session's solve"),
    ("lock_hits", "hits found after taking the lock"),
    ("lock_timeouts", "lock waits given up (solved anyway)"),
    ("audits", "audited hits (not counted as hits)"),
    ("audit_contradictions", "audit contradictions"),
    ("audit_instabilities", "audit instabilities"),
    ("audit_failures", "audits that could not run"),
    ("gc_deleted", "entries evicted"),
];

/// Counter totals for today, the last 7 days and all time.
struct Totals {
    today: BTreeMap<String, i64>,
    week: BTreeMap<String, i64>,
    all: BTreeMap<String, i64>,
}

fn totals(rows: &[(String, String, i64)]) -> Totals {
    let now = now_s();
    let today = day(now);
    let week_start = day(now - 6 * 86_400);
    let mut t = Totals {
        today: BTreeMap::new(),
        week: BTreeMap::new(),
        all: BTreeMap::new(),
    };
    for (d, name, n) in rows {
        *t.all.entry(name.clone()).or_default() += n;
        if *d >= week_start {
            *t.week.entry(name.clone()).or_default() += n;
        }
        if *d == today {
            *t.today.entry(name.clone()).or_default() += n;
        }
    }
    t
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn json_period(m: &BTreeMap<String, i64>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (name, _) in COUNTERS {
        let n = m.get(*name).copied().unwrap_or(0);
        if *name == "saved_us" {
            parts.push(format!("\"saved_seconds\": {:.3}", n as f64 / 1e6));
        } else {
            parts.push(format!("{}: {n}", json_str(name)));
        }
    }
    let bypass: Vec<String> = m
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("bypass: ")
                .map(|r| format!("{}: {v}", json_str(r)))
        })
        .collect();
    parts.push(format!("\"bypass_reasons\": {{{}}}", bypass.join(", ")));
    format!("{{{}}}", parts.join(", "))
}

fn stats(json: bool) -> i32 {
    let cfg = Config::from_env();
    let Ok(dir) = cache_dir(&cfg).inspect_err(|e| eprintln!("z3cache: {e}")) else {
        return 1;
    };
    if !store_exists(dir) {
        if json {
            println!(
                "{{\"path\": {}, \"entries\": 0, \"bytes\": 0}}",
                json_str(&dir.join(FILE_NAME).display().to_string())
            );
        } else {
            println!("no cache yet in {}", dir.display());
        }
        return 0;
    }
    // Reading statistics must not create, migrate or otherwise change the
    // store.
    let mut store = match SqliteStore::open_read_only(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("z3cache: cannot read the cache: {e}");
            return 1;
        }
    };
    let (entries, rows) = match (store.count(), store.counters()) {
        (Ok(n), Ok(rows)) => (n, rows),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("z3cache: cannot read the cache: {e}");
            return 1;
        }
    };
    let t = totals(&rows);
    let bytes = store.file_bytes();
    if json {
        println!(
            "{{\"path\": {}, \"entries\": {entries}, \"bytes\": {bytes}, \"today\": {}, \"last_7_days\": {}, \"all_time\": {}}}",
            json_str(&store.path().display().to_string()),
            json_period(&t.today),
            json_period(&t.week),
            json_period(&t.all)
        );
        return 0;
    }
    let mut o = String::new();
    let _ = writeln!(o, "cache    {}", store.path().display());
    let _ = writeln!(o, "entries  {entries}, {}", mb(bytes));
    let _ = writeln!(
        o,
        "\n{:<34} {:>12} {:>12} {:>12}",
        "", "today", "7 days", "all time"
    );
    let cell = |m: &BTreeMap<String, i64>, name: &str| {
        let n = m.get(name).copied().unwrap_or(0);
        if name == "saved_us" {
            format!("{:.1} s", n as f64 / 1e6)
        } else {
            n.to_string()
        }
    };
    let mut row = |label: &str, name: &str| {
        let _ = writeln!(
            o,
            "{label:<34} {:>12} {:>12} {:>12}",
            cell(&t.today, name),
            cell(&t.week, name),
            cell(&t.all, name)
        );
    };
    for (name, label) in COUNTERS {
        row(label, name);
    }
    let reasons: Vec<String> = t
        .all
        .keys()
        .filter(|k| k.starts_with("bypass: "))
        .cloned()
        .collect();
    for k in reasons {
        row(&format!("  {}", k.trim_start_matches("bypass: ")), &k);
    }
    print!("{o}");
    0
}

fn clear() -> i32 {
    let cfg = Config::from_env();
    let mut store = match open_store(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("z3cache: {e}");
            return 1;
        }
    };
    match store.clear() {
        Ok(n) => {
            println!("deleted {n} entries from {}", store.path().display());
            0
        }
        Err(e) => {
            eprintln!("z3cache: cannot clear the cache: {e}");
            1
        }
    }
}

/// Lock files untouched for this long are removed by `gc` if no session
/// holds them. A lock file is created by the first miss on its key and then
/// shared by every session that misses the same key; a burst of worktrees
/// verifying the same commit finishes well within an hour, and a file
/// removed too early only costs a duplicate solve (lock::clean).
const LOCK_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

fn gc() -> i32 {
    let cfg = Config::from_env();
    for w in &cfg.warnings {
        eprintln!("z3cache: {w}");
    }
    let Ok(dir) = cache_dir(&cfg).inspect_err(|e| eprintln!("z3cache: {e}")) else {
        return 1;
    };
    if !store_exists(dir) {
        println!("no cache yet in {}", dir.display());
        return 0;
    }
    let Some(_gc_lock) = lock::try_gc_lock(dir) else {
        println!("another z3cache gc is running");
        return 0;
    };
    let mut store = match open_store(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("z3cache: {e}");
            return 1;
        }
    };
    let cap = cfg.max_size;
    println!("cache    {}", store.path().display());
    println!(
        "cap      {} (Z3CACHE_MAX_SIZE); eviction goes down to 80%",
        mb(cap)
    );
    let deleted = match store.evict(cap) {
        Ok(e) if e.deleted == 0 => {
            println!(
                "store    {}, within the cap: nothing evicted",
                mb(e.bytes_before)
            );
            0
        }
        Ok(e) => {
            println!(
                "store    {} -> {}: evicted {} least-recently-used entries, {} left",
                mb(e.bytes_before),
                mb(e.bytes_after),
                e.deleted,
                e.entries_left
            );
            e.deleted
        }
        Err(e) => {
            eprintln!("z3cache: eviction failed: {e}");
            return 1;
        }
    };
    let c = lock::clean(dir, LOCK_MAX_AGE);
    let failed = match c.failed {
        0 => String::new(),
        n => format!("; {n} could not be checked"),
    };
    println!(
        "locks    removed {} lock files unused for an hour; {} old ones are held{failed}",
        c.removed, c.held
    );
    let counters = [("gc_deleted".to_string(), u64_to_sql(deleted))];
    if let Err(e) = store.finish(&[], &counters) {
        eprintln!("z3cache: cannot update counters: {e}");
    }
    0
}

fn doctor() -> i32 {
    let cfg = Config::from_env();
    let mut ok = true;
    for w in &cfg.warnings {
        println!("warning       {w}");
    }
    let real = match find_real_solver(&cfg, &Z3) {
        Ok(p) => {
            let how = if cfg.real_z3.is_some() {
                "Z3CACHE_REAL_Z3"
            } else {
                "first z3 on PATH that is not z3cache"
            };
            println!("real solver   {} (from {how})", p.display());
            Some(p)
        }
        Err(e) => {
            println!("real solver   NOT FOUND: {e}");
            ok = false;
            None
        }
    };

    let mut store = None;
    match &cfg.dir {
        None => {
            println!("cache dir     NONE: set Z3CACHE_DIR");
            ok = false;
        }
        Some(dir) => match check_writable(dir) {
            Ok(()) => {
                println!("cache dir     {} (writable)", dir.display());
                match SqliteStore::open(dir) {
                    Ok(mut s) => {
                        let n = s
                            .count()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|e| format!("? ({e})"));
                        println!(
                            "store         {}: {n} entries, {}",
                            s.path().display(),
                            mb(s.file_bytes())
                        );
                        store = Some(s);
                    }
                    Err(e) => {
                        println!("store         CANNOT OPEN: {e} (sessions run uncached)");
                        ok = false;
                    }
                }
            }
            Err(e) => {
                println!("cache dir     {} NOT WRITABLE: {e}", dir.display());
                ok = false;
            }
        },
    }

    if let Some(real) = &real {
        let memo = SolverStamp::of(real)
            .ok()
            .and_then(|st| store.as_mut().and_then(|s| s.solver(&st).ok().flatten()));
        match memo {
            Some(info) => {
                println!(
                    "identity      {} (memoised)",
                    blake3::Hash::from(info.id).to_hex()
                );
                match info.version_response {
                    Some(v) => println!("version       {}", String::from_utf8_lossy(&v).trim()),
                    None => println!("version       not seen yet"),
                }
            }
            None => match hash_executable(real) {
                Ok(id) => println!(
                    "identity      {} (not memoised yet)",
                    blake3::Hash::from(id).to_hex()
                ),
                Err(e) => {
                    println!("identity      CANNOT HASH: {e}");
                    ok = false;
                }
            },
        }
        match round_trip(real) {
            Ok(report) => println!("round trip    {report}"),
            Err(e) => {
                println!("round trip    FAILED: {e}");
                ok = false;
            }
        }
    }
    if ok { 0 } else { 1 }
}

fn check_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)
}

/// Runs one small query through this program twice against a temporary
/// cache: the first must reach the solver, the second must not.
fn round_trip(real: &Path) -> Result<String, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = std::env::temp_dir().join(format!("z3cache-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let input = b"(declare-const x Int)\n(assert (and (> x 0) (< x 0)))\n(check-sat)\n";
    let run = || -> Result<(String, f64), String> {
        let start = Instant::now();
        let mut child = Command::new(&me)
            .args(["-smt2", "-in"])
            .env("Z3CACHE_DIR", &dir)
            .env("Z3CACHE_REAL_Z3", real)
            .env_remove("Z3CACHE_MODE")
            .env_remove("Z3CACHE_RECORD")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(input)
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        Ok((String::from_utf8_lossy(&out.stdout).into_owned(), ms))
    };
    let starts = || -> Result<i64, String> {
        let mut s = SqliteStore::open(&dir).map_err(|e| e.to_string())?;
        let rows = s.counters().map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter(|r| r.1 == "solver_starts")
            .map(|r| r.2)
            .sum())
    };
    let result = (|| {
        let (cold, cold_ms) = run()?;
        let after_cold = starts()?;
        let (warm, warm_ms) = run()?;
        let after_warm = starts()?;
        if cold != "unsat\n" || warm != "unsat\n" {
            return Err(format!("expected unsat twice, got {cold:?} then {warm:?}"));
        }
        if after_cold != 1 {
            return Err(format!("cold run started {after_cold} solvers, expected 1"));
        }
        if after_warm != after_cold {
            return Err(
                "warm run started a solver: the answer was not served from the cache".into(),
            );
        }
        Ok(format!(
            "cold {cold_ms:.1} ms (solver started), warm {warm_ms:.1} ms (answered from the cache, no solver started)"
        ))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}
