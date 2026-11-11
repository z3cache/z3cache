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
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::command::{Class, classify};
use crate::config::{Config, Lifetime, Mode, StorePolicy};
use crate::lock;
use crate::log;
use crate::profile::SolverProfile;
use crate::record::{Record, Recorder, join_args};
use crate::sexp::{Command, Splitter};
use crate::solver::{Solver, hash_executable, solver_id};
use crate::state::{Hashes, KeyContext, Replay, State, Verdict};
use crate::store::{self, Entry, Key, SolverInfo, SolverStamp, SqliteStore, Store, now_s, u64_to_sql};

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

/// Chunks of client input (at most 64 KiB each) queued ahead of the session.
const INPUT_QUEUE: usize = 4;

/// What the client-input thread hands to the session.
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

impl Live {
    /// Adds to `batch` the client's command for every option whose value in
    /// the solver differs, and notes the new value as sent. Returns whether
    /// it added any.
    fn sync_options(&mut self, state: &State, batch: &mut Vec<u8>) -> bool {
        let mut added = false;
        for (name, value, command) in state.options() {
            if self.options.get(name).map(Vec::as_slice) != Some(value) {
                push_line(batch, command);
                self.options.insert(name.to_vec(), value.to_vec());
                added = true;
            }
        }
        added
    }
}

/// Adds a command and its newline to a batch for the solver.
fn push_line(batch: &mut Vec<u8>, cmd: &[u8]) {
    batch.extend_from_slice(cmd);
    batch.push(b'\n');
}

/// Adds a command followed by the sentinel command.
fn push_framed(batch: &mut Vec<u8>, cmd: &[u8], sentinel: &[u8]) {
    push_line(batch, cmd);
    push_line(batch, sentinel);
}

