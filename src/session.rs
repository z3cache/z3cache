//! The proxy loop (DESIGN section 7).
//!
//! Silent, scope and option commands only update the tracked state. Queries
//! and reads are looked up by key: a hit writes the stored response; a miss
//! materializes the solver (starting it if needed and sending the unsent
//! part of the path) and asks it. A session whose queries all hit never
//! starts a solver. Commands outside the class tables switch to bypass:
//! materialize, then forward the client's commands one by one (or, where
//! responses cannot be told apart, copy bytes verbatim).

mod audit;
mod bypass;
mod channel;
mod client;
mod live;
mod store;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use crate::command::{Class, classify};
use crate::config::{Config, Mode, StorePolicy};
use crate::lock::{self, KeyLock};
use crate::log;
use crate::profile::SolverProfile;
use crate::record::{Record, Recorder, join_args};
use crate::sexp::{Command, Splitter};
use crate::solver::solver_id;
use crate::state::{Hashes, KeyContext, State, Verdict};
use crate::store::{Entry, SolverInfo, SolverStamp, Store, now_s, u64_to_sql};
use audit::sample;
use channel::{Framing, Gone, Output, Reply, Section};
use client::{ClientOut, Closed, Input, read_client_input};
use live::Live;
use store::{Counts, SessionStore, identify, session_counters};

/// The session must end with this exit code.
struct Stop(i32);

/// How often an idle session checks that its solver is still alive.
const IDLE_CHECK: Duration = Duration::from_millis(100);

/// How the session treats the client's commands (section 7).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Answering from the cache where it can.
    Caching,
    /// Framed bypass: forwarding command by command, adding `offset` to the
    /// solver's `rlimit-count` in statistics responses (rule 7).
    Framed { offset: u64 },
    /// Raw bypass: copying bytes verbatim to the end of the session.
    Raw,
}

/// What the main loop does after a command.
enum Next {
    Continue,
    /// Switch to raw bypass, sending these bytes to the solver first.
    Raw(Vec<u8>),
}

/// The outcome of taking the in-flight lock for a miss.
enum Locked {
    Held(KeyLock),
    /// Another session stored the answer; the lock is already released.
    Answered(Entry),
    /// No lock: none is used, or waiting for it timed out.
    Not,
}

/// One proxy session: the client's state, the live solver if any, and what
/// the session has done so far.
struct Session<'a> {
    profile: &'a dyn SolverProfile,
    cfg: &'a Config,
    /// Random per session: the sentinel, audit sampling, eviction chance.
    nonce: [u8; 32],
    real: &'a Path,
    args: &'a [OsString],
    /// An argument sets a wall-clock limit (section 7).
    wall_clock_arg: bool,
    framing: Framing<'a>,
    live: Option<Live<'a>>,
    out: ClientOut,
    phase: Phase,
    /// Why the session left caching, if it did.
    bypass_reason: Option<String>,
    state: State,
    splitter: Splitter,
    /// `None` if the solver could not be identified (nothing is cached), or
    /// if there is neither a store nor a recorder to use keys.
    key_ctx: Option<KeyContext>,
    stamp: Option<SolverStamp>,
    /// `None` if the solver could not be identified. Its `id` is zero if the
    /// binary was not hashed because nothing uses keys (see [`run`]).
    solver_info: Option<SolverInfo>,
    store: SessionStore,
    recorder: Option<Recorder>,
    /// The resource counter given to the client (section 8).
    rlimit: u64,
    /// The solver printed an error: nothing more is stored (rule 6).
    no_store: bool,
    /// The solver printed an error in reply to the client: the session
    /// exits 1.
    errored: bool,
    counts: Counts,
    touched: Vec<crate::store::Key>,
    seq: u64,
    commands: u64,
    incomplete_input: bool,
}

