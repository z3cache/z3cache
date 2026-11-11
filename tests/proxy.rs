//! Tier 2: the proxy against the scripted stub solver. No real Z3 needed.
//!
//! Every run gets a fresh cache unless a test passes `Z3CACHE_DIR` to share
//! one between runs.

mod common;

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use z3cache::store::FILE_NAME;

const PROXY_ARGS: &[&str] = &["-smt2", "-in"];

/// A cache directory and stub log shared by several runs.
struct Fixture {
    cache: PathBuf,
    log: PathBuf,
    stub: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = temp_dir(name);
        Fixture {
            cache: dir.join("cache"),
            log: dir.join("stub.log"),
            stub: stub(),
        }
    }

    /// The environment for the proxy on the shared cache, plus `extra`.
    fn env<'a>(&'a self, extra: &[(&'a str, &'a OsStr)]) -> Vec<(&'a str, &'a OsStr)> {
        let mut env = vec![
            ("Z3CACHE_REAL_Z3", self.stub.as_os_str()),
            ("Z3CACHE_DIR", self.cache.as_os_str()),
            ("Z3STUB_LOG", self.log.as_os_str()),
        ];
        env.extend_from_slice(extra);
        env
    }

    /// Runs the proxy on the shared cache with extra environment.
    fn run_env(&self, input: &[u8], extra: &[(&str, &OsStr)]) -> Run {
        run(&z3cache(), PROXY_ARGS, input, &self.env(extra))
    }

    fn run(&self, input: &[u8]) -> Run {
        self.run_env(input, &[])
    }

    fn launches(&self) -> usize {
        launches(&self.log)
    }

    /// The canonical commands the stub received, without the proxy's
    /// sentinels and statistics reads.
    fn received(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.contains("z3cache-sentinel") && *l != "(get-info :all-statistics)")
            .map(str::to_string)
            .collect()
    }

    fn cli(&self, args: &[&str]) -> Run {
        run(
            &z3cache(),
            args,
            b"",
            &[
                ("Z3CACHE_DIR", self.cache.as_os_str()),
                ("Z3CACHE_REAL_Z3", self.stub.as_os_str()),
            ],
        )
    }

    /// An all-time counter of the shared cache.
    fn counter(&self, name: &str) -> i64 {
        counter(&self.cache, name)
    }

    /// The proxy on the shared cache, not yet started, with extra
    /// environment.
    fn command(&self, extra: &[(&str, &OsStr)]) -> Command {
        proxy_command(&z3cache(), PROXY_ARGS, &self.env(extra))
    }
}

/// Runs the same input through the stub directly and through the proxy.
fn both(input: &[u8], extra_env: &[(&str, &OsStr)]) -> (Run, Run) {
    let stub = stub();
    let direct = run(&stub, PROXY_ARGS, input, extra_env);
    let mut env = vec![("Z3CACHE_REAL_Z3", stub.as_os_str())];
    env.extend_from_slice(extra_env);
    let proxied = run(&z3cache(), PROXY_ARGS, input, &env);
    (direct, proxied)
}

fn assert_same(input: &[u8], extra_env: &[(&str, &OsStr)]) -> Run {
    let (direct, proxied) = both(input, extra_env);
    assert_eq!(proxied.text(), direct.text());
    assert_eq!(proxied.stdout, direct.stdout);
    assert_eq!(proxied.code, direct.code);
    assert!(!proxied.text().contains("z3cache-sentinel"));
    proxied
}

#[test]
fn verus_like_session_is_byte_identical_cold_and_warm() {
    let input = corpus("verus_like.smt2");
    let answers = ("Z3STUB_ANSWERS", OsStr::new("unsat,sat,unsat"));
    let direct = run(&stub(), PROXY_ARGS, &input, &[answers]);
    let f = Fixture::new("verus-like");
    let cold = f.run_env(&input, &[answers]);
    assert_eq!(cold.text(), direct.text());
    assert_eq!(cold.code, 0);
    assert_eq!(f.launches(), 1);
    // The second function's first query is sat, so it is not stored and is
    // solved again. The first function's query and the second function's
    // later unsat query are answered from the cache.
    let warm = f.run_env(&input, &[("Z3STUB_ANSWERS", OsStr::new("sat"))]);
    assert_eq!(warm.text(), direct.text());
    assert_eq!(f.counter("hits"), 2);
}

#[test]
fn warm_run_launches_no_solver() {
    let input = b"(declare-fun f (Int) Int)\n(push)\n(assert (> (f 1) 0))\n(get-info :version)\n\
                  (check-sat)\n(get-value ((f 1)))\n(pop)\n(echo \"<<DONE>>\")\n\
                  (push)\n(assert (> (f 2) 0))\n(check-sat)\n(pop)\n(echo \"<<DONE>>\")\n";
    let f = Fixture::new("warm");
    let cold = f.run(input);
    assert_eq!(f.launches(), 1);
    let warm = f.run(input);
    assert_eq!(warm.text(), cold.text());
    assert_eq!(warm.code, 0);
    assert_eq!(f.launches(), 1, "the warm run started a solver");
    assert_eq!(f.counter("hits"), 3, "two queries and one read");
    assert_eq!(f.counter("solver_starts"), 1);
}

#[test]
fn statistics_come_from_the_virtual_counter_and_never_decrease() {
    let input = b"(get-info :all-statistics)\n(push)(assert a)(check-sat)(pop)\n\
                  (get-info :all-statistics)\n(push)(assert b)(check-sat)(pop)\n\
                  (get-info :all-statistics)\n";
    let f = Fixture::new("rlimit");
    let cold = f.run(input);
    // The first read has no solver to ask; later ones are the stub's
    // statistics with the counter replaced.
    assert!(
        cold.text().starts_with("(:rlimit-count 0)\n"),
        "{}",
        cold.text()
    );
    assert!(cold.text().contains(":max-memory"));
    assert_eq!(rlimit_counts(&cold.text()), vec![0, 100, 200]);
    let warm = f.run(input);
    assert_eq!(
        warm.text(),
        "(:rlimit-count 0)\nunsat\n(:rlimit-count 100)\nunsat\n(:rlimit-count 200)\n"
    );
    // A miss after hits: the stub's own counter restarts at 0, the client's
    // does not.
    let mixed = f.run(
        b"(push)(assert a)(check-sat)(pop)(push)(assert c)(check-sat)(pop)(get-info :all-statistics)",
    );
    assert_eq!(rlimit_counts(&mixed.text()), vec![200]);
    // A framed bypass after hits keeps the counter from going back.
    let bypass = f.run(
        b"(push)(assert a)(check-sat)(pop)(get-info :all-statistics)(pop)(get-info :all-statistics)",
    );
    let counts = rlimit_counts(&bypass.text());
    assert_eq!(counts.len(), 2);
    assert!(counts[1] >= counts[0], "{counts:?}");
}

#[test]
fn every_chunk_size_gives_the_same_output() {
    let stub = stub();
    let input = corpus("strings_comments.smt2");
    let direct = run(&stub, PROXY_ARGS, &input, &[]);
    for chunk in [1, 2, 3, 7, 64] {
        let out = run_chunked(
            &z3cache(),
            PROXY_ARGS,
            &input,
            chunk,
            &[("Z3CACHE_REAL_Z3", stub.as_os_str())],
        );
        assert_eq!(out.stdout, direct.stdout, "chunk size {chunk}");
    }
}

#[test]
fn replay_sends_the_canonical_path_without_cached_frames() {
    let f = Fixture::new("replay");
    f.run(b"(declare-fun f () Int)\n(push)\n(assert   (> f 1))\n(check-sat)\n(pop)\n");
    std::fs::remove_file(&f.log).unwrap();
    let out = f.run(
        b"(declare-fun f () Int)\n(push)\n(assert (> f 1)) ; cached\n(check-sat)\n(pop)\n\
          (push)\n(assert\n  (> f 2))\n(check-sat)\n(pop)\n",
    );
    assert_eq!(out.text(), "unsat\nunsat\n");
    assert_eq!(
        f.received(),
        vec![
            "launch -smt2 -in",
            "(declare-fun f () Int)",
            "(push)",
            "(assert (> f 2))",
            "(check-sat)",
            "(pop)",
        ]
    );
}

#[test]
fn cached_queries_on_the_path_are_rerun_and_mismatches_counted() {
    let f = Fixture::new("mismatch");
    f.run(b"(assert a)\n(check-sat)\n");
    // The cached unsat is served, then the miss replays it and the stub now
    // says sat.
    let out = f.run_env(
        b"(assert a)\n(check-sat)\n(assert c)\n(check-sat)\n",
        &[("Z3STUB_ANSWERS", OsStr::new("sat,unsat"))],
    );
    assert_eq!(out.text(), "unsat\nunsat\n");
    assert_eq!(f.counter("replay_mismatches"), 1);
    assert_eq!(f.counter("hits"), 1);
    // After a mismatch the session stores nothing.
    assert_eq!(f.counter("stores"), 1);
}

#[test]
fn only_unsat_is_stored() {
    for answer in ["sat", "unknown"] {
        let f = Fixture::new(answer);
        let env = [("Z3STUB_ANSWERS", OsStr::new(answer))];
        let input = b"(assert a)\n(check-sat)\n(get-value (x))\n";
        let first = f.run_env(input, &env);
        let second = f.run_env(input, &env);
        assert_eq!(first.text(), second.text());
        assert_eq!(f.launches(), 2, "{answer} must be solved again");
        assert_eq!(f.counter("stores"), 0);
    }
}

