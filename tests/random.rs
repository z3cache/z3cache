//! Tier 3, randomized: SMT-LIB sessions generated from fixed seeds, run
//! through Z3 directly and through the proxy cold and then warm. Outputs and
//! exit statuses must be identical apart from statistics and error
//! positions (`common::normalize`, and see `comparable`).
//!
//! All sessions of a configuration share one cache, and sessions come in
//! families that share a prefix, so a later session meets entries stored by
//! an earlier one with the same or a similar path: a key that left out
//! something that matters would serve a wrong answer here.
//!
//! The generator keeps a hidden witness assignment. An assertion either
//! holds under it or is recorded as possibly false, so the generator knows
//! when a path is certainly satisfiable. `get-value` is asked only then, and
//! only after pinning every constant (and `f` at their values) to the
//! witness, so the model it reads is unique: models are otherwise free to
//! differ with solver history (DESIGN section 13), which is not a cache
//! error.
//!
//! Skipped, with a message, when `Z3CACHE_REAL_Z3` is unset and no `z3` is
//! on `PATH`. `Z3CACHE_RANDOM_SESSIONS` changes the number of sessions.

mod common;

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use common::*;

const ARGS: &[&str] = &["-smt2", "-in"];
const SESSIONS: u64 = 300;
/// Sessions per family: they share options, declarations, witness and a
/// first stretch of commands.
const FAMILY: u64 = 4;
const LIMIT: Duration = Duration::from_secs(30);

/// SplitMix64: small, fast, and good enough to drive a generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

#[derive(Clone)]
struct Frame {
    ints: Vec<(String, i64)>,
    bools: Vec<(String, bool)>,
    /// Every assertion on the path up to here holds under the witness.
    satisfiable: bool,
}

struct Gen {
    rng: Rng,
    out: String,
    frames: Vec<Frame>,
    /// The witness interpretation of `f`: `f(v) = fa * v + fb`.
    fa: i64,
    fb: i64,
    /// The session enabled unsat cores (an option the proxy does not cache,
    /// so the whole session bypasses).
    cores: bool,
    /// Model-based quantifier instantiation is off, so quantified
    /// assertions are allowed: with it on, Z3 can search for a model of
    /// them indefinitely.
    mbqi_off: bool,
    /// The last command was a check whose answer is certainly `sat`.
    last_sat: bool,
}

fn int_lit(v: i64) -> String {
    if v < 0 {
        format!("(- {})", -v)
    } else {
        v.to_string()
    }
}

impl Gen {
    fn ints(&self) -> Vec<(String, i64)> {
        self.frames.iter().flat_map(|f| f.ints.clone()).collect()
    }

    fn bools(&self) -> Vec<(String, bool)> {
        self.frames.iter().flat_map(|f| f.bools.clone()).collect()
    }

    fn satisfiable(&self) -> bool {
        self.frames.last().unwrap().satisfiable
    }

    fn f(&self, v: i64) -> i64 {
        self.fa * v + self.fb
    }

    /// Emits one command, with its whitespace varied: spaces outside string
    /// literals and quoted symbols become newlines, runs of spaces or
    /// comments. The canonical form, and so the key, must not change.
    fn emit(&mut self, cmd: &str) {
        let text = self.vary_whitespace(cmd);
        self.emit_exact(&text);
    }

    /// Emits one command as given.
    fn emit_exact(&mut self, text: &str) {
        self.out.push_str(text);
        self.out
            .push_str(if self.rng.chance(85) { "\n" } else { " " });
        self.last_sat = false;
    }

    fn vary_whitespace(&mut self, cmd: &str) -> String {
        let mut text = String::new();
        let (mut in_str, mut in_sym) = (false, false);
        for c in cmd.chars() {
            match c {
                '"' if !in_sym => in_str = !in_str,
                '|' if !in_str => in_sym = !in_sym,
                ' ' if !in_str && !in_sym => {
                    match self.rng.below(12) {
                        0 => text.push_str("\n  "),
                        1 => text.push_str("  \t"),
                        2 => text.push_str(" ; a (comment\n"),
                        _ => text.push(' '),
                    }
                    continue;
                }
                _ => {}
            }
            text.push(c);
        }
        text
    }