/// Runs a proxy session on stdin/stdout and returns the exit code.
pub fn run(cfg: &Config, profile: &dyn SolverProfile, real: &Path, args: &[OsString]) -> i32 {
    let started = Instant::now();
    let mut store = SessionStore::open(cfg);
    let mut recorder = cfg.record.as_deref().and_then(Recorder::create);
    let stamp = SolverStamp::of(real).ok();
    // Keys are used only to look up, store and record. Without a store or a
    // recorder the binary is not hashed; the identity then only remembers
    // the version response for this session, and its `id` is never read.
    let (solver_info, key_ctx) = if store.is_open() || recorder.is_some() {
        let info = identify(real, stamp.as_ref(), &mut store);
        let key_ctx = info.as_ref().map(|info| KeyContext {
            solver_id: solver_id(&info.id, &cfg.solver_salt),
            argv: args.iter().map(|a| a.as_encoded_bytes().to_vec()).collect(),
        });
        (info, key_ctx)
    } else {
        let unhashed = SolverInfo { id: [0; 32], version_response: None };
        (Some(unhashed), None)
    };

    if let Some(r) = &mut recorder {
        r.set("argv", join_args(args));
        r.set("real_solver", real.display());
        if let Some(k) = &key_ctx {
            r.set("solver_id", blake3::Hash::from(k.solver_id).to_hex());
        }
    }

    let nonce = make_nonce();
    let sentinel = format!("z3cache-sentinel-{}", &blake3::Hash::from(nonce).to_hex()[..20]);
    let wall_clock_arg = args.iter().any(|a| {
        let a = a.as_encoded_bytes();
        a.iter().position(|&c| c == b'=').is_some_and(|eq| profile.is_wall_clock_option(&a[..eq]))
    });
    let mut s = Session {
        profile,
        cfg,
        nonce,
        real,
        args,
        wall_clock_arg,
        framing: Framing::new(profile, &sentinel),
        live: None,
        out: ClientOut::new(recorder.as_mut().and_then(Recorder::take_output)),
        phase: Phase::Caching,
        bypass_reason: None,
        state: State::new(),
        splitter: Splitter::new(),
        key_ctx,
        stamp,
        solver_info,
        store,
        recorder,
        rlimit: 0,
        no_store: false,
        errored: false,
        counts: Counts::default(),
        touched: Vec::new(),
        seq: 0,
        commands: 0,
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

fn make_nonce() -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&crate::since_epoch().as_nanos().to_le_bytes());
    let local = 0u8;
    h.update(&(&local as *const u8 as usize).to_le_bytes());
    *h.finalize().as_bytes()
}

impl<'a> Session<'a> {
    fn main_loop(&mut self) -> i32 {
        let input = read_client_input();
        loop {
            let data = match input.recv_timeout(IDLE_CHECK) {
                Ok(Input::Data(d)) => d,
                Ok(Input::End) | Err(RecvTimeoutError::Disconnected) => return self.end_of_input(),
                Err(RecvTimeoutError::Timeout) => {
                    if self.live.as_mut().is_some_and(|l| l.channel.has_exited()) {
                        return self.gone(Gone(Vec::new())).0;
                    }
                    continue;
                }
            };
            if let Some(r) = &mut self.recorder {
                r.input(&data);
            }
            self.splitter.feed(&data);
            while let Some(next) = self.splitter.next_command() {
                let step = match next {
                    Ok(cmd) => self.dispatch(cmd),
                    Err(e) => self.raw_bypass(format!("parse error: {e}"), None),
                };
                match step {
                    Ok(Next::Continue) => {}
                    Ok(Next::Raw(pending)) => return self.raw_pipe(pending, input),
                    Err(Stop(code)) => return code,
                }
            }
        }
    }

    /// Classifies a command and handles it as the phase requires.
    fn dispatch(&mut self, cmd: Command) -> Result<Next, Stop> {
        self.commands += 1;
        match (self.phase, classify(&cmd, self.profile)) {
            // A command or option the tables do not know may change how the
            // solver frames its output, so it is never forwarded framed.
            (_, Class::Bypass(reason)) => self.raw_bypass(reason, Some(cmd)),
            (_, Class::Exit) => Err(Stop(self.exit(&cmd))),
            (Phase::Caching, class) => self.cache(cmd, class),
            (Phase::Framed { offset }, class) => self.forward(&cmd, &class, offset),
            (Phase::Raw, _) => unreachable!("raw bypass splits no commands"),
        }
    }

    /// Caching: updates the path, or answers from the store or a solver.
    fn cache(&mut self, cmd: Command, class: Class) -> Result<Next, Stop> {
        match class {
            Class::Silent => self.state.append(cmd.canonical),
            Class::Push(n) => self.state.push(n),
            Class::Pop(n) => {
                if self.state.pop(n).is_err() {
                    return self.framed_bypass("pop below the base frame", Some((cmd, Class::Pop(n))));
                }
                self.pop_live()?;
            }
            Class::SetOption { name, value } => self.state.set_option(&name, &value, &cmd.canonical),
            Class::Local => self.local(&cmd)?,
            Class::Statistics => self.statistics()?,
            Class::Query => self.lookup_or_solve(&cmd, true)?,
            Class::Read => self.lookup_or_solve(&cmd, false)?,
            Class::Bypass(_) | Class::Exit => unreachable!("handled by dispatch"),
        }
        if self.errored {
            // Section 7: after the solver reports an error, stop caching.
            return self.framed_bypass("solver reported an error", None);
        }
        Ok(Next::Continue)
    }

