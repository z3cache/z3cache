//! Tier 3: the proxy against a real Z3. Each corpus transcript must give the
//! same output and exit status through the proxy, cold and then warm, as
//! through Z3 directly, apart from error positions and statistics. Also
//! checks the DESIGN section 16 assumptions that a transcript can show.
//!
//! Skipped, with a message, when `Z3CACHE_REAL_Z3` is unset and no `z3` is
//! on `PATH`.

mod common;

use std::ffi::OsStr;

use common::*;

macro_rules! require_z3 {
    () => {
        match real_z3() {
            Some(z3) => z3,
            None => {
                eprintln!("SKIPPED: no real Z3 (set Z3CACHE_REAL_Z3 or put z3 on PATH)");
                return;
            }
        }
    };
}

const ARGS: &[&str] = &["-smt2", "-in"];

#[test]
fn corpus_matches_real_z3_cold_and_warm() {
    let z3 = require_z3!();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".smt2"))
        .collect();
    names.sort();
    assert!(!names.is_empty());
    // Each configuration gets its own cache and runs every file cold, then
    // warm.
    let configs: [(&str, &[(&str, &str)]); 3] = [
        ("default", &[]),
        ("fresh solvers", &[("Z3CACHE_SOLVER", "fresh")]),
        ("store all", &[("Z3CACHE_STORE", "all")]),
    ];
    for name in names {
        let input = corpus(&name);
        let direct = run(&z3, ARGS, &input, &[]);
        for (config, extra) in configs {
            let cache = temp_dir(&format!("diff-{name}"));
            let mut env = vec![
                ("Z3CACHE_REAL_Z3", z3.as_os_str()),
                ("Z3CACHE_DIR", cache.as_os_str()),
            ];
            env.extend(extra.iter().map(|(k, v)| (*k, OsStr::new(v))));
            for pass in ["cold", "warm"] {
                let proxied = run(&z3cache(), ARGS, &input, &env);
                assert_eq!(
                    normalize(&proxied.text()),
                    normalize(&direct.text()),
                    "{name}, {config}, {pass}"
                );
                assert_eq!(
                    proxied.code, direct.code,
                    "{name}, {config}, {pass}: exit status"
                );
            }
        }
    }
}

/// A1: `set-option` is not undone by `pop`.
#[test]
fn a1_options_survive_pop() {
    let z3 = require_z3!();
    let out = run(&z3, ARGS, &corpus("options_pop.smt2"), &[]);
    assert!(out.text().starts_with("unknown\n"), "{}", out.text());
}

/// A2: `echo` prints its string unquoted on one line, `""` unescaped.
#[test]
fn a2_echo_is_unquoted() {
    let z3 = require_z3!();
    let out = run(
        &z3,
        ARGS,
        b"(echo \"a b\")(echo \"q\"\"q\")(echo \"\")",
        &[],
    );
    assert_eq!(out.text(), "a b\nq\"q\n\n");
}

/// The proxy's sentinel is appended on the same line as the command, so the
/// solver must answer a command that has no trailing newline.
#[test]
fn z3_answers_without_a_trailing_newline() {
    let z3 = require_z3!();
    let out = run(
        &z3cache(),
        ARGS,
        b"(echo \"x\")",
        &[("Z3CACHE_REAL_Z3", z3.as_os_str())],
    );
    assert_eq!(out.text(), "x\n");
}