#[test]
fn reads_after_unsat_are_stored() {
    let f = Fixture::new("reads");
    let input = b"(assert a)\n(check-sat)\n(get-value (x))\n";
    let cold = f.run(input);
    let warm = f.run(input);
    assert_eq!(cold.text(), "unsat\n((x 0))\n");
    assert_eq!(warm.text(), cold.text());
    assert_eq!(f.launches(), 1);
    assert_eq!(f.counter("stores"), 2);
}

#[test]
fn errors_surface_at_the_next_solver_use_and_stop_storing() {
    // The stub rejects `stub-error` when it receives it. The proxy sends
    // silent commands only when it needs the solver, so the error appears
    // with the next answer the solver gives, after the local echoes.
    let f = Fixture::new("errors");
    let out = f.run(b"(echo \"a\")\n(assert stub-error)\n(echo \"b\")\n(check-sat)\n");
    assert_eq!(
        out.text(),
        "a\nb\n(error \"stub: asserted stub-error\")\nunsat\n"
    );
    assert_eq!(out.code, 1, "the solver's exit status");
    // Rule 6: nothing is stored from the session afterwards, even a query
    // whose path no longer contains the failing command.
    let f = Fixture::new("errors2");
    f.run(b"(push)\n(assert stub-error)\n(check-sat)\n(pop)\n(assert ok)\n(check-sat)\n");
    assert_eq!(f.counter("stores"), 0);
    assert_eq!(f.counter("errored_sessions"), 1);
    // And the rest of the session is a verbatim copy (section 7).
    assert_eq!(f.counter("bypassed_sessions"), 1);
    f.run(b"(assert ok)\n(check-sat)\n");
    assert_eq!(f.launches(), 2);
}

#[test]
fn read_only_mode_stores_nothing() {
    let f = Fixture::new("ro");
    let input = b"(assert a)\n(check-sat)\n";
    let out = f.run_env(input, &[("Z3CACHE_MODE", OsStr::new("ro"))]);
    assert_eq!(out.text(), "unsat\n");
    f.run(input);
    assert_eq!(f.launches(), 2);
    // And it reads what rw stored.
    f.run_env(input, &[("Z3CACHE_MODE", OsStr::new("ro"))]);
    assert_eq!(f.launches(), 2);
}

#[test]
fn enforce_mode_fails_on_a_miss_without_starting_a_solver() {
    let f = Fixture::new("enforce");
    let input = b"(echo \"x\")\n(assert a)\n(check-sat)\n";
    let enforce = [("Z3CACHE_MODE", OsStr::new("enforce"))];
    let out = f.run_env(input, &enforce);
    assert_eq!(
        out.text(),
        "x\n(error \"z3cache: cache miss in enforce mode\")\n"
    );
    assert_eq!(out.code, 1);
    assert_eq!(f.launches(), 0);
    f.run(input);
    let out = f.run_env(input, &enforce);
    assert_eq!(out.text(), "x\nunsat\n");
    assert_eq!(out.code, 0);
    assert_eq!(f.launches(), 1);
}

#[test]
fn version_is_answered_locally_once_seen() {
    let f = Fixture::new("version");
    let first = f.run(b"(get-info :version)\n");
    assert_eq!(first.text(), "(:version \"4.99.0\")\n");
    assert_eq!(f.launches(), 1);
    let second = f.run(b"(get-info :version)\n(echo \"x\")\n");
    assert_eq!(second.text(), "(:version \"4.99.0\")\nx\n");
    assert_eq!(f.launches(), 1);
}

#[test]
fn failing_to_record_the_version_keeps_the_store_in_use() {
    // The solver identity is a memo: a failed write of it, here of the
    // version reply, must not disable lookups and stores.
    let f = Fixture::new("version-unwritable");
    f.run(b"(check-sat)\n");
    {
        let conn = rusqlite::Connection::open(f.cache.join(FILE_NAME)).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER no_solver_writes BEFORE INSERT ON solver \
             BEGIN SELECT RAISE(ABORT, 'solver table is read-only'); END;",
        )
        .unwrap();
    }
    let out = f.run(b"(get-info :version)\n(assert a)\n(check-sat)\n");
    assert_eq!(out.text(), "(:version \"4.99.0\")\nunsat\n");
    assert_eq!(f.counter("stores"), 2);
    assert_eq!(f.counter("sessions"), 2);
}

#[test]
fn unusable_store_still_answers_correctly() {
    let dir = temp_dir("badstore");
    let file = dir.join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let input = corpus("verus_like.smt2");
    let answers = ("Z3STUB_ANSWERS", OsStr::new("unsat,sat,unsat"));
    let direct = run(&stub(), PROXY_ARGS, &input, &[answers]);
    let out = run(
        &z3cache(),
        PROXY_ARGS,
        &input,
        &[
            ("Z3CACHE_REAL_Z3", stub().as_os_str()),
            ("Z3CACHE_DIR", file.as_os_str()),
            answers,
        ],
    );
    assert_eq!(out.stdout, direct.stdout);
    assert_eq!(out.code, 0);
}

#[test]
fn interactive_batches_do_not_wait_for_end_of_input() {
    // Like Verus: write a batch, read up to <<DONE>>, then write the next.
    let stub = stub();
    let mut child = proxy_command(
        &z3cache(),
        PROXY_ARGS,
        &[("Z3CACHE_REAL_Z3", stub.as_os_str())],
    )
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    for i in 0..3 {
        writeln!(
            stdin,
            "(push)\n(assert (> x {i}))\n(check-sat)\n(pop)\n(echo \"<<DONE>>\")"
        )
        .unwrap();
        stdin.flush().unwrap();
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "unexpected end of output");
            if line == "<<DONE>>\n" {
                break;
            }
            lines.push(line);
        }
        assert_eq!(lines, vec!["unsat\n"]);
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn unknown_command_bypasses_and_still_matches() {
    let input = b"(echo \"a\")\n(reset)\n(check-sat)\n(frobnicate)\n(echo \"b\")\n";
    let dir = temp_dir("bypass");
    let out = assert_same(input, &[("Z3CACHE_RECORD", dir.as_os_str())]);
    assert_eq!(out.code, 1, "the stub reports frobnicate as an error");
    let meta = read_single(&dir, "meta");
    assert!(meta.contains("bypass=command reset\n"), "{meta}");
    // Raw bypass output is not scanned for errors (it cannot be told apart
    // from echo text); the solver's exit status carries them.
    assert!(meta.contains("errored=0\n"), "{meta}");
}

#[test]
fn bypass_after_cached_answers_replays_the_path() {
    let f = Fixture::new("bypass-replay");
    f.run(b"(declare-const x Int)\n(assert (> x 0))\n(check-sat)\n");
    std::fs::remove_file(&f.log).unwrap();
    let out =
        f.run(b"(declare-const x Int)\n(assert (> x 0))\n(check-sat)\n(reset)\n(echo \"after\")\n");
    assert_eq!(out.text(), "unsat\nafter\n");
    let got = f.received();
    assert_eq!(
        &got[..5],
        &[
            "launch -smt2 -in",
            "(declare-const x Int)",
            "(assert (> x 0))",
            "(check-sat)",
            "(reset)"
        ]
    );
    assert_eq!(f.counter("bypassed_sessions"), 1);
}

#[test]
fn bypass_option_and_pop_underflow() {
    assert_same(b"(set-option :print-success true)\n(check-sat)\n", &[]);
    assert_same(b"(push)(pop)(pop)(echo \"x\")(check-sat)", &[]);
    assert_same(b"(echo \"x\") stray-atom (check-sat)", &[]);
}

#[test]
fn exit_stops_reading() {
    let out = assert_same(&corpus("exit.smt2"), &[]);
    assert_eq!(out.text(), "before\n");
    assert_eq!(out.code, 0);
}

#[test]
fn incomplete_command_at_end_of_input_is_forwarded() {
    let f = Fixture::new("incomplete");
    let out = f.run(&corpus("incomplete.smt2"));
    assert_eq!(out.text(), "complete\n");
    assert_eq!(out.code, 0);
    // A real solver would complain about the incomplete command, so the
    // proxy hands it over rather than dropping it.
    assert_eq!(f.launches(), 1);
}

#[test]
fn solver_crash_passes_on_its_status() {
    let input = b"(echo \"one\")\n(check-sat)\n(echo \"two\")\n";
    let out = assert_same(input, &[("Z3STUB_ANSWERS", OsStr::new("crash:7"))]);
    assert_eq!(out.text(), "one\n");
    assert_eq!(out.code, 7);
}

#[test]
fn other_arguments_exec_the_real_solver() {
    let stub = stub();
    let env = [("Z3CACHE_REAL_Z3", stub.as_os_str())];
    let out = run(&z3cache(), &["--version"], b"", &env);
    assert_eq!(out.text(), "Z3 version 4.99.0 - 64 bit (stub)\n");
    let out = run(&z3cache(), &["-smt2", "file.smt2"], b"", &env);
    assert_eq!(out.code, 3);
}

#[test]
fn mode_off_execs_the_real_solver() {
    let f = Fixture::new("off");
    let out = f.run_env(b"(check-sat)", &[("Z3CACHE_MODE", OsStr::new("off"))]);
    assert_eq!(out.text(), "unsat\n");
    let log = std::fs::read_to_string(&f.log).unwrap();
    assert!(!log.contains("z3cache-sentinel"), "{log}");
    assert!(!f.cache.exists(), "off mode must not create a cache");
}

