//! Tier 5: Verus on the sample crates in `tests/verus/`, directly and
//! through the proxy, cold and warm. Results and diagnostics must be
//! identical, and the counters must show the cache doing what DESIGN says.
//!
//! Skipped, with a message, unless `Z3CACHE_VERUS` points at a `verus`
//! binary. The Z3 next to it (as in a Verus release) is the real solver.
//! Every Verus run is bounded: a run that does not finish in time is killed
//! with its solvers and fails the test.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use z3cache::store::FILE_NAME;

/// Longest a single Verus run may take. The sample crates verify in well
/// under a second each.
const LIMIT: Duration = Duration::from_secs(90);

struct Verus {
    bin: PathBuf,
    z3: PathBuf,
}

fn verus() -> Option<Verus> {
    let bin = std::env::var_os("Z3CACHE_VERUS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)?;
    let z3 = bin.with_file_name("z3");
    Some(Verus { bin, z3 })
}

macro_rules! require_verus {
    () => {
        match verus() {
            Some(v) => v,
            None => {
                eprintln!(
                    "SKIPPED: set Z3CACHE_VERUS to a verus binary to run the end-to-end tests"
                );
                return;
            }
        }
    };
}

/// What a Verus run printed, and its exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Outcome {
    /// The "verification results" line.
    fn results(&self) -> &str {
        self.stdout
            .lines()
            .chain(self.stderr.lines())
            .find(|l| l.contains("verification results"))
            .unwrap_or_else(|| panic!("no results line in:\n{self}"))
    }

    /// The output with timing lines removed, for runs that print them.
    fn without_timings(&self) -> Outcome {
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.contains(" ms") && !l.contains("rlimit ("))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Outcome {
            code: self.code,
            stdout: strip(&self.stdout),
            stderr: strip(&self.stderr),
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {}\n--- stdout\n{}--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

/// Asserts that two runs printed the same and exited the same.
#[track_caller]
fn assert_same(what: &str, got: &Outcome, want: &Outcome) {
    assert!(
        got == want,
        "{what} differs\n=== got\n{got}\n=== want\n{want}"
    );
}

impl Verus {
    /// Runs Verus on `dir/file` with Z3 directly.
    fn direct(&self, dir: &Path, file: &str, args: &[&str]) -> Outcome {
        self.run(dir, file, args, &[("VERUS_Z3_PATH", self.z3.as_os_str())])
    }

    /// Runs Verus on `dir/file` through z3cache with the cache in `cache`
    /// and `env` added.
    fn cached(
        &self,
        dir: &Path,
        file: &str,
        args: &[&str],
        cache: &Path,
        env: &[(&str, &str)],
    ) -> Outcome {
        let z3cache = z3cache();
        let mut vars = vec![
            ("VERUS_Z3_PATH", z3cache.as_os_str()),
            ("Z3CACHE_REAL_Z3", self.z3.as_os_str()),
            ("Z3CACHE_DIR", cache.as_os_str()),
        ];
        vars.extend(env.iter().map(|(k, v)| (*k, OsStr::new(v))));
        self.run(dir, file, args, &vars)
    }

    /// Runs Verus in its own process group, with every `Z3CACHE_*` variable
    /// cleared, no config file, and `env` set. Waits for stderr to close as well as for
    /// Verus to exit: the solvers inherit stderr, so this also waits for
    /// every z3cache process to finish its bookkeeping.
    fn run(&self, dir: &Path, file: &str, args: &[&str], env: &[(&str, &OsStr)]) -> Outcome {
        let mut cmd = Command::new(&self.bin);
        cmd.arg(file)
            .arg("--no-report-long-running")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("Z3CACHE_") {
                cmd.env_remove(k);
            }
        }
        cmd.env_remove("VERUS_Z3_PATH");
        cmd.env("Z3CACHE_CONFIG", no_config());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let deadline = Instant::now() + LIMIT;
        let mut child = cmd.spawn().unwrap();
        let group = child.id() as i32;
        let (tx, rx) = mpsc::channel();
        for (name, mut pipe) in [
            (
                "stdout",
                Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            ),
            ("stderr", Box::new(child.stderr.take().unwrap())),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                let _ = tx.send(Event::Closed(
                    name,
                    String::from_utf8_lossy(&buf).into_owned(),
                ));
            });
        }
        std::thread::spawn(move || {
            let status = child.wait().unwrap();
            let _ = tx.send(Event::Exited(status.code().unwrap_or(-1)));
        });
        let (mut code, mut stdout, mut stderr) = (None, None, None);
        while code.is_none() || stdout.is_none() || stderr.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Event::Exited(c)) => code = Some(c),
                Ok(Event::Closed("stdout", s)) => stdout = Some(s),
                Ok(Event::Closed(_, s)) => stderr = Some(s),
                Err(_) => {
                    // SAFETY: kill(2) on our own process group.
                    unsafe { libc::kill(-group, libc::SIGKILL) };
                    panic!(
                        "verus {file} {args:?} in {} did not finish within {LIMIT:?} (exited: {code:?})",
                        dir.display()
                    );
                }
            }
        }
        Outcome {
            code: code.unwrap(),
            stdout: stdout.unwrap(),
            stderr: stderr.unwrap(),
        }
    }
}

