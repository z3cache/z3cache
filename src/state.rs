//! Solver state as seen by the client: frames, options and keys
//! (DESIGN section 6).

use std::collections::BTreeMap;

use crate::FORMAT_VERSION;

/// A query's outcome, read from the first line of its response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Sat,
    Unsat,
    Unknown,
    /// Any other first line.
    Other,
    /// The response reported an error.
    Error,
}

impl Verdict {
    /// The verdict of a response that reports no error.
    pub fn of(response: &[u8]) -> Verdict {
        let first = response.split(|&b| b == b'\n').next().unwrap_or(b"");
        match first.trim_ascii() {
            b"sat" => Verdict::Sat,
            b"unsat" => Verdict::Unsat,
            b"unknown" => Verdict::Unknown,
            _ => Verdict::Other,
        }
    }

    /// `sat` or `unsat`.
    pub fn is_definite(self) -> bool {
        matches!(self, Verdict::Sat | Verdict::Unsat)
    }

    /// The name used in logs and records.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Sat => "sat",
            Verdict::Unsat => "unsat",
            Verdict::Unknown => "unknown",
            Verdict::Other => "other",
            Verdict::Error => "error",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Inputs to every key that do not change during a session.
#[derive(Debug, Clone)]
pub struct KeyContext {
    pub solver_id: [u8; 32],
    pub argv: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OptionValue {
    value: Vec<u8>,
    /// The canonical `set-option` command that set it, for replay.
    command: Vec<u8>,
}

/// A command to send when materialising a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// Canonical form.
    pub command: Vec<u8>,
    /// For a query: the response already given to the client, which a
    /// re-run should reproduce.
    pub served: Option<Vec<u8>>,
    /// For a `set-option`: the option name and value it sets.
    pub option: Option<(Vec<u8>, Vec<u8>)>,
}

struct Frame {
    hasher: blake3::Hasher,
    /// Commands to send when materialising this frame, in order.
    replay: Vec<Replay>,
    /// The option map when this frame was pushed.
    options_at_push: BTreeMap<Vec<u8>, OptionValue>,
    /// The verdict of the latest query in this frame.
    last_query: Option<Verdict>,
}

impl Frame {
    fn new(options_at_push: BTreeMap<Vec<u8>, OptionValue>) -> Self {
        let mut hasher = blake3::Hasher::new();
        put(&mut hasher, b"z3cache/frame");
        Frame {
            hasher,
            replay: Vec::new(),
            options_at_push,
            last_query: None,
        }
    }
}

/// The hashes computed for a query or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hashes {
    /// The cache key: everything the response can depend on.
    pub key: blake3::Hash,
    /// Solver, arguments and every frame below the top one.
    pub context: blake3::Hash,
    /// Options, the top frame and the command itself.
    pub query: blake3::Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopUnderflow;

pub struct State {
    options: BTreeMap<Vec<u8>, OptionValue>,
    frames: Vec<Frame>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        State {
            options: BTreeMap::new(),
            frames: vec![Frame::new(BTreeMap::new())],
        }
    }

    /// Number of open `push` frames.
    pub fn depth(&self) -> usize {
        self.frames.len() - 1
    }

    fn top(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("base frame is never popped")
    }

    /// Appends a silent command.
    pub fn append(&mut self, canonical: impl Into<Vec<u8>>) {
        let canonical = canonical.into();
        let top = self.top();
        put(&mut top.hasher, &canonical);
        top.replay.push(Replay {
            command: canonical,
            served: None,
            option: None,
        });
    }

    /// Appends a query after it has been answered with `served`, whose
    /// verdict is `verdict`. The response goes into the frame hash with the
    /// command: what follows a query (a read such as `get-model`, or a later
    /// query in the frame) depends on its answer, so its key must too.
    pub fn append_query(
        &mut self,
        canonical: impl Into<Vec<u8>>,
        served: impl Into<Vec<u8>>,
        verdict: Verdict,
    ) {
        let (canonical, served) = (canonical.into(), served.into());
        let top = self.top();
        put(&mut top.hasher, &canonical);
        put(&mut top.hasher, b"z3cache/served");
        put(&mut top.hasher, &served);
        top.replay.push(Replay {
            command: canonical,
            served: Some(served),
            option: None,
        });
        top.last_query = Some(verdict);
    }