    /// An integer term and its value under the witness. With `pinned`, `f`
    /// is applied only to constants, whose `f` values are pinned.
    fn int_term(&mut self, depth: u32, pinned: bool) -> (String, i64) {
        let ints = self.ints();
        let choice = if depth == 0 {
            self.rng.below(2)
        } else {
            self.rng.below(7)
        };
        match choice {
            0 => {
                let v = self.rng.range(-4, 4);
                (int_lit(v), v)
            }
            1 => self.rng.pick(&ints).clone(),
            2 => {
                let (a, x) = self.int_term(depth - 1, pinned);
                let (b, y) = self.int_term(depth - 1, pinned);
                (format!("(+ {a} {b})"), x + y)
            }
            3 => {
                let (a, x) = self.int_term(depth - 1, pinned);
                let (b, y) = self.int_term(depth - 1, pinned);
                (format!("(- {a} {b})"), x - y)
            }
            4 => {
                let k = self.rng.range(-3, 3);
                let (a, x) = self.int_term(depth - 1, pinned);
                (format!("(* {} {a})", int_lit(k)), k * x)
            }
            5 => {
                let (a, x) = if pinned {
                    self.rng.pick(&ints).clone()
                } else {
                    self.int_term(depth - 1, pinned)
                };
                (format!("(f {a})"), self.f(x))
            }
            _ => {
                let (p, c) = self.bool_term(depth - 1, pinned);
                let (a, x) = self.int_term(depth - 1, pinned);
                let (b, y) = self.int_term(depth - 1, pinned);
                (format!("(ite {p} {a} {b})"), if c { x } else { y })
            }
        }
    }

    /// A boolean term and its value under the witness.
    fn bool_term(&mut self, depth: u32, pinned: bool) -> (String, bool) {
        let bools = self.bools();
        let choice = if depth == 0 { 0 } else { self.rng.below(11) };
        match choice {
            0 => self.rng.pick(&bools).clone(),
            1 => {
                let (p, v) = self.bool_term(depth - 1, pinned);
                (format!("(not {p})"), !v)
            }
            2..=4 => {
                let (p, v) = self.bool_term(depth - 1, pinned);
                let (q, w) = self.bool_term(depth - 1, pinned);
                match choice {
                    2 => (format!("(and {p} {q})"), v && w),
                    3 => (format!("(or {p} {q})"), v || w),
                    _ => (format!("(=> {p} {q})"), !v || w),
                }
            }
            _ => {
                let (a, x) = self.int_term(depth - 1, pinned);
                let (b, y) = self.int_term(depth - 1, pinned);
                let ops: [(&str, bool); 6] = [
                    ("<", x < y),
                    ("<=", x <= y),
                    (">", x > y),
                    (">=", x >= y),
                    ("=", x == y),
                    ("distinct", x != y),
                ];
                let (op, v) = *self.rng.pick(&ops);
                (format!("({op} {a} {b})"), v)
            }
        }
    }

    fn declare_int(&mut self, name: String) {
        let v = self.rng.range(-5, 5);
        self.emit(&format!("(declare-const {name} Int)"));
        self.frames.last_mut().unwrap().ints.push((name, v));
    }

    fn declare_bool(&mut self, name: String) {
        let v = self.rng.chance(50);
        self.emit(&format!("(declare-const {name} Bool)"));
        self.frames.last_mut().unwrap().bools.push((name, v));
    }

    /// Asserts a term; most are made to hold under the witness.
    fn assert(&mut self) {
        let depth = 1 + self.rng.below(3) as u32;
        let (mut p, mut v) = self.bool_term(depth, false);
        if !v && self.rng.chance(60) {
            p = format!("(not {p})");
            v = true;
        }
        self.emit(&format!("(assert {p})"));
        self.frames.last_mut().unwrap().satisfiable &= v;
    }

    fn push(&mut self, n: usize) {
        let cmd = if n == 1 && self.rng.chance(70) {
            "(push)".to_string()
        } else {
            format!("(push {n})")
        };
        self.emit(&cmd);
        for _ in 0..n {
            let top = Frame {
                ints: Vec::new(),
                bools: Vec::new(),
                satisfiable: self.satisfiable(),
            };
            self.frames.push(top);
        }
    }

    fn pop(&mut self, n: usize) {
        let cmd = if n == 1 && self.rng.chance(70) {
            "(pop)".to_string()
        } else {
            format!("(pop {n})")
        };
        self.emit(&cmd);
        for _ in 0..n {
            self.frames.pop();
        }
    }

    /// Checks are made inside a frame, as Verus makes them. Z3 4.16.0
    /// switches to its incremental solver at the first `push`, and the two
    /// can disagree on `unknown` against `sat` for quantified paths; the
    /// proxy sends a frame's `push` only when it needs the solver, so a
    /// check at the base frame can run in the other mode than in Z3 alone.
    fn enter_frame(&mut self) {
        if self.frames.len() == 1 {
            self.push(1);
        }
    }