#[test]
fn invoked_as_z3_never_selects_the_admin_cli() {
    let stub = stub();
    let dir = temp_dir("symlink");
    let link = dir.join("z3");
    std::os::unix::fs::symlink(z3cache(), &link).unwrap();
    let out = run(
        &link,
        &["stats"],
        b"",
        &[("Z3CACHE_REAL_Z3", stub.as_os_str())],
    );
    assert_eq!(out.text(), "stub: unsupported arguments stats\n");
    let out = run(&z3cache(), &["help"], b"", &[]);
    assert!(out.text().starts_with("usage:"));
}

#[test]
fn the_real_solver_is_found_on_path_but_not_ourselves() {
    let stub = stub();
    let dir = temp_dir("path");
    let ours = dir.join("ours");
    let theirs = dir.join("theirs");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::create_dir_all(&theirs).unwrap();
    std::os::unix::fs::symlink(z3cache(), ours.join("z3")).unwrap();
    std::os::unix::fs::symlink(&stub, theirs.join("z3")).unwrap();
    let path = std::env::join_paths([&ours, &theirs]).unwrap();
    let out = run(
        &ours.join("z3"),
        &["--version"],
        b"",
        &[
            ("PATH", path.as_os_str()),
            ("Z3CACHE_REAL_Z3", OsStr::new("")),
        ],
    );
    assert_eq!(out.text(), "Z3 version 4.99.0 - 64 bit (stub)\n");
}

/// A Verus release directory in `dir`: `release/z3` is the stub solver, and
/// `release/rust_verify` is a copy of the stub, which as `rust_verify` runs
/// its arguments as a command (see `examples/stub_solver.rs`). It is a copy
/// because the kernel reports a process's executable with symlinks resolved
/// (`/proc/<pid>/exe`, `proc_pidpath`): a symlink would place the parent in
/// the stub's own directory. A script would not do either: its process is
/// the interpreter's.
fn verus_release(dir: &Path) -> PathBuf {
    let release = dir.join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::copy(stub(), release.join("rust_verify")).unwrap();
    std::os::unix::fs::symlink(stub(), release.join("z3")).unwrap();
    std::fs::canonicalize(release).unwrap()
}

#[test]
fn the_real_solver_is_found_next_to_the_parent_process() {
    let dir = temp_dir("parent");
    let release = verus_release(&dir);
    let driver = release.join("rust_verify");
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let log = dir.join("z3cache.log");
    let stub_log = dir.join("stub.log");
    // No setting and nothing on PATH: only the parent can lead to a solver.
    let env = [
        ("Z3CACHE_REAL_Z3", OsStr::new("")),
        ("PATH", empty.as_os_str()),
        ("Z3CACHE_LOG", log.as_os_str()),
        ("Z3STUB_LOG", stub_log.as_os_str()),
    ];
    let z3cache = z3cache();
    let z = z3cache.to_str().unwrap();

    let out = run(
        &driver,
        &[z, "-smt2", "-in"],
        b"(assert a)\n(check-sat)\n",
        &env,
    );
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    assert_eq!(launches(&stub_log), 1);
    // The solver is resolved to the file that runs: the stub that the
    // release's z3 links to.
    let want = format!(
        "real solver {} (next to {}, the program that started z3cache)",
        std::fs::canonicalize(stub()).unwrap().display(),
        driver.display()
    );
    assert!(file_contains(&log, &want), "{want:?} not in the log");

    // Other arguments go to the same solver.
    let out = run(&driver, &[z, "--version"], b"", &env);
    assert_eq!(out.text(), "Z3 version 4.99.0 - 64 bit (stub)\n");

    // Started by anything else, z3cache finds no solver, and says how to
    // set one up.
    let out = proxy_command(&z3cache, PROXY_ARGS, &env)
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no real z3 found"), "{err}");
    assert!(err.contains("run `z3cache setup`"), "{err}");

    // doctor, run from a shell, cannot look next to Verus: not a fault. An
    // explicit setting that does not work is.
    let doctor = run(&z3cache, &["doctor"], b"", &env);
    assert_eq!(doctor.code, 0, "{}", doctor.text());
    assert!(
        doctor.text().contains("real solver   none found from here"),
        "{}",
        doctor.text()
    );
    let mut bad = env.to_vec();
    bad[0].1 = OsStr::new("/nonexistent/z3");
    let doctor = run(&z3cache, &["doctor"], b"", &bad);
    assert_eq!(doctor.code, 1, "{}", doctor.text());
    assert!(
        doctor
            .text()
            .contains("real solver   NOT FOUND: Z3CACHE_REAL_Z3=/nonexistent/z3"),
        "{}",
        doctor.text()
    );
}

#[test]
fn the_real_solver_is_found_next_to_verus_on_path() {
    let dir = temp_dir("verus-on-path");
    let release = verus_release(&dir);
    let verus = release.join("verus");
    std::fs::write(&verus, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&verus, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // Verus installed as a symlink in a bin directory, without its z3.
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&verus, bin.join("verus")).unwrap();
    let log = dir.join("z3cache.log");
    let env = [
        ("Z3CACHE_REAL_Z3", OsStr::new("")),
        ("PATH", bin.as_os_str()),
        ("Z3CACHE_LOG", log.as_os_str()),
    ];
    let out = run(&z3cache(), &["--version"], b"", &env);
    assert_eq!(out.text(), "Z3 version 4.99.0 - 64 bit (stub)\n");
    let how = format!("next to {}, the verus on PATH", verus.display());
    assert!(file_contains(&log, &how), "{how:?} not in the log");

    let doctor = run(&z3cache(), &["doctor"], b"", &env);
    let text = doctor.text();
    assert_eq!(doctor.code, 0, "{text}");
    assert!(
        text.contains(&format!(
            "real solver   {} ({how})",
            std::fs::canonicalize(stub()).unwrap().display()
        )),
        "{text}"
    );
    assert!(
        text.contains("setting       real_z3 = none (found automatically) (default)"),
        "{text}"
    );
}

#[test]
fn cli_stats_clear_doctor() {
    let f = Fixture::new("cli");
    f.run(b"(assert a)\n(check-sat)\n");
    f.run(b"(assert a)\n(check-sat)\n");
    let text = f.cli(&["stats"]).text();
    assert!(text.contains("entries  1,"), "{text}");
    assert_eq!(f.counter("sessions"), 2);
    assert_eq!(f.counter("hits"), 1);
    assert_eq!(f.counter("misses"), 1);
    let cleared = f.cli(&["clear"]);
    assert_eq!(cleared.code, 0);
    assert!(
        cleared.text().starts_with("deleted 1 entries"),
        "{}",
        cleared.text()
    );
    f.run(b"(assert a)\n(check-sat)\n");
    assert_eq!(f.launches(), 2);
    let doctor = f.cli(&["doctor"]);
    assert_eq!(doctor.code, 0, "{}", doctor.text());
    assert!(
        doctor.text().contains("(from Z3CACHE_REAL_Z3)"),
        "{}",
        doctor.text()
    );
    assert!(
        doctor.text().contains("no solver started"),
        "{}",
        doctor.text()
    );
    let gc = f.cli(&["gc"]);
    assert_eq!(gc.code, 0, "{}", gc.text());
    assert!(
        gc.text().contains("within the cap: nothing evicted"),
        "{}",
        gc.text()
    );
    assert_eq!(f.cli(&["stats", "--bogus"]).code, 2);
}

/// The files in `dir` with extension `ext`.
fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .collect()
}

fn read_single(dir: &Path, ext: &str) -> String {
    let files = files_with_ext(dir, ext);
    assert_eq!(files.len(), 1, "{files:?}");
    std::fs::read_to_string(&files[0]).unwrap()
}

/// Query rows of the single session in `dir`: (key, context, query, verdict).
fn query_rows(dir: &Path) -> Vec<(String, String, String, String)> {
    read_single(dir, "tsv")
        .lines()
        .skip(1)
        .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|f| f[1] == "query")
        .map(|f| (f[3].clone(), f[4].clone(), f[5].clone(), f[6].clone()))
        .collect()
}

fn record(input: &[u8], name: &str) -> PathBuf {
    record_with(input, name, "unsat")
}

fn record_with(input: &[u8], name: &str, answers: &str) -> PathBuf {
    let stub = stub();
    let dir = temp_dir(name);
    let out = run(
        &z3cache(),
        PROXY_ARGS,
        input,
        &[
            ("Z3CACHE_REAL_Z3", stub.as_os_str()),
            ("Z3CACHE_RECORD", dir.as_os_str()),
            ("Z3STUB_ANSWERS", OsStr::new(answers)),
        ],
    );
    assert_eq!(out.code, 0);
    dir
}

fn find(dir: &Path, ext: &str) -> PathBuf {
    files_with_ext(dir, ext).swap_remove(0)
}