enum Event {
    Exited(i32),
    Closed(&'static str, String),
}

/// A fresh directory (a "worktree") holding a copy of the sample crate
/// `tests/verus/<name>.rs`, under the same file name.
fn sample(name: &str, tag: &str) -> PathBuf {
    let dir = temp_dir(&format!("e2e-{tag}"));
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/verus")
        .join(format!("{name}.rs"));
    std::fs::copy(&src, dir.join(format!("{name}.rs"))).unwrap();
    dir
}

/// `z3cache stats --json` for the cache in `dir`.
struct Stats(String);

fn stats(cache: &Path) -> Stats {
    let mut cmd = Command::new(z3cache());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("Z3CACHE_") {
            cmd.env_remove(k);
        }
    }
    let out = cmd
        .args(["stats", "--json"])
        .env("Z3CACHE_DIR", cache)
        .env("Z3CACHE_CONFIG", no_config())
        .output()
        .unwrap();
    assert!(out.status.success(), "z3cache stats failed");
    Stats(String::from_utf8(out.stdout).unwrap())
}

impl Stats {
    /// The all-time part, or nothing if there is no store yet.
    fn all_time(&self) -> &str {
        self.0.find("\"all_time\"").map_or("", |at| &self.0[at..])
    }

    /// An all-time counter.
    fn get(&self, name: &str) -> i64 {
        number_after(self.all_time(), &format!("\"{name}\": ")).unwrap_or(0)
    }

    fn entries(&self) -> i64 {
        number_after(&self.0, "\"entries\": ").unwrap()
    }

    /// The all-time bypass reasons, as JSON object text.
    fn bypass_reasons(&self) -> &str {
        let all = self.all_time();
        let Some(at) = all.find("\"bypass_reasons\": {") else {
            return "";
        };
        let rest = &all[at + "\"bypass_reasons\": ".len()..];
        &rest[..=rest.find('}').unwrap()]
    }
}

fn number_after(text: &str, label: &str) -> Option<i64> {
    let at = text.find(label)? + label.len();
    let digits: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

/// How much counter `name` grew between two snapshots.
fn grew(before: &Stats, after: &Stats, name: &str) -> i64 {
    after.get(name) - before.get(name)
}

/// One recorded session (`Z3CACHE_RECORD`).
struct Recorded {
    input: String,
    meta: BTreeMap<String, String>,
    /// `(key, served)` of each query or read.
    rows: Vec<(String, String)>,
}

fn recorded(dir: &Path) -> Vec<Recorded> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("meta") {
            continue;
        }
        let meta = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter_map(|l| {
                l.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        let rows = std::fs::read_to_string(path.with_extension("tsv"))
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                (f[3].to_string(), f[8].to_string())
            })
            .collect();
        let input = std::fs::read_to_string(path.with_extension("in")).unwrap();
        out.push(Recorded { input, meta, rows });
    }
    out
}

impl Recorded {
    /// The module of `krate` whose items this session declares, or `root`.
    /// Verus names items `<crate>!<module>.<item>`.
    fn module(&self, krate: &str, modules: &[&str]) -> String {
        let found: Vec<&str> = modules
            .iter()
            .copied()
            .filter(|m| self.input.contains(&format!("{krate}!{m}.")))
            .collect();
        assert!(
            found.len() <= 1,
            "a session declares items of several modules: {found:?}"
        );
        found.first().copied().unwrap_or("root").to_string()
    }