    fn check_sat(&mut self) {
        self.enter_frame();
        self.emit("(check-sat)");
        if self.rng.chance(20) {
            self.emit("(get-info :reason-unknown)");
        } else {
            self.last_sat = self.satisfiable();
        }
    }

    fn check_sat_assuming(&mut self) {
        self.enter_frame();
        let bools = self.bools();
        let mut lits = Vec::new();
        let mut all_true = true;
        for _ in 0..1 + self.rng.below(3) {
            let (b, v) = self.rng.pick(&bools).clone();
            // Mostly assume what the witness has, sometimes the opposite.
            let positive = if self.rng.chance(75) { v } else { !v };
            all_true &= positive == v;
            lits.push(if positive { b } else { format!("(not {b})") });
        }
        // With cores enabled, sometimes make the assumptions contradictory,
        // so that a core exists.
        let core = self.cores && self.rng.chance(50);
        if core {
            let (b, _) = self.rng.pick(&bools).clone();
            lits.push(b.clone());
            lits.push(format!("(not {b})"));
        }
        self.emit(&format!("(check-sat-assuming ({}))", lits.join(" ")));
        if core {
            self.emit("(get-unsat-core)");
        } else {
            self.last_sat = self.satisfiable() && all_true;
        }
    }

    /// Pins every constant, and `f` at their values, to the witness, checks
    /// (certainly `sat`) and asks for values the pins determine.
    fn pinned_get_value(&mut self) {
        let ints = self.ints();
        let bools = self.bools();
        self.push(1);
        for (x, v) in &ints {
            self.emit(&format!("(assert (= {x} {}))", int_lit(*v)));
            self.emit(&format!("(assert (= (f {x}) {}))", int_lit(self.f(*v))));
        }
        for (b, v) in &bools {
            self.emit(&format!("(assert (= {b} {v}))"));
        }
        self.emit("(check-sat)");
        let mut terms = Vec::new();
        for _ in 0..1 + self.rng.below(3) {
            let depth = self.rng.below(3) as u32;
            terms.push(if self.rng.chance(60) {
                self.int_term(depth, true).0
            } else {
                self.bool_term(depth, true).0
            });
        }
        // Z3 prints each term of a `get-value` as the client wrote it, and
        // the proxy sends the canonical form, so these terms are kept
        // canonical (see `get_value_echoes_the_terms_as_written`).
        self.emit_exact(&format!("(get-value ({}))", terms.join(" ")));
    }

    fn echo(&mut self) {
        let pieces = [
            "a", "b c", "\"\"", "(", ")", ";", "|", "\\", " ", "x1", "<<DONE>>",
        ];
        let mut s = String::new();
        for _ in 0..self.rng.below(5) {
            s.push_str(self.rng.pick(&pieces));
        }
        self.emit(&format!("(echo \"{s}\")"));
    }

    /// Options, declarations and a first stretch of commands, shared by a
    /// family of sessions.
    fn prefix(&mut self) {
        if self.rng.chance(10) {
            self.cores = true;
            self.emit("(set-option :produce-unsat-cores true)");
        }
        if self.rng.chance(30) {
            self.emit("(set-option :auto_config false)");
        }
        if self.rng.chance(50) {
            let on = self.rng.chance(30);
            self.mbqi_off = !on;
            self.emit(&format!("(set-option :smt.mbqi {on})"));
        }
        if self.rng.chance(40) {
            self.set_rlimit();
        }
        self.emit("(declare-fun f (Int) Int)");
        for i in 0..2 + self.rng.below(3) {
            self.declare_int(format!("x{i}"));
        }
        if self.rng.chance(30) {
            self.declare_int("|x y|".to_string());
        }
        for i in 0..1 + self.rng.below(3) {
            self.declare_bool(format!("b{i}"));
        }
        for _ in 0..self.rng.below(6) {
            self.step();
        }
    }

    /// No limit, or one no generated check comes near.
    fn set_rlimit(&mut self) {
        let limit = *self.rng.pick(&[0, 0, 10_000_000]);
        self.emit(&format!("(set-option :rlimit {limit})"));
    }

    /// A quantified assertion about `f`, made only with model-based
    /// quantifier instantiation off. Z3 then cannot show such a path
    /// satisfiable and answers `unknown`. Its patterns create no new terms,
    /// so instantiation ends. These are the generator's source of `unknown`, as a check
    /// near its resource limit can land on either side of it depending on
    /// solver history (DESIGN section 13), which is not a cache error.
    fn assert_quantified(&mut self) {
        let k = self.rng.range(-3, 3);
        let c = self.rng.range(-20, 0);
        let body = match self.rng.below(3) {
            0 => format!("(=> (> y {}) (> (f y) {}))", int_lit(k), int_lit(c)),
            1 => format!("(>= (f y) (- y {}))", int_lit(k.abs())),
            _ => format!("(not (= (f y) {}))", int_lit(c - 100)),
        };
        self.emit(&format!(
            "(assert (forall ((y Int)) (! {body} :pattern ((f y)))))"
        ));
        self.frames.last_mut().unwrap().satisfiable = false;
    }

