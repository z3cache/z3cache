//! The proxy loop (DESIGN section 7).
//!
//! Silent, scope and option commands only update the tracked state. Queries
//! and reads are looked up by key: a hit writes the stored response; a miss
//! materializes the solver (starting it if needed and sending the unsent
//! part of the path) and asks it. A session whose queries all hit never
//! starts a solver. Commands outside the class tables switch to bypass:
//! materialize, then forward the client's commands one by one (or, where
//! responses cannot be told apart, copy bytes verbatim).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::command::{Class, classify};
use crate::config::{Config, Lifetime, Mode, StorePolicy};
use crate::lock;
use crate::log;
use crate::profile::SolverProfile;
use crate::record::{Record, Recorder, join_args};
use crate::sexp::{Command, Splitter};
use crate::solver::{Solver, hash_executable, solver_id};
use crate::state::{Hashes, KeyContext, State};
use crate::store::{Entry, Key, SolverInfo, SolverStamp, SqliteStore, Store, now_s};

/// What the caller of a step should do next.
enum Step {
    Continue,
    Done(i32),
    /// Switch to bypass, starting with `trigger` if there is one. `raw`:
    /// responses cannot be framed, so copy bytes verbatim.
    Bypass {
        reason: String,
        trigger: Option<Command>,
        raw: bool,
    },
}

/// The session must end with this exit code.
struct Stop(i32);

/// What the client-input thread hands to the session.
/// Chunks of client input (at most 64 KiB each) queued ahead of the session.
const INPUT_QUEUE: usize = 4;

enum Input {
    Data(Vec<u8>),
    End,
}

/// How often an idle session checks that its solver is still alive.
const IDLE_CHECK: Duration = Duration::from_millis(100);

/// A running solver and how much of the path it has received.
struct Live {
    solver: Solver,
    /// `sent[i]`: number of replay entries of frame `i` already sent. The
    /// solver's stack depth is `sent.len() - 1`.
    sent: Vec<usize>,
    /// The option values the solver has received. `pop` does not undo them
    /// (assumption A1), so they can differ from the client's.
    options: BTreeMap<Vec<u8>, Vec<u8>>,
}

#[derive(Default)]
struct Counts {
    hits: i64,
    misses: i64,
    stores: i64,
    saved_us: i64,
    solver_starts: i64,
    replay_mismatches: i64,
    /// Misses that had to wait for another session's in-flight lock.
    lock_waits: i64,
    /// Hits found by the lookup made after taking the lock (also in `hits`;
    /// an audited answer is neither).
    lock_hits: i64,
    /// Misses that gave up waiting for a lock and solved without it.
    lock_timeouts: i64,
    audits: i64,
    audit_contradictions: i64,
    audit_instabilities: i64,
    audit_failures: i64,
}

struct Session<'a> {
    profile: &'a dyn SolverProfile,
    mode: Mode,
    policy: StorePolicy,
    lifetime: Lifetime,
    /// Fraction of query hits to audit (section 13).
    verify: f64,
    gc_every: u32,
    max_size: u64,
    lock_wait: Duration,
    /// While an in-flight lock is held, output for the client is kept here
    /// and written after the lock is released (section 10).
    deferred: Option<Vec<u8>>,
    cache_dir: Option<PathBuf>,
    /// Random per session: the sentinel, audit sampling, eviction chance.
    nonce: [u8; 32],
    real: &'a Path,
    args: &'a [OsString],
    live: Option<Live>,
    state: State,
    splitter: Splitter,
    /// `None` if the solver could not be identified: nothing is cached.
    key_ctx: Option<KeyContext>,
    stamp: Option<SolverStamp>,
    solver_info: Option<SolverInfo>,
    store: Option<SqliteStore>,
    recorder: Option<Recorder>,
    sentinel_cmd: Vec<u8>,
    sentinel_lines: Vec<Vec<u8>>,
    /// The resource counter given to the client (section 8).
    rlimit: u64,
    /// The solver printed an error: nothing more is stored (rule 6).
    no_store: bool,
    counts: Counts,
    touched: Vec<Key>,
    seq: u64,
    commands: u64,
    errored: bool,
    bypass: Option<String>,
    /// In bypass, forwarding command by command: the amount added to the
    /// solver's `rlimit-count` in statistics responses (rule 7).
    passthrough: Option<u64>,
    incomplete_input: bool,
}