    /// The verdict of the latest query still on the path, if any.
    pub fn last_query_verdict(&self) -> Option<Verdict> {
        self.frames.iter().rev().find_map(|f| f.last_query)
    }

    /// Number of frames, including the base frame.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn set_option(&mut self, name: &[u8], value: &[u8], canonical: &[u8]) {
        self.options.insert(
            name.to_vec(),
            OptionValue {
                value: value.to_vec(),
                command: canonical.to_vec(),
            },
        );
        self.top().replay.push(Replay {
            command: canonical.to_vec(),
            served: None,
            option: Some((name.to_vec(), value.to_vec())),
        });
    }

    /// The options in effect for the client: (name, value, the canonical
    /// command that set it).
    pub fn options(&self) -> impl Iterator<Item = (&[u8], &[u8], &[u8])> {
        self.options
            .iter()
            .map(|(n, v)| (n.as_slice(), v.value.as_slice(), v.command.as_slice()))
    }

    pub fn push(&mut self, n: u64) {
        for _ in 0..n {
            self.frames.push(Frame::new(self.options.clone()));
        }
    }

    /// Discards `n` frames. Option changes made inside a discarded frame
    /// outlive it, so they are added to the parent's replay list (but not
    /// its hash) unless the parent already had the same value.
    pub fn pop(&mut self, n: u64) -> Result<(), PopUnderflow> {
        if n > self.depth() as u64 {
            return Err(PopUnderflow);
        }
        for _ in 0..n {
            let popped = self.frames.pop().expect("checked depth");
            let changed: Vec<Replay> = self
                .options
                .iter()
                .filter(|(name, now)| {
                    popped.options_at_push.get(*name).map(|o| &o.value) != Some(&now.value)
                })
                .map(|(name, now)| Replay {
                    command: now.command.clone(),
                    served: None,
                    option: Some((name.clone(), now.value.clone())),
                })
                .collect();
            self.top().replay.extend(changed);
        }
        Ok(())
    }

    /// The replay list of frame `i` (0 is the base frame).
    pub fn replay(&self, i: usize) -> &[Replay] {
        &self.frames[i].replay
    }

    pub fn hashes(&self, ctx: &KeyContext, command: &[u8]) -> Hashes {
        let frame_hashes: Vec<blake3::Hash> =
            self.frames.iter().map(|f| f.hasher.finalize()).collect();
        let (top, below) = frame_hashes.split_last().expect("base frame");

        let mut key = blake3::Hasher::new();
        put(&mut key, b"z3cache/key");
        put(&mut key, &FORMAT_VERSION.to_le_bytes());
        self.put_solver(&mut key, ctx);
        self.put_options(&mut key);
        put_count(&mut key, frame_hashes.len());
        for h in &frame_hashes {
            put(&mut key, h.as_bytes());
        }
        put(&mut key, command);

        let mut context = blake3::Hasher::new();
        put(&mut context, b"z3cache/context");
        put(&mut context, &FORMAT_VERSION.to_le_bytes());
        self.put_solver(&mut context, ctx);
        put_count(&mut context, below.len());
        for h in below {
            put(&mut context, h.as_bytes());
        }

        let mut query = blake3::Hasher::new();
        put(&mut query, b"z3cache/query");
        put(&mut query, &FORMAT_VERSION.to_le_bytes());
        self.put_options(&mut query);
        put(&mut query, top.as_bytes());
        put(&mut query, command);

        Hashes {
            key: key.finalize(),
            context: context.finalize(),
            query: query.finalize(),
        }
    }

    fn put_solver(&self, h: &mut blake3::Hasher, ctx: &KeyContext) {
        put(h, &ctx.solver_id);
        put_count(h, ctx.argv.len());
        for a in &ctx.argv {
            put(h, a);
        }
    }

    fn put_options(&self, h: &mut blake3::Hasher) {
        put_count(h, self.options.len());
        for (name, v) in &self.options {
            put(h, name);
            put(h, &v.value);
        }
    }
}