    fn step(&mut self) {
        let depth = self.frames.len() - 1;
        match self.rng.below(100) {
            0..=24 => self.assert(),
            25..=36 => {
                let n = if self.rng.chance(80) { 1 } else { 2 };
                self.push(n);
            }
            37..=48 if depth > 0 => {
                let n = 1 + self.rng.below(depth.min(3));
                self.pop(n);
            }
            49..=51 => {
                // Siblings reuse names: what a popped frame declared must
                // not leak into a sibling's key.
                let top = self.frames.last().unwrap();
                let n = top.ints.len() + top.bools.len();
                if self.rng.chance(60) {
                    self.declare_int(format!("y{depth}_{n}"));
                } else {
                    self.declare_bool(format!("c{depth}_{n}"));
                }
            }
            52..=63 => self.check_sat(),
            64..=69 => self.check_sat_assuming(),
            70..=71 => self.set_rlimit(),
            72..=74 if self.mbqi_off => self.assert_quantified(),
            75..=78 => self.echo(),
            79..=80 => self.emit("(get-info :version)"),
            81..=82 => self.emit("(get-info :all-statistics)"),
            // Without cores enabled, Z3 reports an error.
            83 if !self.cores && self.rng.chance(25) => self.emit("(get-unsat-core)"),
            84 if depth == 0 && self.rng.chance(30) => {
                // Below the base frame: Z3 reports an error.
                self.emit("(pop)");
            }
            _ => self.assert(),
        }
    }

    fn session(seed: u64) -> String {
        let family = seed / FAMILY;
        let mut rng = Rng::new(family);
        let base = Frame {
            ints: Vec::new(),
            bools: Vec::new(),
            satisfiable: true,
        };
        let mut g = Gen {
            fa: rng.range(-2, 2),
            fb: rng.range(-3, 3),
            rng,
            out: String::new(),
            frames: vec![base],
            cores: false,
            mbqi_off: false,
            last_sat: false,
        };
        g.prefix();
        g.rng = Rng::new(seed.wrapping_add(1 << 32));
        for _ in 0..15 + g.rng.below(25) {
            if g.last_sat && g.rng.chance(50) {
                g.pinned_get_value();
            } else {
                g.step();
            }
        }
        if g.rng.chance(20) {
            g.emit("(exit)");
        }
        g.out
    }
}

/// [`normalize`], and also: after a `sat` or `unsat` answer, Z3's
/// `:reason-unknown` describes solver history rather than the query (Z3
/// 4.16.0 gives `"unknown"` after an earlier push/pop and `""` without
/// one), and the proxy sends a `push` only when it needs the solver. Verus
/// asks for the reason only after `unknown`, where it is compared.
fn comparable(out: &str) -> String {
    let mut o = String::new();
    let mut last = "";
    for line in normalize(out).split_inclusive('\n') {
        if line.starts_with("(:reason-unknown ") && last != "unknown\n" {
            o.push_str("(:reason-unknown <after a definite answer>)\n");
            continue;
        }
        if matches!(line, "sat\n" | "unsat\n" | "unknown\n") {
            last = line;
        }
        o.push_str(line);
    }
    o
}

fn describe(
    seed: u64,
    config: &str,
    pass: &str,
    session: &str,
    want: &Run,
    got: Option<&Run>,
) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "seed {seed}, {config}, {pass}: proxy output differs from Z3"
    );
    let _ = writeln!(s, "--- session (Gen::session({seed}))\n{session}");
    let _ = writeln!(
        s,
        "--- z3, exit {}\n{}",
        want.code,
        comparable(&want.text())
    );
    match got {
        Some(r) => {
            let _ = writeln!(s, "--- z3cache, exit {}\n{}", r.code, comparable(&r.text()));
        }
        None => {
            let _ = writeln!(s, "--- z3cache did not finish within {LIMIT:?}");
        }
    }
    s
}

