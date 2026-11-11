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
use crate::config::{Config, FileStatus, Source, find_real_solver};
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
  z3cache stats [--all|--json]      what the cache has answered, and its size
  z3cache clear                     delete every cached entry
  z3cache gc                        evict to max_size and remove stale lock files
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
            [] => stats(StatsView::Summary),
            [a] if a == "--all" => stats(StatsView::All),
            [j] if j == "--json" => stats(StatsView::Json),
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
        .ok_or_else(|| format!("no cache directory: {}", cfg.how_to_set("dir")))
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

fn stats(view: StatsView) -> i32 {
    let cfg = Config::load();
    let Ok(dir) = cache_dir(&cfg).inspect_err(|e| eprintln!("z3cache: {e}")) else {
        return 1;
    };
    if !store_exists(dir) {
        if view == StatsView::Json {
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
    if view == StatsView::Json {
        println!(
            "{{\"path\": {}, \"entries\": {entries}, \"bytes\": {bytes}, \"today\": {}, \"last_7_days\": {}, \"all_time\": {}}}",
            json_str(&store.path().display().to_string()),
            json_period(&t.today),
            json_period(&t.week),
            json_period(&t.all)
        );
        return 0;
    }
    print!(
        "{}",
        render_stats(
            &store.path().display().to_string(),
            entries,
            bytes,
            &t,
            view == StatsView::All
        )
    );
    0
}

/// How `z3cache stats` shows its numbers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatsView {
    /// The main rows, plus any other row that is not zero.
    Summary,
    /// Every row.
    All,
    Json,
}

/// Rows of the `stats` table, in order: (counter or derived name, label).
/// `hit_rate` is derived from `hits` and `misses`.
const MAIN_ROWS: &[(&str, &str)] = &[
    ("hit_rate", "hit rate"),
    ("hits", "answered from cache"),
    ("misses", "solved by Z3"),
    ("saved_us", "solver time saved"),
    ("sessions", "sessions"),
    ("solver_starts", "solver starts"),
    ("stores", "entries stored"),
];

/// Rows shown only when not zero (or with `--all`).
const OPTIONAL_ROWS: &[(&str, &str)] = &[
    ("lock_waits", "waited for another worktree"),
    ("lock_hits", "answered after waiting"),
    ("lock_timeouts", "gave up waiting, solved anyway"),
    ("audits", "hits re-checked against Z3"),
    ("audit_instabilities", "re-checks that came back unknown"),
    ("audit_failures", "re-checks that could not run"),
    ("gc_deleted", "entries evicted"),
];

/// Rows that indicate a problem; listed under "needs attention" when not
/// zero.
const PROBLEM_ROWS: &[(&str, &str)] = &[
    ("replay_mismatches", "replay mismatches"),
    ("errored_sessions", "sessions with solver errors"),
    ("audit_contradictions", "re-checks contradicting cache"),
    ("bypassed_sessions", "bypassed sessions"),
];

fn render_stats(path: &str, entries: u64, bytes: u64, t: &Totals, all: bool) -> String {
    let periods = [&t.today, &t.week, &t.all];
    let get = |m: &BTreeMap<String, i64>, name: &str| m.get(name).copied().unwrap_or(0);
    let cell = |m: &BTreeMap<String, i64>, name: &str| match name {
        "hit_rate" => {
            let (h, s) = (get(m, "hits"), get(m, "misses"));
            if h + s == 0 {
                "-".to_string()
            } else {
                format!("{:.1}%", 100.0 * h as f64 / (h + s) as f64)
            }
        }
        "saved_us" => duration(get(m, name)),
        _ => thousands(get(m, name)),
    };
    let nonzero = |name: &str| periods.iter().any(|m| get(m, name) != 0);

    let mut o = String::new();
    let _ = writeln!(o, "cache    {path}");
    let _ = writeln!(o, "entries  {}, {}", thousands(entries as i64), mb(bytes));
    let _ = writeln!(
        o,
        "\n{:<32} {:>10} {:>10} {:>10}",
        "", "today", "7 days", "all time"
    );
    let row = |o: &mut String, label: &str, name: &str| {
        let _ = writeln!(
            o,
            "{label:<32} {:>10} {:>10} {:>10}",
            cell(&t.today, name),
            cell(&t.week, name),
            cell(&t.all, name)
        );
    };
    for (name, label) in MAIN_ROWS {
        row(&mut o, label, name);
    }
    for (name, label) in OPTIONAL_ROWS {
        if all || nonzero(name) {
            row(&mut o, label, name);
        }
    }

    let reasons: Vec<String> = t
        .all
        .keys()
        .filter(|k| k.starts_with("bypass: "))
        .cloned()
        .collect();
    let problems: Vec<&(&str, &str)> = PROBLEM_ROWS
        .iter()
        .filter(|(name, _)| all || nonzero(name))
        .collect();
    let attention = PROBLEM_ROWS.iter().any(|(name, _)| nonzero(name));
    if problems.is_empty() {
        let _ = writeln!(
            o,
            "\nno bypassed sessions, solver errors or replay mismatches"
        );
    } else {
        let heading = if attention {
            "needs attention (see `z3cache doctor` and Z3CACHE_LOG)"
        } else {
            "problems"
        };
        let _ = writeln!(o, "\n{heading}");
        for (name, label) in problems {
            row(&mut o, label, name);
            if *name == "bypassed_sessions" {
                for k in &reasons {
                    row(
                        &mut o,
                        &format!("  {}", k.trim_start_matches("bypass: ")),
                        k,
                    );
                }
            }
        }
    }
    o
}

/// `1234567` as `1,234,567`.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Microseconds as `45.2 s`, `12m 29s` or `3h 05m`.
fn duration(us: i64) -> String {
    let s = us.max(0) as f64 / 1e6;
    if s < 60.0 {
        format!("{s:.1} s")
    } else if s < 3600.0 {
        let s = s.round() as i64;
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        let m = (s / 60.0).round() as i64;
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

fn clear() -> i32 {
    let cfg = Config::load();
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
    let cfg = Config::load();
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
        "cap      {} (max_size, {}); eviction goes down to 80%",
        mb(cap),
        source_text(&cfg, "max_size")
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
    let cfg = Config::load();
    let mut ok = true;
    for w in &cfg.warnings {
        println!("warning       {w}");
    }
    match (&cfg.file.path, &cfg.file.status) {
        (None, _) => println!("config file   none (HOME is unset)"),
        (Some(p), FileStatus::Found) => println!("config file   {} (found)", p.display()),
        (Some(p), FileStatus::Missing) => println!("config file   {} (not found)", p.display()),
        (Some(p), FileStatus::Unreadable(e)) => {
            println!("config file   {} (UNREADABLE: {e}; ignored)", p.display())
        }
        (Some(p), FileStatus::Invalid(e)) => {
            println!("config file   {} (INVALID TOML: {e}; ignored)", p.display())
        }
    }
    for s in &cfg.settings {
        println!(
            "setting       {} = {} ({})",
            s.key,
            s.value,
            source_text(&cfg, s.key)
        );
    }
    let real = match find_real_solver(&cfg, &Z3) {
        Ok(p) => {
            let how = cfg
                .origin("real_z3")
                .unwrap_or_else(|| "first z3 on PATH that is not z3cache".into());
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
            println!("cache dir     NONE: {}", cfg.how_to_set("dir"));
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

/// Where a setting came from, for `doctor` and `gc`.
fn source_text(cfg: &Config, key: &str) -> String {
    match cfg.source(key) {
        Source::Env => format!("environment, {}", crate::config::env_name(key)),
        Source::File(n) => format!("config file, line {n}"),
        Source::Default => "default".into(),
    }
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
            // The user's config file does not apply to this self-test: its
            // mode or recording would defeat or pollute it.
            .env("Z3CACHE_CONFIG", dir.join("no-config"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn totals_of(pairs: &[(&str, i64)]) -> Totals {
        let m: BTreeMap<String, i64> = pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        Totals {
            today: m.clone(),
            week: m.clone(),
            all: m,
        }
    }

    #[test]
    fn numbers_and_durations() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(19_741), "19,741");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(duration(45_200_000), "45.2 s");
        assert_eq!(duration(748_600_000), "12m 29s");
        assert_eq!(duration(11_100_000_000), "3h 05m");
    }

    #[test]
    fn every_counter_has_a_row() {
        for (name, _) in COUNTERS {
            let shown = [MAIN_ROWS, OPTIONAL_ROWS, PROBLEM_ROWS]
                .iter()
                .any(|rows| rows.iter().any(|(n, _)| n == name));
            assert!(shown, "counter {name} is not in any stats row");
        }
        for (_, label) in MAIN_ROWS.iter().chain(OPTIONAL_ROWS).chain(PROBLEM_ROWS) {
            assert!(
                label.len() <= 32,
                "label {label:?} is wider than the column"
            );
        }
    }

    #[test]
    fn summary_shows_hit_rate_and_hides_zero_rows() {
        let t = totals_of(&[("hits", 3), ("misses", 1), ("sessions", 2)]);
        let text = render_stats("/c", 5, 1000, &t, false);
        assert!(text.contains("hit rate"), "{text}");
        assert!(text.contains("75.0%"), "{text}");
        assert!(!text.contains("waited for another worktree"), "{text}");
        assert!(
            text.contains("no bypassed sessions, solver errors or replay mismatches"),
            "{text}"
        );
        let all = render_stats("/c", 5, 1000, &t, true);
        assert!(all.contains("waited for another worktree"), "{all}");
        assert!(all.contains("\nproblems\n"), "{all}");
        assert!(!all.contains("needs attention"), "{all}");
    }

    #[test]
    fn problems_are_listed_with_bypass_reasons() {
        let t = totals_of(&[
            ("hits", 1),
            ("bypassed_sessions", 2),
            ("bypass: command reset", 2),
        ]);
        let text = render_stats("/c", 1, 1000, &t, false);
        assert!(text.contains("needs attention"), "{text}");
        assert!(text.contains("bypassed sessions"), "{text}");
        assert!(text.contains("  command reset"), "{text}");
        assert!(!text.contains("replay mismatches"), "{text}");
    }

    #[test]
    fn no_queries_shows_no_hit_rate() {
        let text = render_stats("/c", 0, 0, &totals_of(&[]), false);
        let line = text.lines().find(|l| l.starts_with("hit rate")).unwrap();
        assert!(line.trim_end().ends_with('-'), "{line}");
    }
}