#[test]
fn recording_writes_transcripts_and_keys() {
    let input = corpus("verus_like.smt2");
    let answers = "unsat,sat,unsat";
    let dir = record_with(&input, "rec", answers);
    assert_eq!(std::fs::read(find(&dir, "in")).unwrap(), input);
    let direct = run(
        &stub(),
        PROXY_ARGS,
        &input,
        &[("Z3STUB_ANSWERS", OsStr::new(answers))],
    );
    assert_eq!(std::fs::read(find(&dir, "out")).unwrap(), direct.stdout);
    let meta = read_single(&dir, "meta");
    for field in [
        "argv=-smt2 -in\n",
        "exit_code=0\n",
        "bypass=\n",
        "errored=0\n",
        "end_unix_ms=",
    ] {
        assert!(meta.contains(field), "missing {field:?} in {meta}");
    }
    let rows = query_rows(&dir);
    assert_eq!(rows.len(), 3);
    let verdicts: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    assert_eq!(verdicts, vec!["unsat", "sat", "unsat"]);
    assert!(rows.iter().all(|r| r.0.len() == 64));
    // The two functions share the module context.
    assert_eq!(rows[0].1, rows[1].1);
    assert_ne!(rows[0].2, rows[1].2);
    // Nothing was cached, so nothing is recorded as served from the cache.
    let tsv = read_single(&dir, "tsv");
    assert!(
        tsv.lines().skip(1).all(|l| l.ends_with("\tsolver")),
        "{tsv}"
    );
}

#[test]
fn keys_ignore_formatting_but_not_content() {
    let base = "(declare-fun f (Int) Int)\n(push)\n(assert (> (f 1) 0))\n(check-sat)\n(pop)\n";
    let reformatted =
        "; comment\n(declare-fun  f  (Int)\n Int)(push)(assert (>\t(f 1)   0))(check-sat)(pop)";
    let changed_query =
        "(declare-fun f (Int) Int)\n(push)\n(assert (> (f 2) 0))\n(check-sat)\n(pop)\n";
    let changed_context = "(declare-fun f (Int) Int)(declare-const c Int)\n(push)\n(assert (> (f 1) 0))\n(check-sat)\n(pop)\n";
    let a = query_rows(&record(base.as_bytes(), "k1"));
    let b = query_rows(&record(reformatted.as_bytes(), "k2"));
    let c = query_rows(&record(changed_query.as_bytes(), "k3"));
    let d = query_rows(&record(changed_context.as_bytes(), "k4"));
    assert_eq!(a, b);
    assert_ne!(a[0].0, c[0].0);
    assert_eq!(a[0].1, c[0].1, "same context");
    assert_ne!(a[0].0, d[0].0);
    assert_eq!(a[0].2, d[0].2, "same query");
}

#[test]
fn analyze_reports_projected_hits() {
    let base = "(declare-fun f (Int) Int)\n(push)(assert (> (f 1) 0))(check-sat)(pop)\n\
                (push)(assert (> (f 2) 0))(check-sat)(pop)\n";
    let next = "(declare-fun f (Int) Int)\n(push)(assert (> (f 1) 0))(check-sat)(pop)\n\
                (push)(assert (> (f 3) 0))(check-sat)(pop)\n";
    let first = record(base.as_bytes(), "an1");
    let second = record(next.as_bytes(), "an2");
    let out = run(
        &z3cache(),
        &["analyze", first.to_str().unwrap(), second.to_str().unwrap()],
        b"",
        &[],
    );
    assert_eq!(out.code, 0);
    let text = out.text();
    assert!(text.contains("projection (nothing was cached)"), "{text}");
    let hit_line = text
        .lines()
        .find(|l| l.trim_start().starts_with("would hit"))
        .unwrap();
    assert!(hit_line.contains(" 1   50.0%"), "{text}");
    let new_line = text
        .lines()
        .find(|l| l.contains("would miss: new query"))
        .unwrap();
    assert!(new_line.contains(" 1   50.0%"), "{text}");
}

// Bypass, options and read-only modes.

#[test]
fn options_off_the_allowlist_bypass_and_are_never_cached() {
    // global-decls changes what pop undoes, so it must bypass.
    let f = Fixture::new("global-decls");
    let input = b"(set-option :global-decls true)\n(push)\n(declare-const p Bool)\n(check-sat)\n(pop)\n(check-sat)\n";
    let direct = run(&stub(), PROXY_ARGS, input, &[]);
    for _ in 0..2 {
        let out = f.run(input);
        assert_eq!(out.text(), direct.text());
    }
    assert_eq!(f.launches(), 2, "nothing may be answered from the cache");
    assert_eq!(f.counter("stores"), 0);
    assert_eq!(f.counter("bypassed_sessions"), 2);
    // Other spellings of an allowed option are not on the list either.
    let g = Fixture::new("spelling");
    g.run(b"(set-option :RLIMIT 5)\n(check-sat)\n");
    g.run(b"(set-option :RLIMIT 5)\n(check-sat)\n");
    assert_eq!(g.launches(), 2);
}

#[test]
fn parameters_off_the_allowlist_run_the_real_solver_directly() {
    let f = Fixture::new("argv");
    let out = run(
        &z3cache(),
        &["-smt2", "-in", "smt.random_seed=1"],
        b"(check-sat)\n",
        &[
            ("Z3CACHE_REAL_Z3", f.stub.as_os_str()),
            ("Z3CACHE_DIR", f.cache.as_os_str()),
            ("Z3STUB_LOG", f.log.as_os_str()),
        ],
    );
    assert_eq!(out.text(), "unsat\n");
    let log = std::fs::read_to_string(&f.log).unwrap();
    assert!(
        !log.contains("z3cache-sentinel"),
        "exec'd, not proxied: {log}"
    );
    assert!(!f.cache.exists());
}

#[test]
fn the_live_solver_gets_options_restored_after_pop() {
    // pop does not undo options in the solver, so the restoring
    // set-option must be sent after the frame is popped.
    let f = Fixture::new("rlimit-restore");
    let out = f.run(&corpus("rlimit_restore.smt2"));
    assert_eq!(out.text(), "unsat\nunsat\n");
    assert_eq!(
        f.received(),
        vec![
            "launch -smt2 -in",
            "(set-option :rlimit 0)",
            "(push)",
            "(set-option :rlimit 1)",
            "(check-sat)",
            "(pop)",
            "(set-option :rlimit 0)",
            "(check-sat)",
        ]
    );
}

#[test]
fn multiline_errors_during_replay_are_passed_on_whole() {
    // Replay output is passed on whole, not line by line.
    let input = b"(assert stub-multiline-error)\n(check-sat)\n";
    let out = assert_same(input, &[]);
    assert!(out.text().contains("  detail two\")\n"), "{}", out.text());
    // And with a cached query re-run in front of it.
    let f = Fixture::new("multiline");
    f.run(b"(assert a)\n(check-sat)\n");
    let input = b"(assert a)\n(check-sat)\n(assert stub-multiline-error)\n(check-sat)\n";
    let direct = run(&stub(), PROXY_ARGS, input, &[]);
    let out = f.run(input);
    assert_eq!(out.text(), direct.text());
    assert_eq!(f.counter("replay_mismatches"), 0);
}

#[test]
fn bypass_shifts_only_statistics_responses() {
    // An echo that looks like a statistics line must come out exactly as
    // asked. `pop` below the base frame starts a framed bypass.
    let f = Fixture::new("shift");
    f.run(b"(push)(assert a)(check-sat)(pop)");
    let out = f.run(
        b"(push)(assert a)(check-sat)(pop)(get-info :all-statistics)(pop)\n\
          (echo \":rlimit-count 0\")\n(get-info :all-statistics)\n",
    );
    let text = out.text();
    assert!(text.starts_with("unsat\n(:rlimit-count 100)\n"), "{text}");
    assert!(text.contains("\n:rlimit-count 0\n"), "{text}");
    // The stub's own counter restarted at 0; the client's does not go back.
    assert!(text.ends_with(" :rlimit-count 100)\n"), "{text}");
    assert_eq!(f.counter("bypassed_sessions"), 1);
}

#[test]
fn unknown_commands_bypass_raw_and_change_nothing() {
    // A command outside the tables may change output framing, so bypass is
    // raw: bytes pass unchanged, and rule 7 is not guaranteed (DESIGN
    // section 8).
    let f = Fixture::new("raw");
    f.run(b"(push)(assert a)(check-sat)(pop)");
    let out = f.run(
        b"(push)(assert a)(check-sat)(pop)(get-info :all-statistics)(reset)\n\
          (echo \":rlimit-count 0\")\n(get-info :all-statistics)\n",
    );
    assert_eq!(
        out.text(),
        "unsat\n(:rlimit-count 100)\n:rlimit-count 0\n(:max-memory   0.19\n :rlimit-count 0)\n"
    );
}

fn sqlite_state(dir: &Path) -> (i64, i64) {
    let conn = rusqlite::Connection::open(dir.join(FILE_NAME)).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let entries: i64 = conn
        .query_row("SELECT count(*) FROM entry", [], |r| r.get(0))
        .unwrap();
    (version, entries)
}

#[test]
fn read_only_modes_never_create_or_change_the_store() {
    for mode in ["ro", "enforce"] {
        let f = Fixture::new(&format!("ro-{mode}"));
        let env = [("Z3CACHE_MODE", OsStr::new(mode))];
        // No cache yet: nothing is created.
        f.run_env(b"(echo \"ok\")\n", &env);
        assert!(!f.cache.exists(), "{mode} created the cache directory");
        // A store with another schema version is neither migrated nor
        // emptied, and is not used.
        f.run(b"(assert a)\n(check-sat)\n");
        {
            let conn = rusqlite::Connection::open(f.cache.join(FILE_NAME)).unwrap();
            conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        }
        let out = f.run_env(b"(assert a)\n(check-sat)\n", &env);
        assert_eq!(sqlite_state(&f.cache), (999, 1), "{mode} changed the store");
        if mode == "enforce" {
            assert_eq!(out.code, 1, "an unusable store means a miss");
        } else {
            assert_eq!(out.text(), "unsat\n");
        }
    }
}