    fn solver_starts(&self) -> i64 {
        self.meta["solver_starts"].parse().unwrap()
    }
}

const PASSING_MODULES: &[&str] = &[
    "arith",
    "loops",
    "recursion",
    "quant",
    "shapes",
    "collections",
];

/// (a) A crate of varied passing proofs: identical output directly, cold
/// and warm; the warm run starts no solver and is answered from the cache.
#[test]
fn passing_crate_is_identical_and_warm_starts_no_solver() {
    let v = require_verus!();
    let dir = sample("passing", "passing");
    let cache = dir.join("cache");

    let direct = v.direct(&dir, "passing.rs", &[]);
    assert_eq!(direct.code, 0, "{direct}");
    assert!(direct.results().contains(" 0 errors"), "{direct}");

    let cold = v.cached(&dir, "passing.rs", &[], &cache, &[]);
    assert_same("cold run", &cold, &direct);
    let after_cold = stats(&cache);
    assert!(after_cold.get("solver_starts") > 0);
    assert!(after_cold.get("stores") > 0);

    let warm = v.cached(&dir, "passing.rs", &[], &cache, &[]);
    assert_same("warm run", &warm, &direct);
    let after_warm = stats(&cache);
    assert_eq!(
        grew(&after_cold, &after_warm, "solver_starts"),
        0,
        "the warm run started a solver"
    );
    assert_eq!(grew(&after_cold, &after_warm, "misses"), 0);
    assert!(grew(&after_cold, &after_warm, "hits") > 0);
    assert_eq!(
        after_warm.get("bypassed_sessions"),
        0,
        "{}",
        after_warm.bypass_reasons()
    );
    assert_eq!(after_warm.get("replay_mismatches"), 0);
}

/// With nothing but `VERUS_Z3_PATH`, as `z3cache setup` leaves it: no
/// `real_z3`, and no `z3` or `verus` on `PATH`. z3cache finds the Z3 next to
/// Verus's driver, the process that starts it (DESIGN section 4).
#[test]
fn the_z3_next_to_verus_is_found_without_configuration() {
    let v = require_verus!();
    let dir = sample("passing", "no-config");
    let cache = dir.join("cache");
    let log = dir.join("z3cache.log");
    let path: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|d| !d.join("z3").exists() && !d.join("verus").exists())
        .collect();
    let path = std::env::join_paths(path).unwrap();
    let direct = v.direct(&dir, "passing.rs", &[]);
    let z3cache = z3cache();
    let env = [
        ("VERUS_Z3_PATH", z3cache.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
        ("Z3CACHE_LOG", log.as_os_str()),
        ("PATH", path.as_os_str()),
    ];
    let cached = v.run(&dir, "passing.rs", &[], &env);
    assert_same("run with no configuration", &cached, &direct);
    let s = stats(&cache);
    assert!(s.get("solver_starts") > 0, "{}", s.0);
    let logged = std::fs::read_to_string(&log).unwrap();
    let z3 = std::fs::canonicalize(&v.z3).unwrap();
    let want = format!("real solver {} (next to ", z3.display());
    assert!(logged.contains(&want), "{want:?} not in\n{logged}");
    assert!(
        logged.contains("the program that started z3cache)"),
        "{logged}"
    );
}