/// Adds to `batch` the part of the path not yet sent, from the cursor `sent`
/// (as in [`Live::sent`]), and advances the cursor: `(push)` for each frame
/// not yet opened, and each unsent replay entry through `entry`.
fn push_unsent(
    state: &State,
    sent: &mut Vec<usize>,
    batch: &mut Vec<u8>,
    mut entry: impl FnMut(&mut Vec<u8>, &Replay),
) {
    for i in 0..state.frame_count() {
        if i >= sent.len() {
            push_line(batch, b"(push)");
            sent.push(0);
        }
        let replay = state.replay(i);
        for r in &replay[sent[i]..] {
            entry(batch, r);
        }
        sent[i] = replay.len();
    }
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

impl Counts {
    /// Each count with the counter name it is added to.
    fn named(&self) -> [(&'static str, i64); 13] {
        [
            ("hits", self.hits),
            ("misses", self.misses),
            ("stores", self.stores),
            ("saved_us", self.saved_us),
            ("solver_starts", self.solver_starts),
            ("replay_mismatches", self.replay_mismatches),
            ("lock_waits", self.lock_waits),
            ("lock_hits", self.lock_hits),
            ("lock_timeouts", self.lock_timeouts),
            ("audits", self.audits),
            ("audit_contradictions", self.audit_contradictions),
            ("audit_instabilities", self.audit_instabilities),
            ("audit_failures", self.audit_failures),
        ]
    }
}

/// What a session adds to the store's counters at exit.
fn session_counters(counts: &Counts, errored: bool, bypass: Option<&str>) -> Vec<(String, i64)> {
    let mut counters = vec![
        ("sessions".to_string(), 1),
        ("errored_sessions".to_string(), i64::from(errored)),
    ];
    counters.extend(counts.named().map(|(name, n)| (name.to_string(), n)));
    if let Some(reason) = bypass {
        counters.push(("bypassed_sessions".into(), 1));
        counters.push((format!("bypass: {reason}"), 1));
    }
    counters
}

/// The store as a session uses it. Any error disables it for the rest of the
/// session (rule 2), and writes are skipped unless the mode is `rw`.
struct SessionStore {
    store: Option<SqliteStore>,
    writable: bool,
}

impl SessionStore {
    fn open(cfg: &Config) -> SessionStore {
        let open = |dir: &Path| match cfg.mode {
            // ro and enforce never write: no file, no migration (section 11).
            Mode::Ro | Mode::Enforce => SqliteStore::open_read_only(dir),
            _ => SqliteStore::open(dir),
        };
        let store = match &cfg.dir {
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
        SessionStore {
            store,
            writable: cfg.mode == Mode::Rw,
        }
    }

    fn is_open(&self) -> bool {
        self.store.is_some()
    }

    fn can_write(&self) -> bool {
        self.writable && self.is_open()
    }

    /// The store, if it is open and may be written. Errors from it are the
    /// caller's to handle.
    fn writing(&mut self) -> Option<&mut SqliteStore> {
        self.store.as_mut().filter(|_| self.writable)
    }

    /// Runs `op` on the store if it is open; an error disables the store.
    fn with<T>(&mut self, op: impl FnOnce(&mut SqliteStore) -> store::Result<T>) -> Option<T> {
        match op(self.store.as_mut()?) {
            Ok(v) => Some(v),
            Err(e) => {
                log!("store disabled: {e}");
                self.store = None;
                None
            }
        }
    }

    /// Like [`SessionStore::with`], for a write: skipped unless writable.
    fn write<T>(&mut self, op: impl FnOnce(&mut SqliteStore) -> store::Result<T>) -> Option<T> {
        if !self.writable {
            return None;
        }
        self.with(op)
    }

    fn get(&mut self, key: &Key) -> Option<Entry> {
        self.with(|s| s.get(key)).flatten()
    }

    /// Returns whether the entry was written.
    fn put(&mut self, key: &Key, entry: &Entry) -> bool {
        self.write(|s| s.put(key, entry)).is_some()
    }

    fn delete(&mut self, key: &Key) {
        self.write(|s| s.delete(key));
    }

    fn solver(&mut self, stamp: &SolverStamp) -> Option<SolverInfo> {
        self.with(|s| s.solver(stamp)).flatten()
    }

    fn put_solver(&mut self, stamp: &SolverStamp, info: &SolverInfo) {
        self.write(|s| s.put_solver(stamp, info));
    }
}

struct Session<'a> {
    profile: &'a dyn SolverProfile,
    cfg: &'a Config,
    /// While an in-flight lock is held, output for the client is kept here
    /// and written after the lock is released (section 10).
    deferred: Option<Vec<u8>>,
    /// Random per session: the sentinel, audit sampling, eviction chance.
    nonce: [u8; 32],
    real: &'a Path,
    args: &'a [OsString],
    /// An argument sets a wall-clock limit (section 7).
    wall_clock_arg: bool,
    live: Option<Live>,
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
        let unhashed = SolverInfo {
            id: [0; 32],
            version_response: None,
        };
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
        a.iter()
            .position(|&c| c == b'=')
            .is_some_and(|eq| profile.is_wall_clock_option(&a[..eq]))
    });
    let mut s = Session {
        profile,
        cfg,
        deferred: None,
        nonce,
        real,
        args,
        wall_clock_arg,
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
fn identify(real: &Path, stamp: Option<&SolverStamp>, store: &mut SessionStore) -> Option<SolverInfo> {
    if let Some(info) = stamp.and_then(|stamp| store.solver(stamp)) {
        return Some(info);
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
    // Failing to record the identity only costs hashing it again next time:
    // the store stays in use.
    if let (Some(st), Some(stamp)) = (store.writing(), stamp)
        && let Err(e) = st.put_solver(stamp, &info)
    {
        log!("cannot record solver identity: {e}");
    }
    Some(info)
}

fn make_nonce() -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&crate::since_epoch().as_nanos().to_le_bytes());
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

/// Reads client input on its own thread, so that a solver that dies while
/// the client is quiet ends the session (section 14). The queue is bounded
/// so that a solver that stops reading pushes back on the client through the
/// pipes instead of growing memory without limit; dropping the receiver
/// releases a blocked sender.
fn read_client_input() -> Receiver<Input> {
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
    input
}

impl Session<'_> {
    fn main_loop(&mut self) -> i32 {
        let input = read_client_input();
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
                let step = match next {
                    Ok(cmd) if self.passthrough.is_some() => self.forward(cmd),
                    Ok(cmd) => self.handle(cmd),
                    Err(e) => Step::Bypass {
                        reason: format!("parse error: {e}"),
                        trigger: None,
                        raw: true,
                    },
                };
                match self.follow(step) {
                    Ok(None) => {}
                    Ok(Some(pending)) => return self.raw_pipe(pending, input),
                    Err(Stop(code)) => return code,
                }
            }
        }
    }

    /// Carries out what a step asks, through any switch to bypass. Returns
    /// the bytes to start a raw pipe with if it comes to that.
    fn follow(&mut self, mut step: Step) -> Result<Option<Vec<u8>>, Stop> {
        loop {
            let (reason, trigger, raw) = match step {
                Step::Continue => return Ok(None),
                Step::Done(code) => return Err(Stop(code)),
                Step::Bypass { reason, trigger, raw } => (reason, trigger, raw),
            };
            let framed = if self.passthrough.is_some() {
                !raw
            } else {
                self.start_bypass(reason, raw)?
            };
            if !framed {
                self.passthrough = None;
                let mut pending = trigger.map(|c| c.raw).unwrap_or_default();
                pending.extend(self.splitter.take_remaining());
                return Ok(Some(pending));
            }
            step = match trigger {
                Some(cmd) => self.forward(cmd),
                None => Step::Continue,
            };
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
                self.state.append(cmd.canonical);
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
            Class::Exit => return Step::Done(self.exit(&cmd)),
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

    /// `exit`: passes it to the solver, if there is one, and shuts down.
    fn exit(&mut self, cmd: &Command) -> i32 {
        if let Some(live) = &mut self.live {
            let _ = live.solver.write(&cmd.raw);
        }
        self.shutdown()
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
        if self.cfg.lifetime == Lifetime::Fresh {
            // Section 7: the next miss gets a solver that has seen only the
            // path at that point.
            return self.discard_live();
        }
        let mut batch = Vec::new();
        while live.sent.len() > frames {
            live.sent.pop();
            push_line(&mut batch, b"(pop)");
        }
        live.sync_options(&self.state, &mut batch);
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
        let (rest, _) = live.solver.finish();
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
        let response = self.ask(&cmd.canonical, true)?.concat();
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
        if let Some(stamp) = &self.stamp {
            self.store.put_solver(stamp, info);
        }
    }

    fn statistics(&mut self) -> Result<(), Stop> {
        if self.live.is_none() {
            let out = self.profile.synthesize_statistics(self.rlimit);
            return self.respond(&out);
        }
        let mut lines = self.ask(self.profile.statistics_command(), true)?;
        self.profile.rewrite_rlimit(&mut lines, self.rlimit);
        self.respond(&lines.concat())
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
        let verdict = if is_query {
            Some(Verdict::of(&e.response))
        } else {
            self.state.last_query_verdict()
        };
        self.policy_allows(verdict).then_some(e)
    }

    fn wall_clock_in_effect(&self) -> bool {
        self.wall_clock_arg
            || self
                .state
                .options()
                .any(|(name, _, _)| self.profile.is_wall_clock_option(name))
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
        self.respond_plain(&entry.response)?;
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
    /// (section 13), otherwise as a hit. Never called with a lock held.
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
        let cfg = self.cfg;
        let lock = match (&cfg.dir, hashes) {
            (Some(dir), Some(h)) if self.store.can_write() => {
                let busy = || log!("in-flight lock busy for {}: waiting", h.key.to_hex());
                match lock::acquire(dir, h.key.as_bytes(), cfg.lock_wait, busy) {
                    lock::Acquired::Lock(l) => {
                        if l.waited {
                            self.counts.lock_waits += 1;
                        }
                        if let Some(entry) = self.accepted(&h, is_query) {
                            drop(l);
                            return self.serve_entry(cmd, h, entry, start, true);
                        }
                        Some(l)
                    }
                    lock::Acquired::TimedOut => {
                        self.counts.lock_timeouts += 1;
                        None
                    }
                    lock::Acquired::Unavailable => None,
                }
            }
            _ => None,
        };

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
            (self.ask(&cmd.canonical, true)?.concat(), None)
        };
        let solve_us = solve_start.elapsed().as_micros() as u64;
        let clean = !self.has_error(&response);

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
            let entry = Entry {
                response: response.clone(),
                is_query,
                rlimit: delta,
                solve_us,
                last_used_s: 0,
            };
            if self.store.put(h.key.as_bytes(), &entry) {
                self.counts.stores += 1;
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
        let cached = Verdict::of(&entry.response);
        let key_hex = h.key.to_hex();
        let contradiction = match self.audit(&cmd.canonical) {
            Err(e) => {
                self.counts.audit_failures += 1;
                log!("audit failed ({e}); answering from the cache");
                None
            }
            Ok((response, _)) if Verdict::of(&response) == cached => None,
            Ok((response, delta)) => {
                let now = Verdict::of(&response);
                if cached.is_definite() && now.is_definite() {
                    Some((response, delta, now))
                } else {
                    self.counts.audit_instabilities += 1;
                    log!("audit: instability for {key_hex}: cached {cached}, now {now}; keeping the entry");
                    None
                }
            }
        };
        let Some((response, delta, now)) = contradiction else {
            return self.answer_from_entry(cmd, h, entry, start, false);
        };
        self.counts.audit_contradictions += 1;
        log!("audit: CONTRADICTION for {key_hex}: cached {cached}, now {now}; entry deleted");
        self.store.delete(h.key.as_bytes());
        // Answered by a solver: a miss (rule 10).
        self.counts.misses += 1;
        self.rlimit += delta;
        self.state.append_query(cmd.canonical.as_slice(), response.as_slice(), now);
        self.respond(&response)?;
        self.record(true, Some(h), Some(now), start, "solver");
        Ok(())
    }

    /// Solves `query` in a new solver after the whole current path. Returns
    /// the response and its resource cost. Unlike [`Session::materialize`],
    /// queries on the path are re-run without sentinels, and any error
    /// fails the audit.
    fn audit(&mut self, query: &[u8]) -> Result<(Vec<u8>, u64), String> {
        let mut solver = Solver::spawn(self.real, self.args).map_err(|e| format!("cannot start: {e}"))?;
        let mut batch = Vec::new();
        push_unsent(&self.state, &mut vec![0], &mut batch, |batch, r| push_line(batch, &r.command));
        let stats = self.profile.statistics_command();
        for cmd in [&b""[..], stats, query, stats] {
            push_framed(&mut batch, cmd, &self.sentinel_cmd);
        }
        let result = self.audit_exchange(&mut solver, &batch);
        let _ = solver.finish();
        // The audit solver replaced the live one as the process to kill on a
        // signal.
        if let Some(live) = &self.live {
            live.solver.track();
        }
        result
    }

    /// Sends the audit's batch and reads its four sections: the path's
    /// output, statistics, the query's response and statistics again.
    fn audit_exchange(&self, solver: &mut Solver, batch: &[u8]) -> Result<(Vec<u8>, u64), String> {
        solver.send(batch).map_err(|e| format!("cannot write: {e}"))?;
        let mut sections = Vec::with_capacity(4);
        for _ in 0..4 {
            let r = solver.read_until(&self.sentinel_lines);
            if r.eof {
                return Err("the audit solver exited".to_string());
            }
            sections.push(r.lines);
        }
        if sections.iter().any(|s| s.iter().any(|l| self.profile.is_error_line(l))) {
            return Err("the audit solver reported an error".to_string());
        }
        let before = self.profile.parse_rlimit(&sections[1]).unwrap_or(0);
        let after = self.profile.parse_rlimit(&sections[3]).unwrap_or(0);
        Ok((sections.swap_remove(2).concat(), after.saturating_sub(before)))
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
        if self.cfg.mode == Mode::Enforce {
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
        let sentinel = &self.sentinel_cmd;
        let mut batch = Vec::new();
        // One entry per sentinel: `None` for output to pass on, `Some` for a
        // re-run query's answer and what was served for it.
        let mut sections: Vec<Option<Vec<u8>>> = Vec::new();
        push_unsent(&self.state, &mut live.sent, &mut batch, |batch, r| {
            if let Some(served) = &r.served {
                push_line(batch, sentinel);
                sections.push(None);
                push_framed(batch, &r.command, sentinel);
                sections.push(Some(served.clone()));
            } else {
                push_line(batch, &r.command);
            }
            if let Some((name, value)) = &r.option {
                live.options.insert(name.clone(), value.clone());
            }
        });
        // Replaying the path in order, with options reconciled at each pop
        // (pop_live), leaves the solver with the client's options. A
        // difference here would be a bug; log it and correct it rather than
        // answer under the wrong settings.
        let replayed = batch.len();
        if live.sync_options(&self.state, &mut batch) {
            log!("option drift: {}", String::from_utf8_lossy(&batch[replayed..]).trim_end());
        }
        if batch.is_empty() {
            return Ok(());
        }
        push_line(&mut batch, sentinel);
        sections.push(None);
        if live.solver.send(&batch).is_err() {
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
            push_framed(&mut batch, c, &self.sentinel_cmd);
        }
        if live.solver.send(&batch).is_err() {
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

    /// [`Session::exchange`] for one command; `visible` if the client asked
    /// for it. Returns its response lines.
    fn ask(&mut self, cmd: &[u8], visible: bool) -> Result<Vec<Vec<u8>>, Stop> {
        let mut sections = self.exchange(&[cmd], visible.then_some(0))?;
        Ok(sections.swap_remove(0))
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
        let forwarding = self.passthrough.is_some();
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
            let _ = live.solver.write(&rest);
        }
        self.shutdown()
    }

    /// Closes the solver's input, passes on its remaining output, and waits.
    /// Returns 0 if no solver was started.
    fn shutdown(&mut self) -> i32 {
        let Some(mut live) = self.live.take() else {
            return 0;
        };
        let (rest, code) = live.solver.finish();
        let _ = self.respond(&rest);
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
        let stats = self.ask(self.profile.statistics_command(), false)?;
        let real = self.profile.parse_rlimit(&stats).unwrap_or(0);
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
        match class {
            Class::Bypass(reason) => {
                return Step::Bypass {
                    reason,
                    trigger: Some(cmd),
                    raw: true,
                };
            }
            Class::Exit => return Step::Done(self.exit(&cmd)),
            _ => {}
        }
        let live = self.live.as_mut().expect("bypass has a live solver");
        if matches!(class, Class::Silent | Class::Push(_) | Class::Pop(_) | Class::SetOption { .. }) {
            return match live.solver.write(&cmd.raw) {
                Ok(()) => Step::Continue,
                Err(_) => Step::Done(self.solver_gone()),
            };
        }
        // Everything else may answer, including commands outside the tables.
        let sent = live
            .solver
            .write(&cmd.raw)
            .and_then(|_| live.solver.send(&self.sentinel_cmd));
        if sent.is_err() {
            return Step::Done(self.solver_gone());
        }
        let resp = live.solver.read_until(&self.sentinel_lines);
        let mut lines = resp.lines;
        // Output before the response itself (errors from earlier commands)
        // is left alone.
        if class == Class::Statistics
            && let Some(start) = lines.iter().position(|l| l.starts_with(b"(:"))
        {
            let offset = self.passthrough.unwrap_or(0);
            for l in &mut lines[start..] {
                self.profile.shift_rlimit(l, offset);
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
        let Some(store) = self.store.writing() else {
            return;
        };
        let counters = session_counters(&self.counts, self.errored, self.bypass.as_deref());
        if let Err(e) = store.finish(&self.touched, &counters) {
            log!("cannot update counters: {e}");
        }
        // Section 9: one session in `gc_every` checks the size at exit and,
        // if the store is over its cap, starts `z3cache gc` in the
        // background, so that eviction never delays this session's exit.
        let gc_every = self.cfg.gc_every;
        let draw = u64::from_le_bytes(self.nonce[24..].try_into().expect("8 bytes"));
        let chosen = gc_every > 0 && draw % u64::from(gc_every) == 0;
        if chosen && store.db_bytes().unwrap_or(0) > self.cfg.max_size {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_shows_every_session_counter() {
        for (name, _) in session_counters(&Counts::default(), true, Some("reason")) {
            if name.starts_with("bypass: ") {
                continue;
            }
            assert!(crate::cli::COUNTERS.iter().any(|(n, _)| *n == name), "{name} is not shown by stats");
        }
    }
}