/// Runs a proxy session on stdin/stdout and returns the exit code.
pub fn run(cfg: &Config, profile: &dyn SolverProfile, real: &Path, args: &[OsString]) -> i32 {
    let started = Instant::now();
    let open = |dir: &Path| match cfg.mode {
        // ro and enforce never write: no file, no migration (section 11).
        Mode::Ro | Mode::Enforce => SqliteStore::open_read_only(dir),
        _ => SqliteStore::open(dir),
    };
    let mut store = match &cfg.dir {
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
    let stamp = SolverStamp::of(real).ok();
    let solver_info = identify(real, stamp.as_ref(), &mut store, cfg.mode);
    let key_ctx = solver_info.as_ref().map(|info| KeyContext {
        solver_id: solver_id(&info.id, &cfg.solver_salt),
        argv: args.iter().map(|a| a.as_encoded_bytes().to_vec()).collect(),
    });

    let mut recorder = cfg.record.as_deref().and_then(Recorder::create);
    if let Some(r) = &mut recorder {
        r.set("argv", join_args(args));
        r.set("real_solver", real.display());
        if let Some(k) = &key_ctx {
            r.set("solver_id", blake3::Hash::from(k.solver_id).to_hex());
        }
    }

    let nonce = make_nonce();
    let sentinel = format!("z3cache-sentinel-{}", &blake3::Hash::from(nonce).to_hex()[..20]);
    let mut s = Session {
        profile,
        mode: cfg.mode,
        policy: cfg.store_policy,
        lifetime: cfg.lifetime,
        verify: if cfg.mode == Mode::Rw { cfg.verify } else { 0.0 },
        gc_every: cfg.gc_every,
        max_size: cfg.max_size,
        lock_wait: cfg.lock_wait,
        deferred: None,
        cache_dir: cfg.dir.clone(),
        nonce,
        real,
        args,
        live: None,
        state: State::new(),
        splitter: Splitter::new(),
        key_ctx,
        stamp,
        solver_info,
        store,
        recorder,
        sentinel_cmd: profile.sentinel_command(&sentinel),
        sentinel_lines: profile.sentinel_lines(&sentinel),
        rlimit: 0,
        no_store: false,
        counts: Counts::default(),
        touched: Vec::new(),
        seq: 0,
        commands: 0,
        errored: false,
        bypass: None,
        passthrough: None,
        incomplete_input: false,
    };
    let mut code = s.main_loop();
    // Z3 exits 1 after reporting an error. The last solver may not have seen
    // it (a fresh solver, or one started for bypass), so do not trust its
    // status alone.
    if code == 0 && s.errored {
        code = 1;
    }
    s.finish(code, started);
    code
}

/// Finds the solver's identity, from the store if it has been seen before.
fn identify(
    real: &Path,
    stamp: Option<&SolverStamp>,
    store: &mut Option<SqliteStore>,
    mode: Mode,
) -> Option<SolverInfo> {
    if let (Some(st), Some(stamp)) = (store.as_mut(), stamp) {
        match st.solver(stamp) {
            Ok(Some(info)) => return Some(info),
            Ok(None) => {}
            Err(e) => {
                log!("store disabled: {e}");
                *store = None;
            }
        }
    }
    let id = match hash_executable(real) {
        Ok(id) => id,
        Err(e) => {
            log!("cannot hash {}: {e}; caching disabled", real.display());
            return None;
        }
    };
    let info = SolverInfo {
        id,
        version_response: None,
    };
    if mode == Mode::Rw
        && let (Some(st), Some(stamp)) = (store.as_mut(), stamp)
            && let Err(e) = st.put_solver(stamp, &info) {
                log!("cannot record solver identity: {e}");
            }
    Some(info)
}

fn make_nonce() -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    h.update(&now.to_le_bytes());
    let local = 0u8;
    h.update(&(&local as *const u8 as usize).to_le_bytes());
    *h.finalize().as_bytes()
}

/// A number in [0, 1) derived from the session nonce and `what`.
fn sample(nonce: &[u8; 32], what: &[u8]) -> f64 {
    let mut h = blake3::Hasher::new();
    h.update(nonce);
    h.update(what);
    let n = u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().expect("8 bytes"));
    (n >> 11) as f64 / (1u64 << 53) as f64
}

fn is_definite(verdict: &str) -> bool {
    verdict == "sat" || verdict == "unsat"
}

fn first_line_verdict(response: &[u8]) -> &'static str {
    let first = response.split(|&b| b == b'\n').next().unwrap_or(b"");
    match first.trim_ascii() {
        b"sat" => "sat",
        b"unsat" => "unsat",
        b"unknown" => "unknown",
        _ => "other",
    }
}