#[test]
fn options_are_restored_in_the_solver_at_pop() {
    // The restoring set-option must reach the solver before the next push,
    // not after it.
    let f = Fixture::new("restore-at-pop");
    f.run(&corpus("rlimit_restore_push.smt2"));
    assert_eq!(
        f.received(),
        vec![
            "launch -smt2 -in",
            "(set-option :rlimit 0)",
            "(push)",
            "(set-option :rlimit 1)",
            "(check-sat)",
            "(pop)",
            "(set-option :rlimit 0)",
            "(push)",
            "(check-sat)",
            "(pop)",
        ]
    );
}

#[test]
fn read_only_modes_leave_entries_and_schema_alone() {
    // ro and enforce never create the directory or database and never
    // change entries, counters, schema or user_version. SQLite may add its
    // -wal/-shm files next to an existing database.
    for mode in ["ro", "enforce"] {
        let f = Fixture::new(&format!("contract-{mode}"));
        f.run(b"(assert a)\n(check-sat)\n");
        let before = std::fs::read(f.cache.join(FILE_NAME)).unwrap();
        let counters = f.cli(&["stats", "--json"]).text();
        let names = |dir: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let existing = names(&f.cache);
        let out = f.run_env(
            b"(assert a)\n(check-sat)\n",
            &[("Z3CACHE_MODE", OsStr::new(mode))],
        );
        assert_eq!(out.text(), "unsat\n", "{mode} answers from the cache");
        assert_eq!(f.launches(), 1);
        for name in names(&f.cache) {
            assert!(
                existing.contains(&name)
                    || [format!("{FILE_NAME}-wal"), format!("{FILE_NAME}-shm")].contains(&name),
                "{mode} created {name}"
            );
        }
        assert_eq!(
            std::fs::read(f.cache.join(FILE_NAME)).unwrap(),
            before,
            "{mode}"
        );
        assert_eq!(sqlite_state(&f.cache), (1, 1));
        assert_eq!(
            f.cli(&["stats", "--json"]).text(),
            counters,
            "{mode} changed counters"
        );
    }
}

#[test]
fn read_only_modes_in_an_unwritable_directory_just_miss() {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["ro", "enforce"] {
        let f = Fixture::new(&format!("unwritable-{mode}"));
        f.run(b"(assert a)\n(check-sat)\n");
        // No writer is left, so SQLite would have to create its -shm file.
        std::fs::set_permissions(&f.cache, std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = f.run_env(
            b"(assert a)\n(check-sat)\n",
            &[("Z3CACHE_MODE", OsStr::new(mode))],
        );
        std::fs::set_permissions(&f.cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Either the read works, or the store is disabled and the query
        // misses; never a crash.
        match mode {
            "ro" => assert_eq!(out.text(), "unsat\n"),
            _ => assert!(
                out.text() == "unsat\n"
                    || (out.code == 1 && out.text().contains("cache miss in enforce mode")),
                "{}",
                out.text()
            ),
        }
        assert_ne!(out.code, 101, "{mode} panicked");
        assert_eq!(sqlite_state(&f.cache), (1, 1));
    }
}

/// Starts the proxy with stdin held open, sends `input`, waits for `ready`
/// on stdout, kills the solver, and requires the proxy to exit on its own.
fn solver_death_while_idle(f: &Fixture, input: &[u8]) -> i32 {
    let mut child = f.command(&[]).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    stdin.flush().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "no ready line");
        if line == "ready\n" {
            break;
        }
    }
    let pgrep = Command::new("pgrep")
        .args(["-P", &child.id().to_string()])
        .output()
        .unwrap();
    let solver = String::from_utf8(pgrep.stdout).unwrap();
    let solver = solver.trim();
    assert!(!solver.is_empty(), "no solver process");
    assert!(
        Command::new("kill")
            .args(["-9", solver])
            .status()
            .unwrap()
            .success()
    );
    // Generous: the proxy notices within 100 ms; this only catches a hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the proxy kept running after its solver died");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    drop(stdin);
    status.code().unwrap_or(-1)
}

#[test]
fn solver_death_ends_the_session_even_with_stdin_open() {
    // Framed bypass: a hit puts the client's counter ahead of the solver's,
    // then pop below the base frame.
    let f = Fixture::new("death-framed");
    f.run(b"(push)(assert a)(check-sat)(pop)");
    let code = solver_death_while_idle(
        &f,
        b"(push)(assert a)(check-sat)(pop)(pop)(echo \"ready\")\n",
    );
    assert_eq!(code, 137);
    // Raw bypass, and an ordinary caching session with a live solver.
    let f = Fixture::new("death-raw");
    assert_eq!(
        solver_death_while_idle(&f, b"(reset)(echo \"ready\")\n"),
        137
    );
    let f = Fixture::new("death-cached");
    assert_eq!(
        solver_death_while_idle(&f, b"(assert a)(check-sat)(echo \"ready\")\n"),
        137
    );
}

/// A solver that stops reading must push back on the client through the
/// pipes; the proxy may not queue the client's input without limit.
#[test]
fn input_is_not_buffered_without_limit_when_the_solver_stops_reading() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let stub = stub();
    let mut child = proxy_command(
        &z3cache(),
        PROXY_ARGS,
        &[("Z3CACHE_REAL_Z3", stub.as_os_str())],
    )
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // Enter raw bypass and wait until the solver is running.
    stdin.write_all(b"(reset)\n(echo \"ready\")\n").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    let ps = Command::new("pgrep")
        .args(["-P", &child.id().to_string()])
        .output()
        .unwrap();
    let solver_pid: i32 = String::from_utf8_lossy(&ps.stdout)
        .trim()
        .parse()
        .expect("solver pid");
    unsafe { libc::kill(solver_pid, libc::SIGSTOP) };

    // Offer 64 MiB of comments; the stopped solver reads none of it.
    let written = Arc::new(AtomicUsize::new(0));
    let w = written.clone();
    let writer = std::thread::spawn(move || {
        let chunk = vec![b';'; 1 << 16];
        for _ in 0..1024 {
            let mut c = chunk.clone();
            c[(1 << 16) - 1] = b'\n';
            if stdin.write_all(&c).is_err() {
                break;
            }
            w.fetch_add(c.len(), Ordering::SeqCst);
        }
    });
    std::thread::sleep(std::time::Duration::from_secs(2));
    let accepted = written.load(Ordering::SeqCst);
    // Pipes plus the proxy's bounded queue hold well under 4 MiB.
    let bounded = accepted < 4 << 20;
    unsafe {
        libc::kill(solver_pid, libc::SIGKILL);
        libc::kill(solver_pid, libc::SIGCONT);
    }
    let _ = child.wait();
    let _ = writer.join();
    assert!(
        bounded,
        "proxy accepted {accepted} bytes while the solver read nothing"
    );
}

// Locks, gc, store policies, solver lifetime and audits.

#[test]
fn store_all_keeps_failures_and_their_reads() {
    let f = Fixture::new("store-all");
    let all = ("Z3CACHE_STORE", OsStr::new("all"));
    let sat = ("Z3STUB_ANSWERS", OsStr::new("sat"));
    let input = b"(assert a)\n(check-sat)\n(get-value (x))\n";
    let first = f.run_env(input, &[all, sat]);
    assert_eq!(first.text(), "sat\n((x 0))\n");
    let second = f.run_env(input, &[all, sat]);
    assert_eq!(second.text(), first.text());
    assert_eq!(f.launches(), 1, "sat and its read were stored");
    assert_eq!(f.counter("stores"), 2);
    // The default policy does not use them (rule 8): failures are solved
    // again.
    f.run_env(input, &[sat]);
    assert_eq!(f.launches(), 2);
    // Never after a solver error, even under `all`.
    let g = Fixture::new("store-all-error");
    g.run_env(b"(assert stub-error)\n(check-sat)\n", &[all, sat]);
    assert_eq!(g.counter("stores"), 0);
}

#[test]
fn fresh_solvers_see_only_the_current_path() {
    let input = b"(declare-fun f () Int)\n(push)\n(assert (> f 1))\n(check-sat)\n(pop)\n\
                  (push)\n(assert (> f 2))\n(check-sat)\n(pop)\n";
    let keep = Fixture::new("keep");
    keep.run(input);
    assert_eq!(keep.launches(), 1);
    let fresh = Fixture::new("fresh");
    let out = fresh.run_env(input, &[("Z3CACHE_SOLVER", OsStr::new("fresh"))]);
    assert_eq!(out.text(), "unsat\nunsat\n");
    assert_eq!(out.code, 0);
    assert_eq!(
        fresh.received(),
        vec![
            "launch -smt2 -in",
            "(declare-fun f () Int)",
            "(push)",
            "(assert (> f 1))",
            "(check-sat)",
            "launch -smt2 -in",
            "(declare-fun f () Int)",
            "(push)",
            "(assert (> f 2))",
            "(check-sat)",
        ]
    );
}

#[test]
fn an_error_means_exit_status_1_whatever_the_last_solver_says() {
    // Z3 exits 1 after reporting an error; a session must too, even if the
    // solver that ends it (a fresh one, or one that hides it) exits 0.
    for lifetime in ["keep", "fresh"] {
        let f = Fixture::new(&format!("status-{lifetime}"));
        let out = f.run_env(
            b"(push)\n(assert stub-error)\n(check-sat)\n(pop)\n(assert ok)\n(check-sat)\n",
            &[
                ("Z3CACHE_SOLVER", OsStr::new(lifetime)),
                ("Z3STUB_EXIT_ZERO", OsStr::new("1")),
            ],
        );
        assert!(out.text().contains("(error"), "{}", out.text());
        assert_eq!(out.code, 1, "{lifetime}");
    }
}