/// (b) Intentional failures next to passing functions: identical errors,
/// spans and exit status. Failures are re-solved under the default policy;
/// under `Z3CACHE_STORE=all` they are cached too.
#[test]
fn failing_crate_reports_the_same_errors() {
    let v = require_verus!();
    let dir = sample("failing", "failing");
    let cache = dir.join("cache");

    let direct = v.direct(&dir, "failing.rs", &[]);
    assert_ne!(direct.code, 0, "{direct}");
    assert!(direct.results().contains(" 2 errors"), "{direct}");
    assert!(direct.stderr.contains("assertion failed"), "{direct}");
    assert!(
        direct.stderr.contains("postcondition not satisfied"),
        "{direct}"
    );

    let cold = v.cached(&dir, "failing.rs", &[], &cache, &[]);
    assert_same("cold run", &cold, &direct);
    let after_cold = stats(&cache);

    let warm = v.cached(&dir, "failing.rs", &[], &cache, &[]);
    assert_same("warm run", &warm, &direct);
    let after_warm = stats(&cache);
    assert!(
        grew(&after_cold, &after_warm, "solver_starts") > 0,
        "the failures were not re-solved"
    );
    assert!(
        grew(&after_cold, &after_warm, "hits") > 0,
        "the passing functions were not cached"
    );

    // Store failures too: the first run stores them, the second needs no
    // solver.
    let all = [("Z3CACHE_STORE", "all")];
    let warm_all = v.cached(&dir, "failing.rs", &[], &cache, &all);
    assert_same("first run storing all", &warm_all, &direct);
    let before = stats(&cache);
    let warm_all = v.cached(&dir, "failing.rs", &[], &cache, &all);
    assert_same("second run storing all", &warm_all, &direct);
    let after = stats(&cache);
    assert_eq!(
        grew(&before, &after, "solver_starts"),
        0,
        "the second run storing all started a solver"
    );
    assert_eq!(grew(&before, &after, "misses"), 0);
    assert_eq!(after.get("replay_mismatches"), 0);
}

/// (c) A function over its `#[verifier::rlimit]`: reported identically.
/// The `unknown` is not stored, so the warm run solves it again.
#[test]
fn exceeded_rlimit_is_reported_identically() {
    let v = require_verus!();
    let dir = sample("rlimit", "rlimit");
    let cache = dir.join("cache");

    let direct = v.direct(&dir, "rlimit.rs", &[]);
    assert_ne!(direct.code, 0, "{direct}");
    assert!(
        direct.stderr.contains("resource limit (rlimit) exceeded"),
        "{direct}"
    );
    assert!(direct.results().contains(" 1 errors"), "{direct}");

    let cold = v.cached(&dir, "rlimit.rs", &[], &cache, &[]);
    assert_same("cold run", &cold, &direct);
    let before = stats(&cache);
    let warm = v.cached(&dir, "rlimit.rs", &[], &cache, &[]);
    assert_same("warm run", &warm, &direct);
    let after = stats(&cache);
    assert!(
        grew(&before, &after, "solver_starts") > 0,
        "the exceeded query was not re-solved"
    );
    assert!(grew(&before, &after, "hits") > 0);
}

/// (d) Editing one function: the result is that of the edited source, the
/// edited module's new queries miss, and every other module is answered
/// entirely from the cache. A breaking edit is reported, not hidden.
#[test]
fn an_edit_misses_only_in_the_edited_module() {
    let v = require_verus!();
    let original = sample("passing", "edit-original");
    let cache = original.join("cache");
    let cold_rec = original.join("record-cold");
    let cold = v.cached(
        &original,
        "passing.rs",
        &[],
        &cache,
        &[("Z3CACHE_RECORD", cold_rec.to_str().unwrap())],
    );
    assert_eq!(cold.code, 0, "{cold}");
    let cold_keys: BTreeSet<String> = recorded(&cold_rec)
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| r.0.clone()))
        .collect();

    let src = std::fs::read_to_string(original.join("passing.rs")).unwrap();
    let edit = |from: &str, to: &str, tag: &str| {
        assert!(src.contains(from), "edit target not found: {from}");
        let dir = temp_dir(&format!("e2e-{tag}"));
        std::fs::write(dir.join("passing.rs"), src.replacen(from, to, 1)).unwrap();
        dir
    };
    // `count_down` in module `loops` gains a postcondition that holds.
    let good = edit(
        "        ensures\n            r == 0,\n",
        "        ensures\n            r == 0,\n            r <= n,\n",
        "edit-good",
    );
    let direct = v.direct(&good, "passing.rs", &[]);
    assert!(direct.results().contains(" 0 errors"), "{direct}");
    let rec = good.join("record");
    let warm = v.cached(
        &good,
        "passing.rs",
        &[],
        &cache,
        &[("Z3CACHE_RECORD", rec.to_str().unwrap())],
    );
    assert_same("edited crate, warm", &warm, &direct);

    let sessions = recorded(&rec);
    let mut edited_misses = 0;
    for s in &sessions {
        let module = s.module("passing", PASSING_MODULES);
        let misses: Vec<&(String, String)> = s.rows.iter().filter(|r| r.1 != "cache").collect();
        if module == "loops" {
            edited_misses += misses.len();
            for (key, _) in misses {
                assert!(
                    !cold_keys.contains(key),
                    "loops: a query seen in the cold run missed"
                );
            }
        } else {
            assert!(
                misses.is_empty(),
                "module {module} is unchanged but missed {} times",
                misses.len()
            );
            assert_eq!(
                s.solver_starts(),
                0,
                "module {module} is unchanged but started a solver"
            );
        }
    }
    assert!(
        edited_misses > 0,
        "the edited function's queries did not miss"
    );

    // `count_down` now claims a false postcondition.
    let bad = edit(
        "        ensures\n            r == 0,\n",
        "        ensures\n            r == 1,\n",
        "edit-bad",
    );
    let direct = v.direct(&bad, "passing.rs", &[]);
    assert_ne!(direct.code, 0, "{direct}");
    assert!(
        direct.stderr.contains("postcondition not satisfied"),
        "{direct}"
    );
    let warm = v.cached(&bad, "passing.rs", &[], &cache, &[]);
    assert_same("broken edit, warm", &warm, &direct);
}