    /// `exit`: passes it to the solver, if there is one, and shuts down.
    fn exit(&mut self, cmd: &Command) -> i32 {
        if let Some(live) = &mut self.live {
            let _ = live.channel.write(&cmd.raw);
        }
        self.shutdown()
    }

    /// `echo` and `get-info :version`.
    fn local(&mut self, cmd: &Command) -> Result<(), Stop> {
        if cmd.head() == Some(b"echo") {
            let tok = cmd.args()[0].bytes();
            let out = self.profile.echo_response(tok).expect("classified echo has a string argument");
            return self.write_client(&out);
        }
        if let Some(v) = self.solver_info.as_ref().and_then(|i| i.version_response.clone()) {
            return self.write_client(&v);
        }
        self.materialize()?;
        let reply = self.ask(&cmd.canonical, Output::Response)?;
        self.write_client(&reply.text())?;
        if reply.error.is_none() {
            self.remember_version(reply.text());
        }
        Ok(())
    }

    fn remember_version(&mut self, response: Vec<u8>) {
        let Some(info) = &mut self.solver_info else {
            return;
        };
        info.version_response = Some(response);
        if let Some(stamp) = &self.stamp {
            self.store.put_solver(stamp, info);
        }
    }

    /// Section 8: synthesized with no solver running, otherwise the solver's
    /// response with the client's counter in it.
    fn statistics(&mut self) -> Result<(), Stop> {
        if self.live.is_none() {
            let out = self.profile.synthesize_statistics(self.rlimit);
            return self.write_client(&out);
        }
        let v = self.rlimit;
        let reply = self.ask(self.profile.statistics_command(), Output::Statistics(Box::new(move |_| v)))?;
        self.write_client(&reply.text())
    }

    /// The store policy (section 7, rule 8), for both storing and serving:
    /// whether an answer may come from or go to the store, given the verdict
    /// of the query it is (or, for a read, of the latest query on the path).
    /// Under `unsat`, this keeps an entry stored by an `all` session for a
    /// failing query (or a read after one) from being used, so that failures
    /// are re-solved as rule 8 intends.
    fn policy_allows(&self, verdict: Option<Verdict>) -> bool {
        match (self.cfg.store_policy, verdict) {
            (_, Some(Verdict::Unsat)) => true,
            (StorePolicy::All, Some(Verdict::Sat | Verdict::Unknown)) => !self.wall_clock_in_effect(),
            _ => false,
        }
    }

    /// Looks a key up and applies the store policy to what it finds.
    fn accepted(&mut self, h: &Hashes, is_query: bool) -> Option<Entry> {
        let e = self.store.get(h.key.as_bytes())?;
        if e.is_query != is_query {
            return None;
        }
        let verdict = if is_query { Some(Verdict::of(&e.response)) } else { self.state.last_query_verdict() };
        self.policy_allows(verdict).then_some(e)
    }

    fn wall_clock_in_effect(&self) -> bool {
        self.wall_clock_arg || self.state.options().any(|(name, _, _)| self.profile.is_wall_clock_option(name))
    }

    /// Whether a solver response may be stored (section 7). `verdict` is the
    /// query's own, or `None` for a read. The store itself skips writes
    /// outside `rw` mode.
    fn storable(&self, clean: bool, is_query: bool, response: &[u8], verdict: Option<Verdict>) -> bool {
        if !clean || self.no_store {
            return false;
        }
        let last = if is_query { verdict } else { self.state.last_query_verdict() };
        // A query's response is exactly its verdict line.
        self.policy_allows(last) && (!is_query || last.is_some_and(|v| response == format!("{v}\n").as_bytes()))
    }

    fn verify(&self) -> f64 {
        if self.cfg.mode == Mode::Rw { self.cfg.verify } else { 0.0 }
    }