/// With `global-decls`, a definition made inside a frame survives `pop`. Two
/// sessions that differ only in that definition must not share an answer.
/// The second session's last query is sat in Z3; a cached unsat from the
/// first would be a false proof.
#[test]
fn global_decls_never_reuse_an_answer() {
    let z3 = require_z3!();
    let session = |value: &str| {
        format!(
            "(set-option :global-decls true)\n(push)\n(define-fun p () Bool {value})\n\
             (check-sat)\n(pop)\n(assert p)\n(check-sat)\n"
        )
    };
    let cache = temp_dir("global-decls");
    let env = [
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    for value in ["false", "true"] {
        let input = session(value);
        let direct = run(&z3, ARGS, input.as_bytes(), &[]);
        let proxied = run(&z3cache(), ARGS, input.as_bytes(), &env);
        assert_eq!(proxied.text(), direct.text(), "p = {value}");
    }
    let direct = run(&z3, ARGS, session("true").as_bytes(), &[]);
    assert_eq!(direct.text(), "sat\nsat\n");
}

/// Options restored inside a popped frame must be back in the solver before
/// anything that follows the pop, including a cached query that is re-run
/// and a read that depends on it.
#[test]
fn options_are_restored_at_pop_before_later_replay() {
    let z3 = require_z3!();
    let cache = temp_dir("restore-warm");
    let env = [
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    let base = "(set-option :rlimit 0)(declare-const x Int)";
    let q = "(push)(assert (> x 0))(assert (< x 0))(check-sat)";
    run(&z3cache(), ARGS, format!("{base}{q}(pop)").as_bytes(), &env);
    let input = format!(
        "{base}(push)(set-option :rlimit 1)(check-sat)(set-option :rlimit 0)(pop){q}(get-info :reason-unknown)"
    );
    let direct = run(&z3, ARGS, input.as_bytes(), &[]);
    let proxied = run(&z3cache(), ARGS, input.as_bytes(), &env);
    assert_eq!(proxied.text(), direct.text());
    assert_eq!(proxied.code, direct.code);
}

/// An option that makes Z3 quote `echo` output must not leave the proxy
/// waiting for its unquoted sentinel. Every case is bounded: a hang fails
/// the test.
#[test]
fn output_format_options_never_hang() {
    let z3 = require_z3!();
    let limit = std::time::Duration::from_secs(60);
    let cases = [
        // At the start, and after another bypass trigger.
        String::from("(set-option :smtlib2_compliant true)(echo \"ok\")(check-sat)"),
        String::from("(reset)(set-option :smtlib2_compliant true)(echo \"ok\")"),
    ];
    for input in &cases {
        let direct = run_bounded(&z3, ARGS, input.as_bytes(), &[], limit);
        let proxied = run_bounded(
            &z3cache(),
            ARGS,
            input.as_bytes(),
            &[("Z3CACHE_REAL_Z3", z3.as_os_str())],
            limit,
        );
        assert_eq!(
            normalize(&proxied.text()),
            normalize(&direct.text()),
            "{input}"
        );
        assert_eq!(proxied.code, direct.code, "{input}");
    }
    // After a framed bypass (counter ahead of the solver's, pop below the
    // base frame), the option switches the session to raw.
    let cache = temp_dir("framed-compliant");
    let env = [
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    let q = "(declare-const x Int)(push)(assert (> x 0))(assert (< x 0))(check-sat)(pop)";
    run_bounded(&z3cache(), ARGS, q.as_bytes(), &env, limit);
    let input = format!(
        "{q}(get-info :all-statistics)(pop)(set-option :smtlib2_compliant true)(echo \"ok\")(check-sat)"
    );
    let direct = run_bounded(&z3, ARGS, input.as_bytes(), &[], limit);
    let proxied = run_bounded(&z3cache(), ARGS, input.as_bytes(), &env, limit);
    assert_eq!(normalize(&proxied.text()), normalize(&direct.text()));
    assert_eq!(proxied.code, direct.code);
}

/// Echo text that looks like an error is data, in a raw bypass and in a
/// framed one, as well as when answered locally (corpus file
/// echo_error.smt2).
#[test]
fn error_shaped_echo_keeps_z3s_exit_status() {
    let z3 = require_z3!();
    let limit = std::time::Duration::from_secs(60);
    let echo = "(echo \"(error \"\"hello\"\")\")";
    let raw = format!("(reset){echo}(check-sat)");
    let direct = run_bounded(&z3, ARGS, raw.as_bytes(), &[], limit);
    assert_eq!(direct.code, 0);
    let proxied = run_bounded(
        &z3cache(),
        ARGS,
        raw.as_bytes(),
        &[("Z3CACHE_REAL_Z3", z3.as_os_str())],
        limit,
    );
    assert_eq!((proxied.text(), proxied.code), (direct.text(), direct.code));

    let cache = temp_dir("framed-echo");
    let env = [
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    let q = "(declare-const x Int)(push)(assert (> x 0))(assert (< x 0))(check-sat)(pop)";
    run_bounded(&z3cache(), ARGS, q.as_bytes(), &env, limit);
    let framed = format!("{q}(get-info :all-statistics)(pop){echo}");
    let direct = run_bounded(&z3, ARGS, framed.as_bytes(), &[], limit);
    let proxied = run_bounded(&z3cache(), ARGS, framed.as_bytes(), &env, limit);
    assert_eq!(normalize(&proxied.text()), normalize(&direct.text()));
    assert_eq!(proxied.code, direct.code);
}

/// Runs `input` through Z3 directly and through the proxy cold and then
/// warm on a cache of its own, with `extra` settings, and requires the same
/// output (apart from error positions and statistics) and exit status.
fn same_as_z3(z3: &std::path::Path, input: &[u8], extra: &[(&str, &str)], what: &str) {
    let limit = std::time::Duration::from_secs(60);
    let direct = run_bounded(z3, ARGS, input, &[], limit);
    let cache = temp_dir("same-as-z3");
    let mut env = vec![
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    env.extend(extra.iter().map(|(k, v)| (*k, OsStr::new(v))));
    for pass in ["cold", "warm"] {
        let proxied = run_bounded(&z3cache(), ARGS, input, &env, limit);
        assert_eq!(
            normalize(&proxied.text()),
            normalize(&direct.text()),
            "{what}, {pass}: {:?}",
            String::from_utf8_lossy(input)
        );
        assert_eq!(
            proxied.code,
            direct.code,
            "{what}, {pass}: exit status for {:?}",
            String::from_utf8_lossy(input)
        );
    }
}

/// A8 and review finding 1: Z3 4.16 skips only space, tab, line feed and
/// carriage return between tokens; every other ASCII control byte (and
/// DEL) is an error there, and the proxy must not drop it as whitespace.
/// Inside strings, quoted symbols and comments they are taken as they are,
/// except NUL, which cuts a string short. Every such byte, in these
/// positions, gives the same output and exit status through the proxy, cold
/// and warm, as through Z3.
#[test]
fn a8_control_bytes_are_read_as_z3_reads_them() {
    let z3 = require_z3!();
    use z3cache::profile::{SolverProfile, Z3};
    let lexicon = Z3.lexicon();
    let contexts: [(&str, &[u8], &[u8]); 5] = [
        ("between tokens", b"(assert", b"false)\n(check-sat)\n"),
        ("before a command", b"", b"(assert false)\n(check-sat)\n"),
        (
            "in a string",
            b"(echo \"a",
            b"b\")\n(assert false)\n(check-sat)\n",
        ),
        (
            "in a quoted symbol",
            b"(declare-const |a",
            b"b| Bool)\n(assert false)\n(check-sat)\n",
        ),
        ("in a comment", b"(assert false) ; x", b"y\n(check-sat)\n"),
    ];
    let bytes: Vec<u8> = (0u8..0x20).chain([0x7f]).collect();
    // Each byte on a thread of its own: some 500 runs in all.
    std::thread::scope(|scope| {
        for &b in &bytes {
            let z3 = &z3;
            scope.spawn(move || {
                // The assumption itself: Z3 skips exactly the profile's
                // whitespace.
                let mut input = b"(assert".to_vec();
                input.push(b);
                input.extend_from_slice(b"false)\n(check-sat)\n");
                let direct = run(z3, ARGS, &input, &[]);
                let skipped = direct.code == 0 && direct.text() == "unsat\n";
                assert_eq!(
                    skipped,
                    lexicon.whitespace.contains(&b),
                    "byte {b:#04x}: {}",
                    direct.text()
                );
                for (what, before, after) in contexts {
                    let mut input = before.to_vec();
                    input.push(b);
                    input.extend_from_slice(after);
                    same_as_z3(z3, &input, &[], &format!("byte {b:#04x} {what}"));
                }
            });
        }
    });
    // The review's input, under both store policies.
    for store in ["unsat", "all"] {
        same_as_z3(
            &z3,
            b"(assert\x0cfalse)\n(check-sat)\n",
            &[("Z3CACHE_STORE", store)],
            "form feed",
        );
    }
}

/// A10: one Z3 process's own `rlimit-count` never decreases, across
/// queries that pass, fail and run out of resources, `pop`, and changes to
/// the `rlimit` option. The proof of rule 7 (`src/rlimit/counter.rs`)
/// assumes it for the solver that bypass forwards to.
#[test]
fn a10_rlimit_count_never_decreases() {
    let z3 = require_z3!();
    let stats = "(get-info :all-statistics)";
    let input = format!(
        "(set-option :rlimit 0)(declare-const x Int)(declare-fun f (Int) Int)\n{stats}\n\
         (push)(assert (> x 0))(assert (< x 0))(check-sat){stats}(pop){stats}\n\
         (push)(assert (> (f x) 0))(check-sat){stats}(pop){stats}\n\
         (push)(assert (forall ((z Int)) (> (f (* z z)) (f z))))(assert (= (f 2) (f 4)))\
         (set-option :rlimit 2000)(check-sat)(set-option :rlimit 0){stats}(pop){stats}\n\
         (check-sat){stats}\n"
    );
    let out = run(&z3, ARGS, input.as_bytes(), &[]);
    let counts = rlimit_counts(&out.text());
    assert_eq!(counts.len(), 8, "{}", out.text());
    assert!(
        counts.windows(2).all(|w| w[0] <= w[1]),
        "rlimit-count went down: {counts:?}"
    );
    assert!(counts[7] > counts[0], "no work was counted: {counts:?}");
}

/// A9 and review finding 3: what makes Z3 4.16 discard the result of its
/// latest check-sat (a read then has nothing to read), and what does not.
/// The proxy's state (`State::read_verdict`) discards it on at least the
/// first kind and keeps it only across the second, and
/// `discard_result_commands` must discard it.
#[test]
fn a9_what_discards_a_result() {
    let z3 = require_z3!();
    use z3cache::profile::{SolverProfile, Z3, Z3_CACHED_OPTIONS};
    let after = |cmds: &str| {
        let input = format!(
            "(declare-const x Int)(declare-fun f (Int) Int)(assert (> x 0))(check-sat)\n{cmds}\n\
             (get-info :reason-unknown)"
        );
        let out = run(&z3, ARGS, input.as_bytes(), &[]).text();
        match out.strip_prefix("sat\n").unwrap_or(&out) {
            s if s.ends_with("(:reason-unknown \"\")\n") => true,
            s if s.ends_with(
                "(:reason-unknown \"state of the most recent check-sat command is not known\")\n",
            ) =>
            {
                false
            }
            s => panic!("{cmds}: {s}"),
        }
    };
    let discard = String::from_utf8(Z3.discard_result_commands().to_vec()).unwrap();
    for cmds in [
        "(push)",
        "(push)(pop)",
        "(pop 0)",
        "(assert (> x 1))",
        "(push)(check-sat)(pop)",
        discard.as_str(),
    ] {
        assert!(!after(cmds), "{cmds} keeps the result");
    }
    let mut keep = vec![
        String::new(),
        "(push 0)".into(),
        "(declare-const y Int)".into(),
        "(define-fun g () Int 1)".into(),
        "(echo \"e\")".into(),
        "(get-model)".into(),
        "(get-value (x))".into(),
        "(get-option :rlimit)".into(),
        "(get-info :version)".into(),
        "(get-info :all-statistics)".into(),
    ];
    for name in Z3_CACHED_OPTIONS {
        let value = match *name {
            "rlimit" | "smt.arith.solver" | "smt.case_split" => "3",
            "smt.qi.eager_threshold" => "100.0",
            _ => "false",
        };
        keep.push(format!("(set-option :{name} {value})"));
    }
    for cmds in &keep {
        assert!(after(cmds), "{cmds} discards the result");
    }
}