#[test]
fn audits_correct_contradictions_and_report_instability() {
    let verify = ("Z3CACHE_VERIFY", OsStr::new("1"));
    let input = b"(assert a)\n(check-sat)\n";

    // Agreement: answered from the cache, but not counted as a hit.
    let f = Fixture::new("audit-agree");
    f.run(input);
    let out = f.run_env(input, &[verify]);
    assert_eq!(out.text(), "unsat\n");
    assert_eq!((f.counter("audits"), f.counter("hits")), (1, 0));
    assert_eq!(f.launches(), 2, "the audit ran its own solver");

    // Contradiction: the fresh answer is given, counted as a miss, and the
    // entry is gone.
    let f = Fixture::new("audit-contradiction");
    f.run(input);
    let out = f.run_env(input, &[verify, ("Z3STUB_ANSWERS", OsStr::new("sat"))]);
    assert_eq!(out.text(), "sat\n");
    assert_eq!(f.counter("audit_contradictions"), 1);
    assert_eq!((f.counter("hits"), f.counter("misses")), (0, 2));
    assert_eq!(
        f.cli(&["stats"]).text().lines().nth(1).unwrap(),
        "entries  0, 0.0 MB"
    );

    // Instability: answered from the cache, entry kept.
    let f = Fixture::new("audit-unstable");
    f.run(input);
    let out = f.run_env(input, &[verify, ("Z3STUB_ANSWERS", OsStr::new("unknown"))]);
    assert_eq!(out.text(), "unsat\n");
    assert_eq!(f.counter("audit_instabilities"), 1);
    f.run(input);
    assert_eq!(f.counter("hits"), 1, "the entry was kept");

    // A failed audit never changes the answer or the exit status.
    let f = Fixture::new("audit-crash");
    f.run(input);
    let out = f.run_env(input, &[verify, ("Z3STUB_ANSWERS", OsStr::new("crash:3"))]);
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    assert_eq!(f.counter("audit_failures"), 1);

    // No audits in enforce mode: it must never start a solver.
    let f = Fixture::new("audit-enforce");
    f.run(input);
    let out = f.run_env(input, &[verify, ("Z3CACHE_MODE", OsStr::new("enforce"))]);
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    assert_eq!(f.launches(), 1);
}

#[test]
fn misses_take_a_lock_file_per_key() {
    let f = Fixture::new("lockfile");
    f.run(b"(assert a)\n(check-sat)\n");
    let shards: Vec<_> = std::fs::read_dir(f.cache.join("locks")).unwrap().collect();
    assert_eq!(shards.len(), 1);
    let shard = shards[0].as_ref().unwrap().path();
    let files: Vec<_> = std::fs::read_dir(&shard)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files.len(), 1);
    let name = files[0].to_string_lossy().into_owned();
    assert_eq!(name.len(), 64);
    assert!(name.starts_with(&*shard.file_name().unwrap().to_string_lossy()));
    // An unusable lock directory only loses deduplication.
    let g = Fixture::new("lockfile-broken");
    std::fs::create_dir_all(&g.cache).unwrap();
    std::fs::write(g.cache.join("locks"), b"not a directory").unwrap();
    assert_eq!(g.run(b"(assert a)\n(check-sat)\n").text(), "unsat\n");
    assert_eq!(g.counter("stores"), 1);
}

fn entry_count(cache: &Path) -> i64 {
    sqlite_state(cache).1
}

#[test]
fn gc_evicts_to_the_cap_and_cleans_old_locks() {
    let f = Fixture::new("gc");
    f.run(&many_queries(0, 300));
    assert_eq!(entry_count(&f.cache), 300);
    // Recency has one-second resolution, so make it explicit: every entry
    // was last used two days ago, then one is used now (a hit refreshes
    // entries older than a day).
    {
        let conn = rusqlite::Connection::open(f.cache.join(FILE_NAME)).unwrap();
        conn.execute(
            "UPDATE entry SET last_used_s = strftime('%s', 'now') - 172800",
            [],
        )
        .unwrap();
    }
    let recent = b"(declare-const x Int)\n(push)(assert (> x 299))(check-sat)(pop)\n";
    assert_eq!(f.run(recent).text(), "unsat\n");
    assert_eq!(f.counter("hits"), 1);
    // An old unheld lock file goes; an old held one stays.
    let shard = f.cache.join("locks").join("ab");
    std::fs::create_dir_all(&shard).unwrap();
    let stale = shard.join(format!("ab{}", "0".repeat(62)));
    let held = shard.join(format!("ab{}", "1".repeat(62)));
    std::fs::write(&stale, b"").unwrap();
    let held_file = std::fs::File::create(&held).unwrap();
    {
        use std::os::fd::AsRawFd;
        assert_eq!(
            unsafe { libc::flock(held_file.as_raw_fd(), libc::LOCK_EX) },
            0
        );
    }
    for p in [&stale, &held] {
        assert!(
            Command::new("touch")
                .args(["-t", "202001010000"])
                .arg(p)
                .status()
                .unwrap()
                .success()
        );
    }
    let gc = run(
        &z3cache(),
        &["gc"],
        b"",
        &[
            ("Z3CACHE_DIR", f.cache.as_os_str()),
            ("Z3CACHE_MAX_SIZE", OsStr::new("64K")),
        ],
    );
    let text = gc.text();
    assert_eq!(gc.code, 0, "{text}");
    assert!(text.contains("evicted"), "{text}");
    assert!(text.contains("removed 1 lock files"), "{text}");
    assert!(text.contains("1 old ones are held"), "{text}");
    assert!(!stale.exists());
    assert!(held.exists());
    let left = entry_count(&f.cache);
    assert!(left > 0 && left < 300, "{left}");
    // The evicted entries were the least recently used: the one used just
    // before gc is still answered from the cache.
    assert_eq!(f.run(recent).text(), "unsat\n");
    assert_eq!(f.counter("hits"), 2);
    assert!(f.counter("gc_deleted") > 0);
    drop(held_file);
}