impl Session<'_> {
    fn main_loop(&mut self) -> i32 {
        // Client input arrives on its own thread, so that a solver that dies
        // while the client is quiet ends the session (section 14). The queue
        // is bounded so that a solver that stops reading pushes back on the
        // client through the pipes instead of growing memory without limit;
        // dropping the receiver releases a blocked sender.
        let (tx, input) = mpsc::sync_channel(INPUT_QUEUE);
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            loop {
                match io::stdin().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Input::Data(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        log!("reading stdin failed: {e}");
                        break;
                    }
                }
            }
            let _ = tx.send(Input::End);
        });
        loop {
            let data = match input.recv_timeout(IDLE_CHECK) {
                Ok(Input::Data(d)) => d,
                Ok(Input::End) | Err(RecvTimeoutError::Disconnected) => return self.end_of_input(),
                Err(RecvTimeoutError::Timeout) => {
                    if self.live.as_mut().is_some_and(|l| l.solver.has_exited()) {
                        return self.solver_gone();
                    }
                    continue;
                }
            };
            if let Some(r) = &mut self.recorder {
                r.input(&data);
            }
            self.splitter.feed(&data);
            while let Some(next) = self.splitter.next_command() {
                let mut step = match next {
                    Ok(cmd) if self.passthrough.is_some() => self.forward(cmd),
                    Ok(cmd) => self.handle(cmd),
                    Err(e) => Step::Bypass {
                        reason: format!("parse error: {e}"),
                        trigger: None,
                        raw: true,
                    },
                };
                loop {
                    match step {
                        Step::Continue => break,
                        Step::Done(code) => return code,
                        Step::Bypass { reason, trigger, raw } => {
                            let framed = if self.passthrough.is_some() {
                                !raw
                            } else {
                                match self.start_bypass(reason, raw) {
                                    Ok(framed) => framed,
                                    Err(Stop(code)) => return code,
                                }
                            };
                            if !framed {
                                self.passthrough = None;
                                let mut pending = trigger.map(|c| c.raw).unwrap_or_default();
                                pending.extend(self.splitter.take_remaining());
                                return self.raw_pipe(pending, input);
                            }
                            step = match trigger {
                                Some(cmd) => self.forward(cmd),
                                None => Step::Continue,
                            };
                        }
                    }
                }
            }
        }
    }

    fn handle(&mut self, cmd: Command) -> Step {
        self.commands += 1;
        let result = match classify(&cmd, self.profile) {
            // A command or option the tables do not know may change how the
            // solver frames its output, so it is never forwarded framed.
            Class::Bypass(reason) => {
                return Step::Bypass {
                    reason,
                    trigger: Some(cmd),
                    raw: true,
                };
            }
            Class::Silent => {
                self.state.append(&cmd.canonical);
                Ok(())
            }
            Class::Push(n) => {
                self.state.push(n);
                Ok(())
            }
            Class::Pop(n) => {
                if self.state.pop(n).is_err() {
                    return Step::Bypass {
                        reason: "pop below the base frame".into(),
                        trigger: Some(cmd),
                        raw: false,
                    };
                }
                self.pop_live()
            }
            Class::SetOption { name, value } => {
                self.state.set_option(&name, &value, &cmd.canonical);
                Ok(())
            }
            Class::Local => self.local(&cmd),
            Class::Statistics => self.statistics(),
            Class::Query => self.lookup_or_solve(&cmd, true),
            Class::Read => self.lookup_or_solve(&cmd, false),
            Class::Exit => {
                let code = match &mut self.live {
                    Some(live) => {
                        let _ = live.solver.write(&cmd.raw);
                        self.shutdown()
                    }
                    None => 0,
                };
                return Step::Done(code);
            }
        };
        match result {
            // Section 7: after the solver reports an error, stop caching.
            Ok(()) if self.errored => Step::Bypass {
                reason: "solver reported an error".into(),
                trigger: None,
                raw: false,
            },
            Ok(()) => Step::Continue,
            Err(Stop(code)) => Step::Done(code),
        }
    }

    /// Discards solver frames the client has popped. `pop` does not undo
    /// options (assumption A1), and option commands the solver had not yet
    /// received are discarded with their frame, so the solver is then sent
    /// the client's command for every option whose value differs. This puts
    /// the solver's options where a real solver's would be at this point,
    /// before any later push, assertion or replayed query.
    fn pop_live(&mut self) -> Result<(), Stop> {
        let frames = self.state.frame_count();
        let Some(live) = &mut self.live else {
            return Ok(());
        };
        if live.sent.len() <= frames {
            return Ok(());
        }
        if self.lifetime == Lifetime::Fresh {
            // Section 7: the next miss gets a solver that has seen only the
            // path at that point.
            return self.discard_live();
        }
        let mut batch = Vec::new();
        while live.sent.len() > frames {
            live.sent.pop();
            batch.extend_from_slice(b"(pop)\n");
        }
        for (name, value, command) in self.state.options() {
            if live.options.get(name).map(Vec::as_slice) != Some(value) {
                batch.extend_from_slice(command);
                batch.push(b'\n');
                live.options.insert(name.to_vec(), value.to_vec());
            }
        }
        match live.solver.write(&batch) {
            Ok(()) => Ok(()),
            Err(_) => Err(Stop(self.solver_gone())),
        }
    }

    /// Ends the live solver, passing on anything it still had to say (such
    /// as an error for a command it received).
    fn discard_live(&mut self) -> Result<(), Stop> {
        let Some(mut live) = self.live.take() else {
            return Ok(());
        };
        live.solver.close_stdin();
        let rest = live.solver.read_rest().concat();
        let _ = live.solver.wait();
        self.respond(&rest)
    }

    fn local(&mut self, cmd: &Command) -> Result<(), Stop> {
        if cmd.head() == Some(b"echo") {
            let tok = cmd.args()[0].bytes();
            let out = self
                .profile
                .echo_response(tok)
                .expect("classified echo has a string argument");
            return self.respond_plain(&out);
        }
        // get-info :version
        if let Some(v) = self.solver_info.as_ref().and_then(|i| i.version_response.clone()) {
            return self.respond_plain(&v);
        }
        self.materialize()?;
        let mut sections = self.exchange(&[&cmd.canonical], Some(0))?;
        let response = sections.remove(0).concat();
        let clean = !self.has_error(&response);
        self.respond(&response)?;
        if clean {
            self.remember_version(response);
        }
        Ok(())
    }

    fn remember_version(&mut self, response: Vec<u8>) {
        let Some(info) = &mut self.solver_info else {
            return;
        };
        info.version_response = Some(response);
        if self.mode != Mode::Rw {
            return;
        }
        if let (Some(st), Some(stamp)) = (self.store.as_mut(), self.stamp.as_ref())
            && let Err(e) = st.put_solver(stamp, info) {
                log!("store disabled: {e}");
                self.store = None;
            }
    }

    fn statistics(&mut self) -> Result<(), Stop> {
        if self.live.is_none() {
            let out = self.profile.synthesize_statistics(self.rlimit);
            return self.respond(&out);
        }
        let stats = self.profile.statistics_command();
        let mut lines = self.exchange(&[stats], Some(0))?.remove(0);
        self.profile.rewrite_rlimit(&mut lines, self.rlimit);
        self.respond(&lines.concat())
    }

    fn lookup(&mut self, key: &Key) -> Option<Entry> {
        let store = self.store.as_mut()?;
        match store.get(key) {
            Ok(e) => e,
            Err(e) => {
                log!("store disabled: {e}");
                self.store = None;
                None
            }
        }
    }

    /// Looks a key up and applies the session's store policy to what it
    /// finds: under `unsat`, an entry stored by an `all` session for a
    /// failing query (or a read after one) is not used, so that failures are
    /// re-solved as rule 8 intends.
    fn accepted(&mut self, h: &Hashes, is_query: bool) -> Option<Entry> {
        let e = self.lookup(h.key.as_bytes())?;
        if e.is_query != is_query {
            return None;
        }
        if self.policy == StorePolicy::Unsat {
            let unsat = if is_query {
                first_line_verdict(&e.response) == "unsat"
            } else {
                self.state.last_query_verdict() == Some("unsat")
            };
            if !unsat {
                return None;
            }
        }
        Some(e)
    }

    fn wall_clock_in_effect(&self) -> bool {
        let option = self
            .state
            .options()
            .any(|(name, _, _)| self.profile.is_wall_clock_option(name));
        let argument = self.args.iter().any(|a| {
            let a = a.as_encoded_bytes();
            a.iter()
                .position(|&c| c == b'=')
                .is_some_and(|eq| self.profile.is_wall_clock_option(&a[..eq]))
        });
        option || argument
    }

    /// Whether a solver response may be stored (section 7). `verdict` is the
    /// query's own, or `-` for a read.
    fn storable(&self, clean: bool, is_query: bool, response: &[u8], verdict: &str) -> bool {
        if !clean || self.no_store || self.mode != Mode::Rw {
            return false;
        }
        let last = if is_query {
            verdict
        } else {
            match self.state.last_query_verdict() {
                Some(v) => v,
                None => return false,
            }
        };
        let accepted = match self.policy {
            StorePolicy::Unsat => last == "unsat",
            StorePolicy::All => {
                matches!(last, "sat" | "unsat" | "unknown")
                    && (last == "unsat" || !self.wall_clock_in_effect())
            }
        };
        // A query's response is exactly its verdict line.
        accepted && (!is_query || response == format!("{last}\n").as_bytes())
    }

    /// Writes a stored response. `count_hit` is false when a solver was
    /// consulted first (an audit), which rule 10 does not count as a hit.
    fn answer_from_entry(
        &mut self,
        cmd: &Command,
        hashes: Option<Hashes>,
        entry: Entry,
        start: Instant,
        count_hit: bool,
    ) -> Result<(), Stop> {
        // Entries never hold errors (rule 6).
        self.respond_plain(&entry.response)?;
        if count_hit {
            self.counts.hits += 1;
            self.counts.saved_us += i64::try_from(entry.solve_us).unwrap_or(i64::MAX);
        }
        if let Some(h) = hashes
            && entry.last_used_s < now_s() - 86_400
        {
            self.touched.push(*h.key.as_bytes());
        }
        let verdict = if entry.is_query {
            self.rlimit += entry.rlimit.unwrap_or(0);
            let v = first_line_verdict(&entry.response);
            self.state.append_query(&cmd.canonical, &entry.response, v);
            v
        } else {
            "-"
        };
        self.record(entry.is_query, hashes, verdict, start, if count_hit { "cache" } else { "audited" });
        Ok(())
    }

    /// Serves an accepted entry: through an audit if this hit is sampled
    /// (section 13), otherwise as a hit. Never called with a lock held.
    fn serve_entry(
        &mut self,
        cmd: &Command,
        h: Hashes,
        entry: Entry,
        start: Instant,
        after_lock: bool,
    ) -> Result<(), Stop> {
        if entry.is_query && self.verify > 0.0 && sample(&self.nonce, h.key.as_bytes()) < self.verify {
            return self.audited_hit(cmd, h, entry, start);
        }
        if after_lock {
            self.counts.lock_hits += 1;
        }
        self.answer_from_entry(cmd, Some(h), entry, start, true)
    }

    fn lookup_or_solve(&mut self, cmd: &Command, is_query: bool) -> Result<(), Stop> {
        let start = Instant::now();
        let hashes = self
            .key_ctx
            .as_ref()
            .map(|k| self.state.hashes(k, &cmd.canonical));
        if let Some(h) = hashes
            && let Some(entry) = self.accepted(&h, is_query)
        {
            return self.serve_entry(cmd, h, entry, start, false);
        }

        // Section 10: take the in-flight lock before starting any solver,
        // then look again: another session may have stored the answer while
        // this one waited, or just before it took the lock.
        let lock = match (&self.cache_dir, hashes) {
            (Some(dir), Some(h)) if self.mode == Mode::Rw && self.store.is_some() => {
                let busy = || log!("in-flight lock busy for {}: waiting", h.key.to_hex());
                match lock::acquire(dir, h.key.as_bytes(), self.lock_wait, busy) {
                    lock::Acquired::Lock(l) => Some(l),
                    lock::Acquired::TimedOut => {
                        self.counts.lock_timeouts += 1;
                        None
                    }
                    lock::Acquired::Unavailable => None,
                }
            }
            _ => None,
        };
        if let (Some(l), Some(h)) = (&lock, hashes) {
            if l.waited {
                self.counts.lock_waits += 1;
            }
            if let Some(entry) = self.accepted(&h, is_query) {
                drop(lock);
                return self.serve_entry(cmd, h, entry, start, true);
            }
        }

        // Nothing reaches the client while the lock is held, so a client
        // that stops reading cannot keep other sessions waiting: output is
        // kept, and written once the lock is released, on every path.
        if lock.is_some() {
            self.deferred = Some(Vec::new());
        }
        let solved = self.solve_and_store(cmd, is_query, hashes);
        let held = self.deferred.take().unwrap_or_default();
        drop(lock);
        let flushed = self.write_out(&held);
        let (response, verdict) = solved?;
        flushed?;
        self.respond(&response)?;
        self.record(is_query, hashes, verdict, start, "solver");
        Ok(())
    }

    /// The miss path: materialize, ask the solver, and store the answer if
    /// the policy allows. Returns the response for the caller to write.
    fn solve_and_store(
        &mut self,
        cmd: &Command,
        is_query: bool,
        hashes: Option<Hashes>,
    ) -> Result<(Vec<u8>, &'static str), Stop> {
        self.counts.misses += 1;
        self.materialize()?;
        let solve_start = Instant::now();
        let (response, delta) = if is_query {
            let stats = self.profile.statistics_command();
            let s = self.exchange(&[stats, &cmd.canonical, stats], Some(1))?;
            let before = self.profile.parse_rlimit(&s[0]);
            let after = self.profile.parse_rlimit(&s[2]);
            let delta = match (before, after) {
                (Some(b), Some(a)) => a.saturating_sub(b),
                _ => {
                    log!("no rlimit-count around a query; recording cost 0");
                    0
                }
            };
            let stats_error = self.has_error(&s[0].concat()) || self.has_error(&s[2].concat());
            if stats_error {
                self.no_store = true;
            }
            (s[1].concat(), Some(delta))
        } else {
            (self.exchange(&[&cmd.canonical], Some(0))?.remove(0).concat(), None)
        };
        let solve_us = solve_start.elapsed().as_micros() as u64;
        let clean = !self.has_error(&response);

        let verdict = if is_query {
            self.rlimit += delta.unwrap_or(0);
            let v = if clean { first_line_verdict(&response) } else { "error" };
            self.state.append_query(&cmd.canonical, &response, v);
            if let Some(live) = &mut self.live {
                // The solver has the query: keep the cursor at the end.
                if let Some(top) = live.sent.last_mut() {
                    *top += 1;
                }
            }
            v
        } else {
            "-"
        };

        if let (true, Some(h)) = (self.storable(clean, is_query, &response, verdict), hashes) {
            let entry = Entry {
                response: response.clone(),
                is_query,
                rlimit: delta,
                solve_us,
                last_used_s: 0,
            };
            if let Some(st) = self.store.as_mut() {
                match st.put(h.key.as_bytes(), &entry) {
                    Ok(()) => self.counts.stores += 1,
                    Err(e) => {
                        log!("store disabled: {e}");
                        self.store = None;
                    }
                }
            }
        }
        Ok((response, verdict))
    }

    /// A query hit chosen for audit (section 13): re-solve it in a separate
    /// fresh solver given the whole current path, before answering. The
    /// audit never changes the outcome of the session except to correct a
    /// contradicted entry.
    fn audited_hit(&mut self, cmd: &Command, h: Hashes, entry: Entry, start: Instant) -> Result<(), Stop> {
        self.counts.audits += 1;
        let cached = first_line_verdict(&entry.response);
        let (response, delta) = match self.audit(&cmd.canonical) {
            Ok(r) => r,
            Err(e) => {
                self.counts.audit_failures += 1;
                log!("audit failed ({e}); answering from the cache");
                return self.answer_from_entry(cmd, Some(h), entry, start, false);
            }
        };
        let now = first_line_verdict(&response);
        if now == cached {
            return self.answer_from_entry(cmd, Some(h), entry, start, false);
        }
        let key_hex = h.key.to_hex();
        if !(is_definite(cached) && is_definite(now)) {
            self.counts.audit_instabilities += 1;
            log!("audit: instability for {key_hex}: cached {cached}, now {now}; keeping the entry");
            return self.answer_from_entry(cmd, Some(h), entry, start, false);
        }
        self.counts.audit_contradictions += 1;
        log!("audit: CONTRADICTION for {key_hex}: cached {cached}, now {now}; entry deleted");
        if let Some(st) = self.store.as_mut()
            && let Err(e) = st.delete(h.key.as_bytes())
        {
            log!("store disabled: {e}");
            self.store = None;
        }
        // Answered by a solver: a miss (rule 10).
        self.counts.misses += 1;
        self.rlimit += delta;
        self.state.append_query(&cmd.canonical, &response, now);
        self.respond(&response)?;
        self.record(true, Some(h), now, start, "solver");
        Ok(())
    }

    /// Solves `query` in a new solver after the whole current path. Returns
    /// the response and its resource cost.
    fn audit(&mut self, query: &[u8]) -> Result<(Vec<u8>, u64), String> {
        let mut solver = Solver::spawn(self.real, self.args).map_err(|e| format!("cannot start: {e}"))?;
        let mut batch = Vec::new();
        for i in 0..self.state.frame_count() {
            if i > 0 {
                batch.extend_from_slice(b"(push)\n");
            }
            for r in self.state.replay(i) {
                batch.extend_from_slice(&r.command);
                batch.push(b'\n');
            }
        }
        let stats = self.profile.statistics_command();
        for cmd in [&b""[..], stats, query, stats] {
            batch.extend_from_slice(cmd);
            batch.push(b'\n');
            batch.extend_from_slice(&self.sentinel_cmd);
            batch.push(b'\n');
        }
        let result = (|| {
            solver
                .write(&batch)
                .and_then(|_| solver.flush())
                .map_err(|e| format!("cannot write: {e}"))?;
            let mut sections = Vec::with_capacity(4);
            for _ in 0..4 {
                let r = solver.read_until(&self.sentinel_lines);
                if r.eof {
                    return Err("the audit solver exited".to_string());
                }
                sections.push(r.lines.concat());
            }
            if sections.iter().any(|s| self.has_error(s)) {
                return Err("the audit solver reported an error".to_string());
            }
            let lines = |s: &[u8]| s.split_inclusive(|&b| b == b'\n').map(<[u8]>::to_vec).collect::<Vec<_>>();
            let before = self.profile.parse_rlimit(&lines(&sections[1])).unwrap_or(0);
            let after = self.profile.parse_rlimit(&lines(&sections[3])).unwrap_or(0);
            Ok((sections.swap_remove(2), after.saturating_sub(before)))
        })();
        solver.close_stdin();
        let _ = solver.wait();
        // The audit solver replaced the live one as the process to kill on a
        // signal.
        if let Some(live) = &self.live {
            live.solver.track();
        }
        result
    }

    fn record(&mut self, is_query: bool, hashes: Option<Hashes>, verdict: &str, start: Instant, served: &str) {
        self.seq += 1;
        if let Some(r) = &mut self.recorder {
            r.record(&Record {
                seq: self.seq,
                kind: if is_query { "query" } else { "read" },
                depth: self.state.depth(),
                hashes,
                verdict,
                us: start.elapsed().as_micros(),
                served,
            });
        }
    }

    fn has_error(&self, response: &[u8]) -> bool {
        response
            .split_inclusive(|&b| b == b'\n')
            .any(|l| self.profile.is_error_line(l))
    }

    /// Starts the solver if needed and sends it the part of the path it has
    /// not seen, then any option whose value in the solver differs from the
    /// client's. Queries answered from the cache are re-run; each re-run is
    /// framed by sentinels so that its answer can be compared with the one
    /// served (a difference is a replay mismatch), while all other output,
    /// such as errors, is passed on complete.
    fn materialize(&mut self) -> Result<(), Stop> {
        if self.mode == Mode::Enforce {
            let msg = b"(error \"z3cache: cache miss in enforce mode\")\n";
            let _ = self.respond_plain(msg);
            log!("enforce: cache miss");
            return Err(Stop(1));
        }
        if self.live.is_none() {
            match Solver::spawn(self.real, self.args) {
                Ok(solver) => {
                    self.counts.solver_starts += 1;
                    self.live = Some(Live {
                        solver,
                        sent: vec![0],
                        options: BTreeMap::new(),
                    });
                }
                Err(e) => {
                    eprintln!("z3cache: cannot start {}: {e}", self.real.display());
                    log!("cannot start {}: {e}", self.real.display());
                    return Err(Stop(127));
                }
            }
        }
        let live = self.live.as_mut().expect("just started");
        let mut batch = Vec::new();
        // One entry per sentinel: `None` for output to pass on, `Some` for a
        // re-run query's answer and what was served for it.
        let mut sections: Vec<Option<Vec<u8>>> = Vec::new();
        let sentinel = |batch: &mut Vec<u8>| {
            batch.extend_from_slice(&self.sentinel_cmd);
            batch.push(b'\n');
        };
        for i in 0..self.state.frame_count() {
            if i >= live.sent.len() {
                batch.extend_from_slice(b"(push)\n");
                live.sent.push(0);
            }
            let replay = self.state.replay(i);
            for r in &replay[live.sent[i]..] {
                if let Some(served) = &r.served {
                    sentinel(&mut batch);
                    sections.push(None);
                    batch.extend_from_slice(&r.command);
                    batch.push(b'\n');
                    sentinel(&mut batch);
                    sections.push(Some(served.clone()));
                } else {
                    batch.extend_from_slice(&r.command);
                    batch.push(b'\n');
                }
                if let Some((name, value)) = &r.option {
                    live.options.insert(name.clone(), value.clone());
                }
            }
            live.sent[i] = replay.len();
        }
        // Replaying the path in order, with options reconciled at each pop
        // (pop_live), leaves the solver with the client's options. A
        // difference here would be a bug; log it and correct it rather than
        // answer under the wrong settings.
        for (name, value, command) in self.state.options() {
            if live.options.get(name).map(Vec::as_slice) != Some(value) {
                log!("option drift: {}", String::from_utf8_lossy(command));
                batch.extend_from_slice(command);
                batch.push(b'\n');
                live.options.insert(name.to_vec(), value.to_vec());
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        sentinel(&mut batch);
        sections.push(None);
        if live.solver.write(&batch).and_then(|_| live.solver.flush()).is_err() {
            return Err(Stop(self.solver_gone()));
        }
        let mut replies = Vec::with_capacity(sections.len());
        let mut eof = false;
        for expected in sections {
            let resp = live.solver.read_until(&self.sentinel_lines);
            replies.push((expected, resp.lines));
            if resp.eof {
                eof = true;
                break;
            }
        }
        for (expected, lines) in replies {
            let got = lines.concat();
            match expected {
                // Errors are the solver's voice: the client sent the failing
                // command and a real solver would have reported it.
                None => self.respond(&got)?,
                Some(served) if got != served => {
                    self.counts.replay_mismatches += 1;
                    self.no_store = true;
                    log!(
                        "replay mismatch: served {:?}, solver now says {:?}",
                        String::from_utf8_lossy(&served),
                        String::from_utf8_lossy(&got)
                    );
                }
                Some(_) => {}
            }
        }
        if eof {
            return Err(Stop(self.solver_gone()));
        }
        Ok(())
    }

    /// Sends each command followed by the sentinel and returns each
    /// response. The solver must be live. `visible` is the index of the
    /// command the client asked for, if any; the others are the proxy's own,
    /// and their output never reaches the client.
    fn exchange(&mut self, cmds: &[&[u8]], visible: Option<usize>) -> Result<Vec<Vec<Vec<u8>>>, Stop> {
        let live = self.live.as_mut().expect("exchange needs a live solver");
        let mut batch = Vec::new();
        for c in cmds {
            batch.extend_from_slice(c);
            batch.push(b'\n');
            batch.extend_from_slice(&self.sentinel_cmd);
            batch.push(b'\n');
        }
        if live.solver.write(&batch).and_then(|_| live.solver.flush()).is_err() {
            return Err(Stop(self.solver_gone()));
        }
        let mut sections = Vec::with_capacity(cmds.len());
        for i in 0..cmds.len() {
            let resp = live.solver.read_until(&self.sentinel_lines);
            if resp.eof {
                // Pass on what the solver said to the client's command
                // before it went away.
                let said = match visible {
                    Some(v) if v < i => sections.swap_remove(v),
                    Some(v) if v == i => resp.lines,
                    _ => Vec::new(),
                };
                let _ = self.respond(&said.concat());
                return Err(Stop(self.solver_gone()));
            }
            sections.push(resp.lines);
        }
        Ok(sections)
    }

    /// Writes a solver's response to the client, noting any error it
    /// reports (rule 6, exit status). Stops the session if the client is
    /// gone.
    fn respond(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        self.emit(bytes, true)
    }

    /// Writes output that is not a solver's report: `echo` text, stored
    /// responses, the proxy's own messages. An `(error` line here is data.
    fn respond_plain(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        self.emit(bytes, false)
    }

    fn emit(&mut self, bytes: &[u8], scan: bool) -> Result<(), Stop> {
        if bytes.is_empty() {
            return Ok(());
        }
        if scan && self.has_error(bytes) {
            if !self.errored {
                let first = bytes.split(|&b| b == b'\n').find(|l| self.profile.is_error_line(l));
                log!("solver reported an error: {}", String::from_utf8_lossy(first.unwrap_or(b"")));
            }
            self.errored = true;
            self.no_store = true;
        }
        if let Some(held) = &mut self.deferred {
            held.extend_from_slice(bytes);
            return Ok(());
        }
        self.write_out(bytes)
    }

    fn write_out(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        if bytes.is_empty() {
            return Ok(());
        }
        if let Some(r) = &mut self.recorder {
            r.output(bytes);
        }
        let mut out = io::stdout().lock();
        if out.write_all(bytes).and_then(|_| out.flush()).is_err() {
            log!("client stopped reading; shutting down");
            return Err(Stop(self.shutdown()));
        }
        Ok(())
    }

    fn end_of_input(&mut self) -> i32 {
        self.incomplete_input = self.splitter.in_command();
        let rest = self.splitter.take_remaining();
        if self.passthrough.is_some() {
            if let Some(live) = &mut self.live {
                let _ = live.solver.write(&rest);
            }
        } else if self.incomplete_input {
            // A real solver would report the incomplete command.
            log!("input ended inside a command");
            if let Err(Stop(code)) = self.materialize() {
                return code;
            }
            if let Some(live) = &mut self.live {
                let _ = live.solver.write(&rest);
            }
        }
        if self.live.is_some() { self.shutdown() } else { 0 }
    }

    /// Closes the solver's input, passes on its remaining output, and waits.
    /// Returns 0 if no solver was started.
    fn shutdown(&mut self) -> i32 {
        let Some(mut live) = self.live.take() else {
            return 0;
        };
        live.solver.close_stdin();
        let rest = live.solver.read_rest().concat();
        let code = live.solver.wait();
        if !rest.is_empty() {
            let _ = self.respond(&rest);
        }
        code
    }

    fn solver_gone(&mut self) -> i32 {
        log!("solver exited unexpectedly");
        self.shutdown()
    }

    /// Enters bypass: materializes, and decides between framed and raw
    /// forwarding. Framed forwarding exists only to keep `rlimit-count` from
    /// going back (rule 7), so it is used only when the client has been given
    /// a counter ahead of the solver's and the trigger allows it. Returns
    /// true for framed.
    fn start_bypass(&mut self, reason: String, raw: bool) -> Result<bool, Stop> {
        log!("bypass: {reason}");
        if self.bypass.is_none() {
            self.bypass = Some(reason);
        }
        self.materialize()?;
        if raw || self.rlimit == 0 {
            return Ok(false);
        }
        let stats = self.profile.statistics_command();
        let s = self.exchange(&[stats], None)?;
        let real = self.profile.parse_rlimit(&s[0]).unwrap_or(0);
        let offset = self.rlimit.saturating_sub(real);
        if offset == 0 {
            return Ok(false);
        }
        self.passthrough = Some(offset);
        Ok(true)
    }

    /// Framed bypass, command by command: the client's bytes go to the
    /// solver unchanged, and each response is framed by a sentinel so that a
    /// statistics response can be recognised by the command that asked for
    /// it. Only those have their `rlimit-count` shifted. Only commands in the
    /// tables are forwarded this way; anything else switches to raw.
    fn forward(&mut self, cmd: Command) -> Step {
        self.commands += 1;
        let class = classify(&cmd, self.profile);
        if let Class::Bypass(reason) = class {
            return Step::Bypass {
                reason,
                trigger: Some(cmd),
                raw: true,
            };
        }
        let offset = self.passthrough.unwrap_or(0);
        let live = self.live.as_mut().expect("bypass has a live solver");
        match class {
            Class::Exit => {
                let _ = live.solver.write(&cmd.raw);
                return Step::Done(self.shutdown());
            }
            Class::Silent | Class::Push(_) | Class::Pop(_) | Class::SetOption { .. } => {
                return match live.solver.write(&cmd.raw) {
                    Ok(()) => Step::Continue,
                    Err(_) => Step::Done(self.solver_gone()),
                };
            }
            _ => {}
        }
        // Everything else may answer, including commands outside the tables.
        let sent = live
            .solver
            .write(&cmd.raw)
            .and_then(|_| live.solver.write(&self.sentinel_cmd))
            .and_then(|_| live.solver.flush());
        if sent.is_err() {
            return Step::Done(self.solver_gone());
        }
        let resp = live.solver.read_until(&self.sentinel_lines);
        let mut lines = resp.lines;
        if class == Class::Statistics && offset > 0 {
            // Output before the response itself (errors from earlier
            // commands) is left alone.
            if let Some(start) = lines.iter().position(|l| l.starts_with(b"(:")) {
                for l in &mut lines[start..] {
                    self.profile.shift_rlimit(l, offset);
                }
            }
        }
        // An echo's text is data, and here it shares a response with any
        // earlier errors, which the solver's exit status still reflects.
        let output = lines.concat();
        let written = if class == Class::Local {
            self.respond_plain(&output)
        } else {
            self.respond(&output)
        };
        if let Err(Stop(code)) = written {
            return Step::Done(code);
        }
        if resp.eof {
            return Step::Done(self.solver_gone());
        }
        Step::Continue
    }

    /// Raw bypass: copy bytes verbatim in both directions for the rest of
    /// the session, starting with `pending`. The proxy cannot tell a
    /// statistics response from other output here, so it changes nothing,
    /// and rule 7 is not guaranteed (DESIGN section 8). The session ends when
    /// the solver's output does, even if the client keeps stdin open.
    fn raw_pipe(&mut self, pending: Vec<u8>, input: Receiver<Input>) -> i32 {
        log!("raw bypass");
        let mut live = self.live.take().expect("materialized");
        let mut to_solver = live.solver.take_stdin();
        let mut in_rec = self.recorder.as_mut().and_then(Recorder::take_input);
        if let Some(w) = &mut to_solver
            && w.write_all(&pending).and_then(|_| w.flush()).is_err()
        {
            to_solver = None;
        }
        if let Some(mut w) = to_solver {
            std::thread::spawn(move || {
                while let Ok(Input::Data(data)) = input.recv() {
                    if let Some(r) = &mut in_rec {
                        let _ = r.write_all(&data).and_then(|_| r.flush());
                    }
                    if w.write_all(&data).and_then(|_| w.flush()).is_err() {
                        break;
                    }
                }
                // Dropping `w` closes the solver's stdin.
            });
        }
        // Raw output is not scanned for errors: it cannot be told apart from
        // echo text. The solver's own exit status reports them.
        let out_rec = self.recorder.as_mut().and_then(Recorder::take_output);
        if let Some(rx) = live.solver.take_lines() {
            pump(rx, out_rec);
        }
        live.solver.wait()
    }

    fn finish(&mut self, code: i32, started: Instant) {
        if let Some(mut r) = self.recorder.take() {
            r.set("exit_code", code);
            r.set("commands", self.commands);
            r.set("errored", u8::from(self.errored));
            r.set("bypass", self.bypass.as_deref().unwrap_or(""));
            r.set("incomplete_input", u8::from(self.incomplete_input));
            r.set("hits", self.counts.hits);
            r.set("misses", self.counts.misses);
            r.set("solver_starts", self.counts.solver_starts);
            r.set("wall_us", started.elapsed().as_micros());
            r.finish();
        }
        if self.mode != Mode::Rw {
            return;
        }
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let c = &self.counts;
        let mut counters: Vec<(String, i64)> = vec![
            ("sessions".into(), 1),
            ("hits".into(), c.hits),
            ("misses".into(), c.misses),
            ("stores".into(), c.stores),
            ("saved_us".into(), c.saved_us),
            ("solver_starts".into(), c.solver_starts),
            ("replay_mismatches".into(), c.replay_mismatches),
            ("errored_sessions".into(), i64::from(self.errored)),
            ("lock_waits".into(), c.lock_waits),
            ("lock_hits".into(), c.lock_hits),
            ("lock_timeouts".into(), c.lock_timeouts),
            ("audits".into(), c.audits),
            ("audit_contradictions".into(), c.audit_contradictions),
            ("audit_instabilities".into(), c.audit_instabilities),
            ("audit_failures".into(), c.audit_failures),
        ];
        if let Some(reason) = &self.bypass {
            counters.push(("bypassed_sessions".into(), 1));
            counters.push((format!("bypass: {reason}"), 1));
        }
        if let Err(e) = store.finish(&self.touched, &counters) {
            log!("cannot update counters: {e}");
        }
        // Section 9: one session in `gc_every` checks the size at exit and,
        // if the store is over its cap, starts `z3cache gc` in the
        // background, so that eviction never delays this session's exit.
        let chosen = self.gc_every > 0
            && u64::from_le_bytes(self.nonce[24..].try_into().expect("8 bytes")) % u64::from(self.gc_every) == 0;
        if chosen && store.db_bytes().unwrap_or(0) > self.max_size {
            match std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .arg0("z3cache")
                    .arg("gc")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
            }) {
                Ok(_) => log!("store over its cap: started z3cache gc"),
                Err(e) => log!("cannot start z3cache gc: {e}"),
            }
        }
    }
}

/// Copies solver output to the client, unchanged, until the solver closes
/// it.
fn pump(rx: Receiver<Vec<u8>>, mut rec: Option<BufWriter<File>>) {
    let mut client_ok = true;
    while let Ok(first) = rx.recv() {
        let mut batch = Vec::new();
        let mut next = Some(first);
        while let Some(line) = next {
            batch.extend_from_slice(&line);
            next = rx.try_recv().ok();
        }
        if let Some(r) = &mut rec {
            let _ = r.write_all(&batch);
        }
        if client_ok {
            let mut out = io::stdout().lock();
            client_ok = out.write_all(&batch).and_then(|_| out.flush()).is_ok();
        }
    }
    if let Some(r) = &mut rec {
        let _ = r.flush();
    }
}