/// Holds solvers back until released, so that two Verus runs are certain to
/// miss the same keys at the same time. The real solver is replaced by a
/// shim that reads one line from a FIFO before it starts Z3 in proxy mode.
/// The test keeps the FIFO open for writing, so a shim blocks until a line
/// is written.
struct Gate {
    shim: PathBuf,
    fifo: std::fs::File,
}

impl Gate {
    fn new(dir: &Path, z3: &Path) -> Gate {
        let path = dir.join("gate");
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "mkfifo failed"
        );
        // Read-write, so that opening does not wait for a reader and the
        // shims' reads block instead of seeing end of file.
        let fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        let shim = dir.join("z3-gated");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\ncase \" $* \" in *\" -in \"*) IFS= read -r _ < {} ;; esac\nexec {} \"$@\"\n",
                quote(&path),
                quote(z3)
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        Gate { shim, fifo }
    }

    /// Lets `n` more solvers start.
    fn admit(&mut self, n: usize) {
        use std::io::Write;
        self.fifo.write_all(&b"go\n".repeat(n)).unwrap();
    }

    /// Lets every solver start from now on: more lines than any test
    /// starts solvers, and well within a pipe's buffer.
    fn open(&mut self) {
        self.admit(1000);
    }
}

/// (e) Two worktrees verified at the same time on one empty cache: the same
/// results, and together they solve no more than one cold run plus the
/// failures they each re-solve.
///
/// The runs are made to collide: solvers are held at a [`Gate`] until one
/// session reports waiting for another's in-flight lock, so the lock path
/// is exercised on every run rather than when timing allows.
fn concurrent_worktrees(name: &str) {
    let v = require_verus!();
    let file = format!("{name}.rs");
    // Reference: what one cold run solves, and what a warm one re-solves.
    let reference = sample(name, &format!("{name}-ref"));
    let ref_cache = reference.join("cache");
    let direct = v.direct(&reference, &file, &[]);
    let cold = v.cached(&reference, &file, &[], &ref_cache, &[]);
    assert_same("reference cold run", &cold, &direct);
    let after_cold = stats(&ref_cache);
    v.cached(&reference, &file, &[], &ref_cache, &[]);
    let after_warm = stats(&ref_cache);
    let cold_misses = after_cold.get("misses");
    let resolved = grew(&after_cold, &after_warm, "misses");

    let shared = temp_dir(&format!("{name}-shared"));
    let mut gate = Gate::new(&shared, &v.z3);
    let shim = gate.shim.to_str().unwrap().to_string();
    // Learn the solver's version reply first: a session asks for it before
    // its first query, and would start a solver for it outside any lock.
    gate.admit(1);
    let version = run_within(
        &z3cache(),
        &["-smt2", "-in"],
        b"(get-info :version)",
        &[
            ("Z3CACHE_REAL_Z3", OsStr::new(&shim)),
            ("Z3CACHE_DIR", shared.as_os_str()),
        ],
        LIMIT,
    )
    .expect("asking the version did not finish");
    assert!(
        version.text().starts_with("(:version"),
        "{}",
        version.text()
    );
    let before = stats(&shared);

    let trees = [
        sample(name, &format!("{name}-wt1")),
        sample(name, &format!("{name}-wt2")),
    ];
    let logs: Vec<PathBuf> = trees.iter().map(|t| t.join("z3cache.log")).collect();
    let outcomes: Vec<Outcome> = std::thread::scope(|s| {
        let runs: Vec<_> = trees
            .iter()
            .zip(&logs)
            .map(|(t, log)| {
                let env = [
                    ("Z3CACHE_REAL_Z3", shim.as_str()),
                    ("Z3CACHE_LOG", log.to_str().unwrap()),
                ];
                let (v, file, shared) = (&v, &file, &shared);
                s.spawn(move || v.cached(t, file, &[], shared, &env))
            })
            .collect();
        // Every first miss holds its lock while its solver waits at the
        // gate, so the other run soon finds a lock busy.
        let deadline = Instant::now() + Duration::from_secs(60);
        let collided = loop {
            if logs.iter().any(|l| file_contains(l, LOCK_BUSY)) {
                break true;
            }
            if Instant::now() > deadline || runs.iter().any(|r| r.is_finished()) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        gate.open();
        assert!(collided, "no session waited for another's lock");
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    for (i, o) in outcomes.iter().enumerate() {
        assert_same(&format!("worktree {}", i + 1), o, &direct);
    }
    let after = stats(&shared);
    let misses = grew(&before, &after, "misses");
    let (waits, lock_hits) = (
        grew(&before, &after, "lock_waits"),
        grew(&before, &after, "lock_hits"),
    );
    eprintln!(
        "{name}: one cold run solved {cold_misses}, a warm run re-solved {resolved}; two concurrent runs \
         solved {misses} (lock waits {waits}, hits after taking a lock {lock_hits})"
    );
    assert!(waits > 0);
    assert!(
        misses >= cold_misses,
        "two runs solved less than one cold run"
    );
    assert!(
        misses <= cold_misses + resolved,
        "two concurrent runs solved {misses} queries; one cold run solves {cold_misses} and {resolved} are re-solved"
    );
    if resolved == 0 {
        assert!(
            lock_hits > 0,
            "a session that waited did not find the answer stored"
        );
    }
    assert_eq!(grew(&before, &after, "lock_timeouts"), 0);
    assert_eq!(after.get("replay_mismatches"), 0);
}

#[test]
fn concurrent_worktrees_share_one_cache() {
    concurrent_worktrees("modules");
}

#[test]
fn concurrent_worktrees_with_failures_share_one_cache() {
    concurrent_worktrees("failing");
}

/// The `rlimit` totals in `verus --time` output: (smt-init, smt-run).
fn rlimit_totals(o: &Outcome) -> (u64, u64) {
    let total = |label: &str| -> u64 {
        let line = o
            .stdout
            .lines()
            .find(|l| l.trim_start().starts_with(label))
            .unwrap_or_else(|| panic!("no {label} line in:\n{o}"));
        let before = line.split(" rlimit").next().unwrap();
        before.rsplit(' ').next().unwrap().parse().unwrap()
    };
    (total("total smt-init:"), total("total smt-run:"))
}

/// (f) `--time` reports the same resource totals cold and warm: the warm
/// run's statistics are synthesized from the stored costs.
#[test]
fn time_reports_the_same_rlimit_totals_cold_and_warm() {
    let v = require_verus!();
    let dir = sample("passing", "time");
    let cache = dir.join("cache");
    let direct = v.direct(&dir, "passing.rs", &["--time"]);
    assert_eq!(direct.code, 0, "{direct}");
    let cold = v.cached(&dir, "passing.rs", &["--time"], &cache, &[]);
    let warm = v.cached(&dir, "passing.rs", &["--time"], &cache, &[]);
    for (what, o) in [("cold", &cold), ("warm", &warm)] {
        assert_eq!(o.code, 0, "{what}: {o}");
        assert_eq!(o.results(), direct.results(), "{what}");
        assert_same(what, &o.without_timings(), &direct.without_timings());
    }
    let (cold_t, warm_t) = (rlimit_totals(&cold), rlimit_totals(&warm));
    eprintln!(
        "rlimit totals (init, run): direct {:?}, cold {cold_t:?}, warm {warm_t:?}",
        rlimit_totals(&direct)
    );
    assert!(cold_t.1 > 0);
    assert_eq!(cold_t, warm_t, "rlimit totals differ cold and warm");
}

/// The output lines in sorted order, with the exit status. Under profiling,
/// Z3 writes its version banner to the shared stderr while Verus prints its
/// notes, and the two interleave differently from run to run, directly as
/// well as through the proxy.
fn sorted_lines(o: &Outcome) -> (i32, String) {
    let mut lines: Vec<&str> = o.stdout.lines().chain(o.stderr.lines()).collect();
    lines.sort_unstable();
    (o.code, lines.join("\n"))
}

/// (g) Profiling makes Verus set Z3's `trace` options: those sessions
/// bypass, with the reason in `stats`, and nothing hangs or changes.
#[test]
fn profiling_sessions_bypass_and_match() {
    let v = require_verus!();
    // `--profile` reruns the query that exceeded its limit with tracing.
    let dir = sample("rlimit", "profile");
    let cache = dir.join("cache");
    let args = ["--profile"];
    let direct = v.direct(&dir, "rlimit.rs", &args);
    assert!(
        direct.stderr.contains("note: profile statistics"),
        "{direct}"
    );
    for pass in ["cold", "warm"] {
        let o = v.cached(&dir, "rlimit.rs", &args, &cache, &[]);
        assert_eq!(
            sorted_lines(&o),
            sorted_lines(&direct),
            "--profile, {pass}\n=== proxied\n{o}\n=== direct\n{direct}"
        );
    }
    let s = stats(&cache);
    assert_eq!(s.get("bypassed_sessions"), 2, "{}", s.0);
    assert_eq!(s.bypass_reasons(), "{\"option :trace\": 2}");

    // `-V capture-profiles` traces every session.
    let dir = sample("passing", "capture-profiles");
    let cache = dir.join("cache");
    let args = ["-V", "capture-profiles"];
    let direct = v.direct(&dir, "passing.rs", &args);
    assert_eq!(direct.code, 0, "{direct}");
    let proxied = v.cached(&dir, "passing.rs", &args, &cache, &[]);
    assert_eq!(
        sorted_lines(&proxied),
        sorted_lines(&direct),
        "\n=== proxied\n{proxied}\n=== direct\n{direct}"
    );
    let s = stats(&cache);
    assert!(s.get("sessions") > 1);
    assert_eq!(s.get("bypassed_sessions"), s.get("sessions"), "{}", s.0);
    assert_eq!(s.entries(), 0, "a traced session stored an entry");
}

/// A read-only snapshot of every row in the store.
fn store_rows(cache: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(
        cache.join(FILE_NAME),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut rows = Vec::new();
    for table in ["entry", "counters", "solver"] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = stmt.column_count();
        let mut q = stmt.query([]).unwrap();
        while let Some(row) = q.next().unwrap() {
            let cells: Vec<String> = (0..n)
                .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                .collect();
            rows.push(format!("{table}: {}", cells.join(" | ")));
        }
    }
    rows.sort();
    rows
}

/// (h) Modes: `off` is plain Z3; `enforce` fails promptly on an empty cache
/// and succeeds on a warm one; `ro` succeeds and changes nothing.
#[test]
fn modes_behave_with_verus() {
    let v = require_verus!();
    let dir = sample("passing", "modes");
    let cache = dir.join("cache");
    let direct = v.direct(&dir, "passing.rs", &[]);
    assert_eq!(direct.code, 0, "{direct}");

    let off = v.cached(&dir, "passing.rs", &[], &cache, &[("Z3CACHE_MODE", "off")]);
    assert_same("mode off", &off, &direct);
    assert!(!cache.join(FILE_NAME).exists(), "mode off created a store");

    let enforce = v.cached(
        &dir,
        "passing.rs",
        &[],
        &cache,
        &[("Z3CACHE_MODE", "enforce")],
    );
    assert_ne!(
        enforce.code, 0,
        "enforce on an empty cache succeeded:\n{enforce}"
    );
    assert!(
        !cache.join(FILE_NAME).exists(),
        "mode enforce created a store"
    );

    let ro_cold = v.cached(&dir, "passing.rs", &[], &cache, &[("Z3CACHE_MODE", "ro")]);
    assert_same("mode ro, empty cache", &ro_cold, &direct);
    assert!(!cache.join(FILE_NAME).exists(), "mode ro created a store");

    let cold = v.cached(&dir, "passing.rs", &[], &cache, &[]);
    assert_same("mode rw, cold", &cold, &direct);
    let before = store_rows(&cache);
    assert!(before.iter().any(|r| r.starts_with("entry: ")));

    let enforce = v.cached(
        &dir,
        "passing.rs",
        &[],
        &cache,
        &[("Z3CACHE_MODE", "enforce")],
    );
    assert_same("mode enforce, warm", &enforce, &direct);
    let ro = v.cached(&dir, "passing.rs", &[], &cache, &[("Z3CACHE_MODE", "ro")]);
    assert_same("mode ro, warm", &ro, &direct);
    assert_eq!(
        store_rows(&cache),
        before,
        "read-only modes changed the store"
    );
}

const MODULES: &[&str] = &["bits", "ranges", "search", "counting", "sets", "top"];

/// (i) A multi-module crate verified with several threads, so several
/// sessions run at once: identical cold and warm. Also with every function
/// in its own solver session.
#[test]
fn multi_module_crate_with_threads() {
    let v = require_verus!();
    for (tag, args) in [
        ("threads", &["--num-threads", "4"][..]),
        ("spinoff", &["--num-threads", "4", "-V", "spinoff-all"][..]),
    ] {
        let dir = sample("modules", tag);
        let cache = dir.join("cache");
        let direct = v.direct(&dir, "modules.rs", args);
        assert_eq!(direct.code, 0, "{tag}: {direct}");
        let cold = v.cached(&dir, "modules.rs", args, &cache, &[]);
        assert_same(&format!("{tag}, cold"), &cold, &direct);
        let after_cold = stats(&cache);
        assert!(
            after_cold.get("sessions") as usize > MODULES.len(),
            "{tag}: {}",
            after_cold.0
        );
        let warm = v.cached(&dir, "modules.rs", args, &cache, &[]);
        assert_same(&format!("{tag}, warm"), &warm, &direct);
        let after_warm = stats(&cache);
        assert_eq!(
            grew(&after_cold, &after_warm, "solver_starts"),
            0,
            "{tag}: the warm run started a solver"
        );
        assert!(grew(&after_cold, &after_warm, "hits") > 0);
    }
}

/// (j) Two recorded runs, then `z3cache analyze`: the projection is
/// labelled as such and every query of the second run would hit.
#[test]
fn analyze_projects_the_second_run_as_hits() {
    let v = require_verus!();
    let dir = sample("passing", "analyze");
    let cache = dir.join("cache");
    let (r1, r2) = (dir.join("record-1"), dir.join("record-2"));
    for rec in [&r1, &r2] {
        let o = v.cached(
            &dir,
            "passing.rs",
            &[],
            &cache,
            &[("Z3CACHE_RECORD", rec.to_str().unwrap())],
        );
        assert_eq!(o.code, 0, "{o}");
    }
    // The second run was answered from the cache.
    let second = recorded(&r2);
    assert!(second.iter().flat_map(|s| &s.rows).all(|r| r.1 == "cache"));

    let out = Command::new(z3cache())
        .arg("analyze")
        .arg(&r1)
        .arg(&r2)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "analyze failed:\n{text}");
    let (first, second) = text
        .split_once("\nrecording 2:")
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        first.contains("projection        none (first recording"),
        "{text}"
    );
    assert!(second.contains("projection (nothing was cached)"), "{text}");
    let queries = second
        .lines()
        .find_map(|l| l.strip_prefix("  queries"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("no queries line:\n{text}"));
    let would_hit = second
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("would hit"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("no would-hit line:\n{text}"));
    assert_ne!(queries, "0", "{text}");
    assert_eq!(
        would_hit, queries,
        "not every query of the second run would hit:\n{text}"
    );
}