fn put(h: &mut blake3::Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn put_count(h: &mut blake3::Hasher, n: usize) {
    h.update(&(n as u64).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Class, classify};
    use crate::profile::Z3;
    use crate::sexp::Splitter;

    fn ctx() -> KeyContext {
        KeyContext {
            solver_id: [7; 32],
            argv: vec![b"-smt2".to_vec(), b"-in".to_vec()],
        }
    }

    /// Feeds commands to a state the way the session does, without a solver.
    fn run(state: &mut State, src: &str) {
        let mut s = Splitter::new();
        s.feed(src.as_bytes());
        while let Some(cmd) = s.next_command() {
            let cmd = cmd.unwrap();
            match classify(&cmd, &Z3) {
                Class::Silent => state.append(cmd.canonical),
                Class::Query => state.append_query(cmd.canonical, b"unsat\n", Verdict::Unsat),
                Class::Push(n) => state.push(n),
                Class::Pop(n) => state.pop(n).unwrap(),
                Class::SetOption { name, value } => state.set_option(&name, &value, &cmd.canonical),
                _ => {}
            }
        }
    }

    fn key(src: &str) -> Hashes {
        let mut st = State::new();
        run(&mut st, src);
        st.hashes(&ctx(), b"(check-sat)")
    }

    #[test]
    fn push_then_pop_restores_the_key() {
        let base = "(declare-const x Int)(assert (> x 0))";
        assert_eq!(
            key(base),
            key(&format!(
                "{base}(push)(declare-const y Int)(assert (< x y))(check-sat)(pop)"
            ))
        );
        assert_eq!(
            key(base),
            key(&format!("{base}(push 2)(assert false)(pop 2)"))
        );
    }

    #[test]
    fn sibling_queries_are_independent() {
        let ctx_src = "(declare-const x Int)";
        let f = "(push)(assert (> x 1))";
        let g = "(push)(assert (> x 2))(check-sat)(pop)";
        assert_eq!(
            key(&format!("{ctx_src}{f}")),
            key(&format!("{ctx_src}{g}{f}"))
        );
    }

    #[test]
    fn whitespace_does_not_matter() {
        assert_eq!(
            key("(assert (> x 0))"),
            key("  (assert\n\t(>  x ; c\n 0))  ")
        );
    }

    #[test]
    fn content_options_and_order_matter() {
        assert_ne!(key("(assert (> x 0))"), key("(assert (> x 1))"));
        assert_ne!(key("(assert a)(assert b)"), key("(assert b)(assert a)"));
        assert_ne!(
            key("(assert a)(set-option :rlimit 5)"),
            key("(assert a)(set-option :rlimit 6)")
        );
        // A frame boundary is part of the path.
        assert_ne!(
            key("(assert a)(assert b)"),
            key("(assert a)(push)(assert b)")
        );
    }

    #[test]
    fn options_count_by_value_not_position() {
        // Verus sets rlimit before each check-sat and resets it after.
        assert_eq!(
            key("(set-option :rlimit 0)(assert a)(set-option :rlimit 5)"),
            key("(assert a)(set-option :rlimit 5)")
        );
    }

    #[test]
    fn option_changes_outlive_popped_frames() {
        let mut st = State::new();
        run(
            &mut st,
            "(set-option :rlimit 0)(assert a)\
             (push)(set-option :rlimit 5)(set-option :smt.mbqi true)(assert b)\
             (set-option :smt.mbqi false)(pop)",
        );
        // The parent's hash is unchanged...
        let mut fresh = State::new();
        run(&mut fresh, "(assert a)");
        assert_eq!(
            st.frames[0].hasher.finalize(),
            fresh.frames[0].hasher.finalize()
        );
        // ...but its replay list carries the options still in effect.
        let replay: Vec<String> = st
            .replay(0)
            .iter()
            .map(|c| String::from_utf8(c.command.clone()).unwrap())
            .collect();
        assert_eq!(
            replay,
            vec![
                "(set-option :rlimit 0)",
                "(assert a)",
                "(set-option :rlimit 5)",
                "(set-option :smt.mbqi false)",
            ]
        );
    }

    #[test]
    fn option_restored_inside_frame_is_not_replayed() {
        let mut st = State::new();
        run(
            &mut st,
            "(set-option :rlimit 0)(push)(set-option :rlimit 5)(check-sat)(set-option :rlimit 0)(pop)",
        );
        let replay: Vec<&[u8]> = st.replay(0).iter().map(|r| &r.command[..]).collect();
        assert_eq!(replay, vec![&b"(set-option :rlimit 0)"[..]]);
    }

    #[test]
    fn context_and_query_hashes_split_at_the_top_frame() {
        let a = key("(assert ctx1)(push)(assert q)");
        let b = key("(assert ctx2)(push)(assert q)");
        let c = key("(assert ctx1)(push)(assert q2)");
        assert_ne!(a.key, b.key);
        assert_eq!(a.query, b.query);
        assert_ne!(a.context, b.context);
        assert_eq!(a.context, c.context);
        assert_ne!(a.query, c.query);
    }

    #[test]
    fn last_query_verdict_follows_the_path() {
        let mut st = State::new();
        assert_eq!(st.last_query_verdict(), None);
        st.append_query(b"(check-sat)", b"unsat\n", Verdict::Unsat);
        st.push(1);
        st.append_query(b"(check-sat)", b"sat\n", Verdict::Sat);
        assert_eq!(st.last_query_verdict(), Some(Verdict::Sat));
        st.pop(1).unwrap();
        assert_eq!(st.last_query_verdict(), Some(Verdict::Unsat));
        assert_eq!(st.replay(0)[0].served.as_deref(), Some(&b"unsat\n"[..]));
    }

    #[test]
    fn keys_after_a_query_depend_on_its_answer() {
        let after = |answer: &[u8]| {
            let mut st = State::new();
            st.append(b"(assert a)");
            st.append_query(b"(check-sat)", answer, Verdict::Other);
            st.hashes(&ctx(), b"(get-model)").key
        };
        assert_ne!(after(b"sat\n"), after(b"unsat\n"));
        assert_eq!(after(b"sat\n"), after(b"sat\n"));
    }

    #[test]
    fn pop_underflow() {
        let mut st = State::new();
        st.push(1);
        assert_eq!(st.pop(2), Err(PopUnderflow));
        assert_eq!(st.pop(1), Ok(()));
        assert_eq!(st.pop(1), Err(PopUnderflow));
    }

    #[test]
    fn solver_and_arguments_are_in_the_key() {
        let st = State::new();
        let a = st.hashes(&ctx(), b"(check-sat)");
        let mut other = ctx();
        other.solver_id = [8; 32];
        assert_ne!(a.key, st.hashes(&other, b"(check-sat)").key);
        let mut other = ctx();
        other.argv.push(b"smt.random_seed=1".to_vec());
        assert_ne!(a.key, st.hashes(&other, b"(check-sat)").key);
    }

    // Property tests: random push/pop/append sequences from fixed seeds.

    use crate::TestRng;

    #[derive(Clone, Debug)]
    enum Op {
        Push(u64),
        Pop(u64),
        Append(String),
        Query(String, &'static str),
        SetOption(&'static str, String),
    }

    fn apply(st: &mut State, op: &Op) {
        match op {
            Op::Push(n) => st.push(*n),
            Op::Pop(n) => st.pop(*n).unwrap(),
            Op::Append(c) => st.append(c.as_bytes()),
            Op::Query(c, r) => {
                st.append_query(c.as_bytes(), r.as_bytes(), Verdict::of(r.as_bytes()))
            }
            Op::SetOption(name, value) => {
                let canonical = format!("(set-option :{name} {value})");
                st.set_option(name.as_bytes(), value.as_bytes(), canonical.as_bytes());
            }
        }
    }

    fn state_of(ops: &[Op]) -> State {
        let mut st = State::new();
        for op in ops {
            apply(&mut st, op);
        }
        st
    }

    fn gen_command(rng: &mut TestRng) -> String {
        format!(
            "(assert (> {} {}))",
            rng.pick(&["x", "y", "|a b|", "(f x)"]),
            rng.below(50)
        )
    }

    /// A random op sequence that stays within the frames it opens; options
    /// set inside a frame are restored before its pop, so that the
    /// sequence leaves no trace when wrapped in push...pop.
    fn gen_ops(rng: &mut TestRng, len: usize, depth: &mut u64, base: u64) -> Vec<Op> {
        let mut ops = Vec::new();
        let mut restore: Vec<Vec<Op>> = Vec::new();
        for _ in 0..len {
            match rng.below(10) {
                0 | 1 => {
                    let n = 1 + rng.below(2) as u64;
                    *depth += n;
                    ops.push(Op::Push(n));
                    restore.extend((0..n).map(|_| Vec::new()));
                }
                2 if *depth > base => {
                    let n = 1 + rng.below((*depth - base).min(2) as usize) as u64;
                    for _ in 0..n {
                        ops.extend(restore.pop().unwrap());
                    }
                    *depth -= n;
                    ops.push(Op::Pop(n));
                }
                3 => ops.push(Op::Query(
                    "(check-sat)".into(),
                    rng.pick(&["sat\n", "unsat\n", "unknown\n"]),
                )),
                4 if !restore.is_empty() => {
                    // Option values in the key come from the map; an
                    // option changed and then restored leaves it as it was.
                    let value = rng.below(5).to_string();
                    ops.push(Op::SetOption("smt.case_split", value));
                    restore
                        .last_mut()
                        .unwrap()
                        .push(Op::SetOption("smt.case_split", "0".into()));
                }
                _ => ops.push(Op::Append(gen_command(rng))),
            }
        }
        while let Some(r) = restore.pop() {
            ops.extend(r);
            *depth -= 1;
            ops.push(Op::Pop(1));
        }
        ops
    }

    const SEEDS: u64 = 500;

    #[test]
    fn property_a_balanced_push_pop_leaves_the_key_unchanged() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            // A prefix that may leave frames open and options set.
            let mut prefix = vec![Op::SetOption("smt.case_split", "0".into())];
            let mut depth = 0;
            for _ in 0..rng.below(12) {
                match rng.below(5) {
                    0 => {
                        depth += 1;
                        prefix.push(Op::Push(1));
                    }
                    1 => prefix.push(Op::Query("(check-sat)".into(), "unsat\n")),
                    _ => prefix.push(Op::Append(gen_command(&mut rng))),
                }
            }
            let mut st = state_of(&prefix);
            let before = st.hashes(&ctx(), b"(check-sat)");
            let frames = st.frame_count();
            let n = 1 + rng.below(3) as u64;
            st.push(n);
            let mut inner_depth = depth + n;
            let len = rng.below(30);
            let inner = gen_ops(&mut rng, len, &mut inner_depth, depth + n);
            for op in &inner {
                apply(&mut st, op);
            }
            assert_eq!(inner_depth, depth + n, "seed {seed}");
            st.pop(n).unwrap();
            assert_eq!(st.frame_count(), frames, "seed {seed}");
            assert_eq!(
                st.hashes(&ctx(), b"(check-sat)"),
                before,
                "seed {seed}: {prefix:?} push {n} {inner:?} pop {n}"
            );
        }
    }

    #[test]
    fn property_changing_a_command_on_the_path_changes_the_key() {
        for seed in 0..SEEDS {
            let mut rng = TestRng::new(seed);
            let mut depth = 0;
            let len = 1 + rng.below(30);
            let mut ops = gen_ops(&mut rng, len, &mut depth, 0);
            // Leave some frames open at the end, so that the path has
            // commands above the base frame.
            ops.truncate(rng.below(ops.len() + 1));
            let key = state_of(&ops).hashes(&ctx(), b"(check-sat)").key;
            // Which commands are still on the path: those whose frame has
            // not been popped since.
            let mut frames: Vec<Vec<usize>> = vec![Vec::new()];
            for (i, op) in ops.iter().enumerate() {
                match op {
                    Op::Push(n) => frames.extend((0..*n).map(|_| Vec::new())),
                    Op::Pop(n) => frames.truncate(frames.len() - *n as usize),
                    Op::Append(_) | Op::Query(..) => frames.last_mut().unwrap().push(i),
                    Op::SetOption(..) => {}
                }
            }
            let on_path: Vec<usize> = frames.concat();
            for (i, op) in ops.iter().enumerate() {
                let changed = match op {
                    Op::Append(c) => Op::Append(c.replacen('>', "<", 1)),
                    Op::Query(c, r) => {
                        // The same query with another answer.
                        let other = if *r == "sat\n" { "unsat\n" } else { "sat\n" };
                        Op::Query(c.clone(), other)
                    }
                    _ => continue,
                };
                let mut mutated = ops.clone();
                mutated[i] = changed;
                let other = state_of(&mutated).hashes(&ctx(), b"(check-sat)").key;
                if on_path.contains(&i) {
                    assert_ne!(
                        other, key,
                        "seed {seed}: changing op {i} ({op:?}) on the path kept the key"
                    );
                } else {
                    assert_eq!(
                        other, key,
                        "seed {seed}: changing popped op {i} ({op:?}) changed the key"
                    );
                }
            }
            // And the command being asked is part of the key.
            assert_ne!(
                state_of(&ops)
                    .hashes(&ctx(), b"(check-sat-assuming (a))")
                    .key,
                key,
                "seed {seed}"
            );
        }
    }
}