    /// Writes a stored response. `count_hit` is false when a solver was
    /// consulted first (an audit), which rule 10 does not count as a hit.
    fn answer_from_entry(
        &mut self,
        cmd: &Command,
        h: Hashes,
        entry: Entry,
        start: Instant,
        count_hit: bool,
    ) -> Result<(), Stop> {
        // Entries never hold errors (rule 6).
        self.write_client(&entry.response)?;
        if count_hit {
            self.counts.hits += 1;
            self.counts.saved_us += u64_to_sql(entry.solve_us);
        }
        if entry.last_used_s < now_s() - 86_400 {
            self.touched.push(*h.key.as_bytes());
        }
        let verdict = if entry.is_query {
            self.rlimit += entry.rlimit.unwrap_or(0);
            let v = Verdict::of(&entry.response);
            self.state.append_query(cmd.canonical.as_slice(), entry.response, v);
            Some(v)
        } else {
            None
        };
        self.record(entry.is_query, Some(h), verdict, start, if count_hit { "cache" } else { "audited" });
        Ok(())
    }

    /// Serves an accepted entry: through an audit if this hit is sampled
    /// (section 13), otherwise as a hit.
    fn serve_entry(
        &mut self,
        cmd: &Command,
        h: Hashes,
        entry: Entry,
        start: Instant,
        after_lock: bool,
    ) -> Result<(), Stop> {
        let verify = self.verify();
        if entry.is_query && verify > 0.0 && sample(&self.nonce, h.key.as_bytes()) < verify {
            return self.audited_hit(cmd, h, entry, start);
        }
        if after_lock {
            self.counts.lock_hits += 1;
        }
        self.answer_from_entry(cmd, h, entry, start, true)
    }

    fn lookup_or_solve(&mut self, cmd: &Command, is_query: bool) -> Result<(), Stop> {
        let start = Instant::now();
        let hashes = self.key_ctx.as_ref().map(|k| self.state.hashes(k, &cmd.canonical));
        if let Some(h) = hashes {
            if let Some(entry) = self.accepted(&h, is_query) {
                return self.serve_entry(cmd, h, entry, start, false);
            }
            match self.lock_key(&h, is_query) {
                Locked::Answered(entry) => return self.serve_entry(cmd, h, entry, start, true),
                Locked::Held(lock) => self.out.hold(lock),
                Locked::Not => {}
            }
        }
        let solved = self.solve_and_store(cmd, is_query, hashes);
        let released = self.release_client();
        let (response, verdict) = solved?;
        released?;
        self.write_client(&response)?;
        self.record(is_query, hashes, verdict, start, "solver");
        Ok(())
    }

    /// Section 10: takes the in-flight lock before starting any solver, then
    /// looks again: another session may have stored the answer while this
    /// one waited, or just before it took the lock.
    fn lock_key(&mut self, h: &Hashes, is_query: bool) -> Locked {
        let cfg = self.cfg;
        let Some(dir) = cfg.dir.as_deref().filter(|_| self.store.can_write()) else {
            return Locked::Not;
        };
        let busy = || log!("in-flight lock busy for {}: waiting", h.key.to_hex());
        match lock::acquire(dir, h.key.as_bytes(), cfg.lock_wait, busy) {
            lock::Acquired::Lock(l) => {
                if l.waited {
                    self.counts.lock_waits += 1;
                }
                match self.accepted(h, is_query) {
                    Some(entry) => Locked::Answered(entry),
                    None => Locked::Held(l),
                }
            }
            lock::Acquired::TimedOut => {
                self.counts.lock_timeouts += 1;
                Locked::Not
            }
            lock::Acquired::Unavailable => Locked::Not,
        }
    }

    /// The miss path: materialize, ask the solver, and store the answer if
    /// the policy allows. Returns the response for the caller to write, and
    /// the query's verdict.
    fn solve_and_store(
        &mut self,
        cmd: &Command,
        is_query: bool,
        hashes: Option<Hashes>,
    ) -> Result<(Vec<u8>, Option<Verdict>), Stop> {
        self.counts.misses += 1;
        self.materialize()?;
        let solve_start = Instant::now();
        let (reply, delta) = if is_query {
            let (reply, delta) = self.solve(&cmd.canonical)?;
            (reply, Some(delta))
        } else {
            (self.ask(&cmd.canonical, Output::Response)?, None)
        };
        let solve_us = solve_start.elapsed().as_micros() as u64;
        let clean = reply.error.is_none();
        let response = reply.text();

        let verdict = if is_query {
            self.rlimit += delta.unwrap_or(0);
            let v = if clean { Verdict::of(&response) } else { Verdict::Error };
            self.state.append_query(cmd.canonical.as_slice(), response.as_slice(), v);
            if let Some(live) = &mut self.live {
                // The solver has the query: keep the cursor at the end.
                if let Some(top) = live.sent.last_mut() {
                    *top += 1;
                }
            }
            Some(v)
        } else {
            None
        };

        if self.storable(clean, is_query, &response, verdict)
            && let Some(h) = hashes
        {
            let entry = Entry { response: response.clone(), is_query, rlimit: delta, solve_us, last_used_s: 0 };
            if self.store.put(h.key.as_bytes(), &entry) {
                self.counts.stores += 1;
            }
        }
        Ok((response, verdict))
    }