/// Runs every session through `z3cache` with one cache, cold then warm,
/// and returns the first difference from Z3.
fn run_config(
    z3: &Path,
    config: &str,
    extra: &[(&str, &str)],
    sessions: &[(String, Run)],
) -> Option<String> {
    let cache = temp_dir(&format!("random-{}", config.replace(' ', "-")));
    let mut env = vec![
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    env.extend(extra.iter().map(|(k, v)| (*k, OsStr::new(v))));
    for pass in ["cold", "warm"] {
        for (seed, (session, want)) in sessions.iter().enumerate() {
            let got = run_within(&z3cache(), ARGS, session.as_bytes(), &env, LIMIT);
            let same = got.as_ref().is_some_and(|r| {
                r.code == want.code && comparable(&r.text()) == comparable(&want.text())
            });
            if !same {
                return Some(describe(
                    seed as u64,
                    config,
                    pass,
                    session,
                    want,
                    got.as_ref(),
                ));
            }
        }
    }
    let n = |name| counter(&cache, name);
    eprintln!(
        "{config}: {} hits, {} misses, {} stored, {} solver starts, {} bypassed sessions",
        n("hits"),
        n("misses"),
        n("stores"),
        n("solver_starts"),
        n("bypassed_sessions")
    );
    let _ = std::fs::remove_dir_all(&cache);
    None
}

#[test]
fn random_sessions_match_real_z3() {
    let Some(z3) = real_z3() else {
        eprintln!("SKIPPED: no real Z3 (set Z3CACHE_REAL_Z3 or put z3 on PATH)");
        return;
    };
    let n: u64 = std::env::var("Z3CACHE_RANDOM_SESSIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SESSIONS);
    let sessions: Vec<String> = (0..n).map(Gen::session).collect();

    // Z3's own answers, in parallel.
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    let direct: Vec<Run> = std::thread::scope(|s| {
        let chunks: Vec<_> = sessions
            .chunks(sessions.len().div_ceil(threads))
            .map(|chunk| {
                let z3 = &z3;
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|session| {
                            run_within(z3, ARGS, session.as_bytes(), &[], LIMIT).unwrap_or_else(
                                || panic!("z3 did not finish within {LIMIT:?}:\n{session}"),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        chunks.into_iter().flat_map(|c| c.join().unwrap()).collect()
    });
    let pairs: Vec<(String, Run)> = sessions.into_iter().zip(direct).collect();

    let mut verdicts = [0usize; 3];
    for (_, r) in &pairs {
        for line in r.text().lines() {
            match line {
                "sat" => verdicts[0] += 1,
                "unsat" => verdicts[1] += 1,
                "unknown" => verdicts[2] += 1,
                _ => {}
            }
        }
    }
    let errored = pairs.iter().filter(|(_, r)| r.code != 0).count();
    eprintln!(
        "{n} sessions: {} sat, {} unsat, {} unknown answers; {errored} sessions with errors",
        verdicts[0], verdicts[1], verdicts[2]
    );
    assert!(
        verdicts.iter().all(|&v| v > 0),
        "the generator should give every verdict"
    );

    // Each configuration has its own cache; they run side by side.
    let configs: [(&str, &[(&str, &str)]); 3] = [
        ("default", &[]),
        ("store all", &[("Z3CACHE_STORE", "all")]),
        ("fresh solvers", &[("Z3CACHE_SOLVER", "fresh")]),
    ];
    let failures: Vec<String> = std::thread::scope(|s| {
        let runs: Vec<_> = configs
            .iter()
            .map(|(name, extra)| {
                let (z3, pairs) = (&z3, &pairs);
                s.spawn(move || run_config(z3, name, extra, pairs))
            })
            .collect();
        runs.into_iter().filter_map(|r| r.join().unwrap()).collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Z3 prints each term of a `get-value` reply exactly as the client wrote it,
/// whitespace and comments included, so the proxy must key and send
/// `get-value` as written rather than in canonical form, cold and warm, and
/// two spellings of the same term must not share an entry.
#[test]
fn get_value_echoes_the_terms_as_written() {
    let Some(z3) = real_z3() else {
        eprintln!("SKIPPED: no real Z3 (set Z3CACHE_REAL_Z3 or put z3 on PATH)");
        return;
    };
    let cache = temp_dir("get-value-text");
    let env = [
        ("Z3CACHE_REAL_Z3", z3.as_os_str()),
        ("Z3CACHE_DIR", cache.as_os_str()),
    ];
    let session = |term: &str| {
        format!(
            "(declare-const x Int)(assert (= x 1))(push)(assert (> x 0))(check-sat)(get-value ({term}))(pop)"
        )
    };
    for term in ["(+ x 1)", "(+  x   1)", "(+ x ; one\n 1)"] {
        let input = session(term);
        let direct = run(&z3, ARGS, input.as_bytes(), &[]);
        let proxied = run(&z3cache(), ARGS, input.as_bytes(), &env);
        assert_eq!(proxied.text(), direct.text(), "{term:?}");
    }
}