#[test]
fn some_sessions_start_eviction_at_exit() {
    let f = Fixture::new("gc-at-exit");
    let env = [
        ("Z3CACHE_GC_EVERY", OsStr::new("1")),
        ("Z3CACHE_MAX_SIZE", OsStr::new("1K")),
    ];
    let input = many_queries(0, 50);
    let start = std::time::Instant::now();
    let out = f.run_env(&input, &env);
    assert_eq!(out.code, 0);
    // The session itself does not wait for eviction; a background gc does
    // it shortly after.
    let deadline = start + std::time::Duration::from_secs(120);
    while entry_count(&f.cache) > 0 {
        assert!(std::time::Instant::now() < deadline, "no eviction happened");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Disabled with 0.
    let g = Fixture::new("gc-never");
    g.run_env(&input, &[("Z3CACHE_GC_EVERY", OsStr::new("0")), env[1]]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(entry_count(&g.cache), 50);
}

#[test]
fn reads_after_a_query_follow_the_answer_actually_given() {
    // get-model after sat is stored by an `all` session; a later session
    // whose query is unsat must not get that model.
    let input = b"(check-sat)\n(get-model)\n";
    let all = ("Z3CACHE_STORE", OsStr::new("all"));
    let unsat_direct = run(&stub(), PROXY_ARGS, input, &[]);

    let f = Fixture::new("read-verdict");
    let first = f.run_env(input, &[all, ("Z3STUB_ANSWERS", OsStr::new("sat"))]);
    assert!(first.text().starts_with("sat\n(\n"), "{}", first.text());
    let second = f.run(input);
    assert_eq!(second.text(), unsat_direct.text());
    assert_eq!(second.code, 1);

    // After an audit contradiction, the corrected answer's reads.
    let g = Fixture::new("read-after-audit");
    g.run_env(input, &[all, ("Z3STUB_ANSWERS", OsStr::new("sat"))]);
    let audited = g.run_env(input, &[all, ("Z3CACHE_VERIFY", OsStr::new("1"))]);
    assert_eq!(audited.text(), unsat_direct.text());
    assert_eq!(g.counter("audit_contradictions"), 1);
}

#[test]
fn a_holder_whose_client_stops_reading_does_not_block_waiters() {
    // Replay output larger than a pipe buffer, for a client that never
    // reads it, must not be written while the lock is held.
    // Handshake: the holder's stub logs the query only after the replay
    // (whose errors the holder must keep, not write), then holds its answer
    // behind a gate until the waiter has logged that it is waiting.
    let f = Fixture::new("blocked-reader");
    let dir = f.log.parent().unwrap().to_path_buf();
    let gate = dir.join("gate");
    let holder_log = dir.join("holder-stub.log");
    let waiter_log = dir.join("waiter.log");
    let mut input = "(assert stub-error)\n".repeat(10_000).into_bytes();
    input.extend_from_slice(b"(check-sat)\n");
    let answers = format!("gate:{}:unsat", gate.display());
    let mut holder = f
        .command(&[
            ("Z3STUB_LOG", holder_log.as_os_str()),
            ("Z3STUB_ANSWERS", OsStr::new(&answers)),
        ])
        .spawn()
        .unwrap();
    let mut holder_in = holder.stdin.take().unwrap();
    let data = input.clone();
    std::thread::spawn(move || {
        let _ = holder_in.write_all(&data);
        // Keep stdin open; never read stdout.
        std::thread::sleep(std::time::Duration::from_secs(300));
    });
    wait_until(
        "the holder has replayed and is solving under its lock",
        || file_contains(&holder_log, "(check-sat)"),
    );
    let (cache, stub) = (f.cache.clone(), f.stub.clone());
    let (wl, inp) = (waiter_log.clone(), input.clone());
    let waiter = std::thread::spawn(move || {
        run_bounded(
            &z3cache(),
            PROXY_ARGS,
            &inp,
            &[
                ("Z3CACHE_REAL_Z3", stub.as_os_str()),
                ("Z3CACHE_DIR", cache.as_os_str()),
                ("Z3CACHE_LOG", wl.as_os_str()),
            ],
            std::time::Duration::from_secs(120),
        )
    });
    wait_until("the waiter found the lock busy", || {
        file_contains(&waiter_log, LOCK_BUSY)
    });
    std::fs::write(&gate, b"").unwrap();
    let waiter = waiter.join().unwrap();
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(waiter.stdout.len() > 64 * 1024);
    assert!(waiter.text().ends_with("unsat\n"));
}

#[test]
fn a_stopped_holder_delays_waiters_only_up_to_the_limit() {
    // Handshake: the holder's stub never answers (its gate is never
    // opened); its logged query proves the holder holds the lock before it
    // is stopped.
    let f = Fixture::new("stopped-holder");
    let gate = f.log.parent().unwrap().join("never-opened");
    let input = b"(assert a)\n(check-sat)\n";
    let answers = format!("gate:{}:unsat", gate.display());
    let mut holder = f
        .command(&[("Z3STUB_ANSWERS", OsStr::new(&answers))])
        .spawn()
        .unwrap();
    holder.stdin.as_mut().unwrap().write_all(input).unwrap();
    wait_until("the holder is solving under its lock", || {
        file_contains(&f.log, "(check-sat)")
    });
    unsafe { libc::kill(holder.id() as i32, libc::SIGSTOP) };
    let start = std::time::Instant::now();
    let waiter = run_bounded(
        &z3cache(),
        PROXY_ARGS,
        input,
        &[
            ("Z3CACHE_REAL_Z3", f.stub.as_os_str()),
            ("Z3CACHE_DIR", f.cache.as_os_str()),
            ("Z3CACHE_LOCK_WAIT", OsStr::new("0.5")),
        ],
        std::time::Duration::from_secs(120),
    );
    let took = start.elapsed();
    unsafe {
        libc::kill(holder.id() as i32, libc::SIGKILL);
        libc::kill(holder.id() as i32, libc::SIGCONT);
    }
    let _ = holder.wait();
    assert_eq!(waiter.text(), "unsat\n");
    assert_eq!(f.counter("lock_timeouts"), 1);
    // The bound is the point: far below the 60 s default, with room for a
    // loaded machine.
    assert!(took < std::time::Duration::from_secs(30), "{took:?}");
}

#[test]
fn numeric_settings_out_of_range_only_warn() {
    let f = Fixture::new("bad-numbers");
    let log = f.log.parent().unwrap().join("z3cache.log");
    let out = f.run_env(
        b"(assert a)\n(check-sat)\n",
        &[
            ("Z3CACHE_LOCK_WAIT", OsStr::new("1e100")),
            ("Z3CACHE_VERIFY", OsStr::new("NaN")),
            ("Z3CACHE_MAX_SIZE", OsStr::new("99999999999999999999T")),
            ("Z3CACHE_GC_EVERY", OsStr::new("-1")),
            ("Z3CACHE_LOG", log.as_os_str()),
        ],
    );
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    for name in [
        "Z3CACHE_LOCK_WAIT",
        "Z3CACHE_VERIFY",
        "Z3CACHE_MAX_SIZE",
        "Z3CACHE_GC_EVERY",
    ] {
        assert!(file_contains(&log, &format!("invalid {name}")), "{name}");
    }
}

#[test]
fn error_shaped_echo_is_data() {
    // `(error` at the start of echo output is not a solver error.
    let echo = b"(echo \"(error \"\"hello\"\")\")\n";
    // Answered locally.
    let out = assert_same(echo, &[]);
    assert_eq!((out.text().as_str(), out.code), ("(error \"hello\")\n", 0));
    // In a raw bypass.
    let mut raw = b"(reset)\n".to_vec();
    raw.extend_from_slice(echo);
    let out = assert_same(&raw, &[]);
    assert_eq!(out.code, 0);
    // In a framed bypass (entered through pop below the base frame, itself
    // a real error) the echo is still passed on unchanged.
    let f = Fixture::new("framed-echo");
    f.run(b"(push)(assert a)(check-sat)(pop)");
    let mut framed = b"(push)(assert a)(check-sat)(pop)(pop)\n".to_vec();
    framed.extend_from_slice(echo);
    let out = f.run(&framed);
    assert!(
        out.text().ends_with("(error \"hello\")\n"),
        "{}",
        out.text()
    );
}

/// Writes `text` as a config file in a fresh directory.
fn config_file(name: &str, text: &[u8]) -> PathBuf {
    let p = temp_dir(name).join("config.toml");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn config_file_sets_the_cache_dir_and_the_solver() {
    let f = Fixture::new("config-dir");
    let config = config_file(
        "config-dir-file",
        format!(
            "# z3cache\n\n  real_z3 = \"{}\"\ndir   =   '{}'  # the cache\n",
            f.stub.display(),
            f.cache.display()
        )
        .as_bytes(),
    );
    // An empty variable falls through to the file.
    let env = [
        ("Z3CACHE_CONFIG", config.as_os_str()),
        ("Z3CACHE_DIR", OsStr::new("")),
        ("Z3STUB_LOG", f.log.as_os_str()),
    ];
    let input = b"(assert a)\n(check-sat)\n";
    for _ in 0..2 {
        let out = run(&z3cache(), PROXY_ARGS, input, &env);
        assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    }
    assert_eq!(f.launches(), 1);
    assert_eq!(f.counter("hits"), 1);
    // So do the command-line tools.
    let stats = run(&z3cache(), &["stats"], b"", &env[..2]);
    assert_eq!(stats.code, 0);
    let want = format!("cache    {}", f.cache.join(FILE_NAME).display());
    assert!(stats.text().starts_with(&want), "{}", stats.text());
    let cleared = run(&z3cache(), &["clear"], b"", &env[..2]);
    assert!(
        cleared.text().starts_with("deleted 1 entries"),
        "{}",
        cleared.text()
    );
}

#[test]
fn config_file_mode_off_execs_the_real_solver() {
    let f = Fixture::new("config-off");
    let config = config_file("config-off-file", b"mode = \"off\"\n");
    let out = f.run_env(b"(check-sat)", &[("Z3CACHE_CONFIG", config.as_os_str())]);
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    let log = std::fs::read_to_string(&f.log).unwrap();
    assert!(!log.contains("z3cache-sentinel"), "{log}");
    assert!(!f.cache.exists(), "off mode must not create a cache");
}

#[test]
fn environment_overrides_the_config_file() {
    let f = Fixture::new("config-env");
    let other = temp_dir("config-env-other").join("cache");
    let config = config_file(
        "config-env-file",
        format!("mode = \"off\"\ndir = \"{}\"\n", other.display()).as_bytes(),
    );
    let env = [
        ("Z3CACHE_CONFIG", config.as_os_str()),
        ("Z3CACHE_MODE", OsStr::new("rw")),
    ];
    let input = b"(assert a)\n(check-sat)\n";
    f.run_env(input, &env);
    let out = f.run_env(input, &env);
    assert_eq!((out.text().as_str(), out.code), ("unsat\n", 0));
    // Cached, in the directory from Z3CACHE_DIR rather than the file's.
    assert_eq!(f.counter("hits"), 1);
    assert_eq!(f.launches(), 1);
    assert!(!other.exists());
}

#[test]
fn a_bad_config_file_only_warns() {
    let input = corpus("verus_like.smt2");
    let answers = ("Z3STUB_ANSWERS", OsStr::new("unsat,sat,unsat"));
    let direct = run(&stub(), PROXY_ARGS, &input, &[answers]);

    // Valid TOML, but no setting in it is usable: the `mode` in the
    // `[z3cache]` table would make every run read-only.
    let malformed = config_file(
        "bad-config",
        b"colour = \"blue\"\nmax_size = \"lots\"\nmode = \"sometimes\"\nverify = true\n\
          [z3cache]\nmode = \"ro\"\n",
    );
    // Not TOML, or not UTF-8: all of it is ignored, including line 1.
    let invalid = config_file("bad-config-toml", b"mode = \"ro\"\nnonsense\n");
    let binary = config_file("bad-config-utf8", b"mode = \"ro\"\nlog = \"\xff\xfe\"\n");
    let unreadable = temp_dir("bad-config-dir");
    let fifo = temp_dir("bad-config-fifo").join("config.toml");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: mkfifo(3) on a path we own.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

    let m = malformed.display();
    let cases = [
        (
            &malformed,
            vec![
                format!("config file {m} line 1: unknown key \"colour\"; ignored"),
                format!("invalid max_size \"lots\" in {m} line 2; using 2G"),
                format!("unknown mode \"sometimes\" in {m} line 3; using rw"),
                format!("config file {m} line 4: verify must be a number, not a boolean"),
                format!("config file {m} line 5: unknown key \"z3cache\"; ignored"),
            ],
        ),
        (
            &invalid,
            vec![format!(
                "config file {} is not valid TOML: line 2, column 9: ",
                invalid.display()
            )],
        ),
        (
            &binary,
            vec![format!(
                "config file {} is not valid TOML: line 2: not UTF-8; ignoring it",
                binary.display()
            )],
        ),
        (
            &unreadable,
            vec![format!("cannot read config file {}", unreadable.display())],
        ),
        (
            &fifo,
            vec![format!(
                "cannot read config file {}: not a regular file",
                fifo.display()
            )],
        ),
    ];
    for (config, warnings) in cases {
        let f = Fixture::new("bad-config-run");
        let log = f.log.parent().unwrap().join("z3cache.log");
        // Cold, then warm: the warm run solves only the sat query again.
        for stub_answers in ["unsat,sat,unsat", "sat"] {
            let env = [
                ("Z3STUB_ANSWERS", OsStr::new(stub_answers)),
                ("Z3CACHE_CONFIG", config.as_os_str()),
                ("Z3CACHE_LOG", log.as_os_str()),
            ];
            let limit = std::time::Duration::from_secs(60);
            let out = run_bounded(&z3cache(), PROXY_ARGS, &input, &f.env(&env), limit);
            assert_eq!(out.text(), direct.text(), "{}", config.display());
            assert_eq!(out.code, direct.code);
        }
        assert_eq!(f.counter("hits"), 2, "{}", config.display());
        let logged = std::fs::read_to_string(&log).unwrap();
        for w in warnings {
            assert!(logged.contains(&w), "{w}\n{logged}");
        }
    }
}

#[test]
fn doctor_shows_the_config_file_and_sources() {
    let f = Fixture::new("config-doctor");
    let config = config_file(
        "config-doctor-file",
        b"max_size = \"500M\"\n# x\nmode = \"rw\"\n",
    );
    let env = [
        ("Z3CACHE_CONFIG", config.as_os_str()),
        ("Z3CACHE_DIR", f.cache.as_os_str()),
        ("Z3CACHE_REAL_Z3", f.stub.as_os_str()),
    ];
    let doctor = run(&z3cache(), &["doctor"], b"", &env);
    let text = doctor.text();
    assert_eq!(doctor.code, 0, "{text}");
    for line in [
        format!("config file   {} (found)\n", config.display()),
        "setting       max_size = 500M (config file, line 1)\n".into(),
        "setting       mode = rw (config file, line 3)\n".into(),
        format!(
            "setting       dir = {} (environment, Z3CACHE_DIR)\n",
            f.cache.display()
        ),
        "setting       verify = 0 (default)\n".into(),
        "setting       log = none (default)\n".into(),
        format!(
            "real solver   {} (from Z3CACHE_REAL_Z3)\n",
            std::fs::canonicalize(&f.stub).unwrap().display()
        ),
    ] {
        assert!(text.contains(&line), "{line}\n{text}");
    }
    assert!(text.contains("no solver started"), "{text}");
    // Without a file.
    let text = f.cli(&["doctor"]).text();
    let want = format!("config file   {} (not found)\n", no_config().display());
    assert!(text.contains(&want), "{text}");
    assert!(
        text.contains("setting       max_size = 2G (default)\n"),
        "{text}"
    );
    // With a file that is not TOML.
    let config = config_file("config-doctor-invalid", b"max_size = 500M\n");
    let env = [
        ("Z3CACHE_CONFIG", config.as_os_str()),
        ("Z3CACHE_DIR", f.cache.as_os_str()),
        ("Z3CACHE_REAL_Z3", f.stub.as_os_str()),
    ];
    let doctor = run(&z3cache(), &["doctor"], b"", &env);
    let text = doctor.text();
    assert_eq!(doctor.code, 0, "{text}");
    let c = config.display();
    for line in [
        format!("warning       config file {c} is not valid TOML: line 1, column 12: "),
        format!("config file   {c} (INVALID TOML: line 1, column 12: "),
        "setting       max_size = 2G (default)\n".into(),
    ] {
        assert!(text.contains(&line), "{line}\n{text}");
    }
}

/// A stand-in solver at `path`: a script that runs the stub with its first
/// `check-sat` answered `answer`, padded with a comment to exactly `len`
/// bytes, with a fixed modification time. Two stand-ins with different
/// answers can so have the same size and modification time.
fn stand_in(path: &Path, answer: &str, len: usize) {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "Z3STUB_ANSWERS={answer} exec '{}' \"$@\"\n",
        stub().display()
    );
    let head = "#!/bin/sh\n#";
    let pad = len
        .checked_sub(head.len() + 1 + body.len())
        .expect("stand-in too long");
    let script = format!("{head}{}\n{body}", "x".repeat(pad));
    assert_eq!(script.len(), len);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
        .unwrap();
}

/// Runs the proxy on `input` in `cwd`, with `env`.
fn run_in(cwd: &Path, input: &[u8], env: &[(&str, &OsStr)]) -> Run {
    use std::io::Read;
    let mut child = proxy_command(&z3cache(), PROXY_ARGS, env)
        .current_dir(cwd)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let code = child.wait().unwrap().code().unwrap_or(-1);
    Run { stdout, code }
}

/// Review finding 2: `Z3CACHE_REAL_Z3=./z3` names a different file in each
/// directory. With the same size and modification time, the second must
/// still not be answered with the first one's answers.
#[test]
fn a_relative_solver_path_is_one_file_per_directory() {
    let dir = temp_dir("relative-identity");
    let (a, b) = (dir.join("a"), dir.join("b"));
    stand_in(&a.join("z3"), "unsat", 300);
    stand_in(&b.join("z3"), "sat", 300);
    let cache = dir.join("cache");
    let env = [
        ("Z3CACHE_REAL_Z3", OsStr::new("./z3")),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    let input = b"(check-sat)\n";
    assert_eq!(run_in(&a, input, &env).text(), "unsat\n");
    assert_eq!(run_in(&a, input, &env).text(), "unsat\n");
    assert_eq!(counter(&cache, "hits"), 1);
    assert_eq!(run_in(&b, input, &env).text(), "sat\n");
}

/// Review finding 2: a bare `Z3CACHE_REAL_Z3=z3` names the file in the
/// current directory, and that file is both the one identified and the one
/// run, whatever is on PATH.
#[test]
fn a_bare_solver_name_is_one_file() {
    let dir = temp_dir("bare-identity");
    let (local, on_path) = (dir.join("local"), dir.join("path"));
    stand_in(&local.join("z3"), "sat", 300);
    stand_in(&on_path.join("z3"), "unsat", 300);
    let cache = dir.join("cache");
    let env = [
        ("Z3CACHE_REAL_Z3", OsStr::new("z3")),
        ("Z3CACHE_DIR", cache.as_os_str()),
        ("PATH", on_path.as_os_str()),
    ];
    let input = b"(check-sat)\n";
    // Seeded from the PATH directory: its z3 runs there.
    assert_eq!(run_in(&on_path, input, &env).text(), "unsat\n");
    assert_eq!(run_in(&on_path, input, &env).text(), "unsat\n");
    assert_eq!(counter(&cache, "hits"), 1);
    // From the other directory, its own z3, never the other's answer.
    assert_eq!(run_in(&local, input, &env).text(), "sat\n");
}

/// Review finding 3: after a pop, the result a read would read is gone
/// in the solver, even if the client's path looks as it did when it was
/// valid. The read is neither answered from the cache nor stored, and the
/// solver, which never had the popped frame, is made to discard its result
/// before it is asked.
#[test]
fn a_read_after_a_pop_goes_to_a_solver_that_has_discarded_its_result() {
    let f = Fixture::new("read-after-pop");
    let all = [
        ("Z3CACHE_STORE", OsStr::new("all")),
        ("Z3STUB_ANSWERS", OsStr::new("sat,unsat")),
    ];
    let input = b"(declare-const x Int)\n(check-sat)\n(get-model)\n(push)\n(assert false)\n\
        (check-sat)\n(pop)\n(get-model)\n";
    f.run_env(input, &all);
    let stored = f.counter("stores");
    let _ = std::fs::remove_file(&f.log);
    f.run_env(input, &all);
    // Both queries and the first read were hits; the last read was not.
    assert_eq!(f.counter("hits"), 3);
    assert_eq!(f.counter("stores"), stored, "nothing more stored");
    assert_eq!(
        f.received(),
        [
            "launch -smt2 -in",
            "(declare-const x Int)",
            "(check-sat)",
            "(push)",
            "(pop)",
            "(get-model)",
        ]
    );
}

/// Review finding 3, under the default policy: `get-info :reason-unknown`
/// after an unsat query is stored, but not served after a pop.
#[test]
fn reason_unknown_after_a_pop_is_not_served_from_the_cache() {
    let f = Fixture::new("reason-after-pop");
    let input = b"(assert false)\n(check-sat)\n(get-info :reason-unknown)\n(push)\n\
        (check-sat)\n(pop)\n(get-info :reason-unknown)\n";
    f.run(input);
    let _ = std::fs::remove_file(&f.log);
    f.run(input);
    // The two queries and the first read.
    assert_eq!(f.counter("hits"), 3);
    assert_eq!(
        f.received(),
        [
            "launch -smt2 -in",
            "(assert false)",
            "(check-sat)",
            "(push)",
            "(pop)",
            "(get-info :reason-unknown)",
        ]
    );
}