    /// Asks the live solver a query between two statistics reads. Returns
    /// its reply and resource cost.
    fn solve(&mut self, query: &[u8]) -> Result<(Reply, u64), Stop> {
        let stats = self.profile.statistics_command();
        let mut replies = self.exchange(vec![
            Section::command(stats, Output::Own),
            Section::command(query, Output::Response),
            Section::command(stats, Output::Own),
        ])?;
        let before = self.profile.parse_rlimit(&replies[0].lines);
        let after = self.profile.parse_rlimit(&replies[2].lines);
        let delta = match (before, after) {
            (Some(b), Some(a)) => a.saturating_sub(b),
            _ => {
                log!("no rlimit-count around a query; recording cost 0");
                0
            }
        };
        Ok((replies.swap_remove(1), delta))
    }

    fn record(
        &mut self,
        is_query: bool,
        hashes: Option<Hashes>,
        verdict: Option<Verdict>,
        start: Instant,
        served: &str,
    ) {
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

    /// Writes to the client through [`ClientOut`]. Ends the session if the
    /// client stopped reading.
    fn write_client(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        self.out.write(bytes).map_err(|Closed| self.client_gone())
    }

    /// Releases the in-flight lock, if one is held, and writes what was kept
    /// while it was.
    fn release_client(&mut self) -> Result<(), Stop> {
        self.out.release().map_err(|Closed| self.client_gone())
    }

    fn client_gone(&mut self) -> Stop {
        log!("client stopped reading; shutting down");
        Stop(self.shutdown())
    }

    fn end_of_input(&mut self) -> i32 {
        self.incomplete_input = self.splitter.in_command();
        let rest = self.splitter.take_remaining();
        let forwarding = matches!(self.phase, Phase::Framed { .. });
        if !forwarding && self.incomplete_input {
            // A real solver would report the incomplete command.
            log!("input ended inside a command");
            if let Err(Stop(code)) = self.materialize() {
                return code;
            }
        }
        if (forwarding || self.incomplete_input)
            && let Some(live) = &mut self.live
        {
            let _ = live.channel.write(&rest);
        }
        self.shutdown()
    }

    fn finish(&mut self, code: i32, started: Instant) {
        self.out.finish();
        if let Some(mut r) = self.recorder.take() {
            r.set("exit_code", code);
            r.set("commands", self.commands);
            r.set("errored", u8::from(self.errored));
            r.set("bypass", self.bypass_reason.as_deref().unwrap_or(""));
            r.set("incomplete_input", u8::from(self.incomplete_input));
            r.set("hits", self.counts.hits);
            r.set("misses", self.counts.misses);
            r.set("solver_starts", self.counts.solver_starts);
            r.set("wall_us", started.elapsed().as_micros());
            r.finish();
        }
        let Some(store) = self.store.writing() else {
            return;
        };
        let counters = session_counters(&self.counts, self.errored, self.bypass_reason.as_deref());
        if let Err(e) = store.finish(&self.touched, &counters) {
            log!("cannot update counters: {e}");
        }
        self.maybe_start_gc();
    }

    /// Section 9: one session in `gc_every` checks the size at exit and, if
    /// the store is over its cap, starts `z3cache gc` in the background, so
    /// that eviction never delays this session's exit.
    fn maybe_start_gc(&mut self) {
        let gc_every = self.cfg.gc_every;
        let draw = u64::from_le_bytes(self.nonce[24..].try_into().expect("8 bytes"));
        let chosen = gc_every > 0 && draw % u64::from(gc_every) == 0;
        let Some(store) = self.store.writing() else {
            return;
        };
        if !chosen || store.db_bytes().unwrap_or(0) <= self.cfg.max_size {
            return;
        }
        let started = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg0("z3cache")
                .arg("gc")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        });
        match started {
            Ok(_) => log!("store over its cap: started z3cache gc"),
            Err(e) => log!("cannot start z3cache gc: {e}"),
        }
    }
}
