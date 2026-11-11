# z3cache design

## 1. Overview

z3cache is a drop-in replacement for the `z3` binary that caches solver
answers. It is built for Verus development across many git worktrees of the
same project, where most proof obligations are identical between worktrees and
would otherwise be re-proved from scratch in each one.

Guarantees:

- Works with unmodified Verus: point `VERUS_Z3_PATH` at the z3cache binary.
- One cache is shared by every worktree and every concurrent Verus process.
- A module whose queries are all cached never starts Z3.
- The cache never makes a build fail or hang.
- `unsat` is reported only if a real Z3 run produced `unsat` for the same
  input.

Scope:

- Z3 only. Solver-specific behaviour is kept in one place (section 17).
- The cache is local to one machine.
- Only solver time is saved. rustc and Verus's own passes still run, so the
  speedup is bounded by the solver share of `verus --time`.

## 2. How Verus drives Z3

As of Verus 0.2026.10.26 (`source/air/src/smt_process.rs`, `smt_verify.rs`,
`context.rs`, `emitter.rs`):

- **Spawn:** `z3 -smt2 -in`, binary path from `VERUS_Z3_PATH`. stdin and
  stdout are pipes; stderr is inherited.
- **Lifetime:** one long-lived process per module, shut down by closing stdin
  and waiting for exit.
- **Framing:** every batch of commands ends with `(echo "<<DONE>>")`. Verus
  reads lines until it sees `<<DONE>>`.
- **Strict output:** Verus treats any line it does not recognise as an error
  and panics on more than two consecutive empty lines.
- **Comments and source paths** go to log files, never to the pipe.
- **Startup:** a block of `set-option` commands, then the module context
  (prelude, declarations, axioms) at the top level.
- **Between functions** Verus adds top-level declarations and axioms (for
  example `req%…` functions for the next function's precondition), so the
  base frame grows during the session.
- **Per function:**

  ```
  (push)
  (get-info :all-statistics)        ; Verus reads only rlimit-count
    <local declarations>
    (assert (not <labelled goal>))
  (get-info :all-statistics)
  (get-info :version)
  (set-option :rlimit N)
  (check-sat)
  (set-option :rlimit 0)
    [unknown        → (get-info :reason-unknown)]
    [unsat + usage  → (get-unsat-core)]
    [failed         → (get-model), then more asserts and check-sats]
  (get-info :all-statistics)
  (pop)
  ```

- **Resource accounting:** Verus subtracts successive `rlimit-count` values to
  report per-query cost, so the values must never decrease.
- **Profiling:** `verus --profile` sets `trace` and `trace_file_name`, which
  makes Z3 write a file.

## 3. Architecture

```
verus ──stdin──▶ z3cache ──(only on a miss)──▶ real z3
      ◀─stdout──    │
                    ▼
       cache-v1.sqlite3   (one file, shared by every worktree)
```

There is one z3cache process per Verus solver process and no daemon. Each
proxy tracks the solver state the client has built up, answers from the cache
when it can, and starts the real Z3 only when it cannot.

Terms:

- **Client:** the program driving the solver, normally Verus.
- **Path:** the commands currently visible to the solver, as a stack of
  frames. `push` opens a frame; `pop` discards the top one.
- **Materialize:** start the real Z3 if needed and send it the part of the
  path it has not yet seen.
- **Bypass:** materialize, then pass bytes between client and solver for the
  rest of the session with no cache reads or writes.

## 4. Invocation

| Invocation | Behaviour |
|---|---|
| Arguments are `-smt2`, `-in` and `name=value` parameters whose names are on the option allowlist (section 5) | Proxy |
| Any other arguments (`--version`, a file name, `-st`, `-t:` …) | `exec` the real Z3 with the same arguments |
| Invoked as `z3cache` with a subcommand | Command-line tool (section 12) |
| `Z3CACHE_MODE=off` | `exec` the real Z3 |

Invoked as `z3` (a symlink), z3cache never treats its arguments as a
subcommand. The real Z3 is `Z3CACHE_REAL_Z3`, or else the first `z3` on
`PATH` that is not z3cache itself.

## 5. Command handling

A streaming reader splits the input into top-level commands. It understands
nested parentheses, string literals (with `""` as the only escape), `|quoted
symbols|` and `;` comments, and copes with commands split across reads.

Each command has a **canonical form**: its tokens joined by single spaces, no
space after `(` or before `)`, comments dropped, string literals and quoted
symbols byte-exact. Hashing and replay use the canonical form, except for
commands whose response repeats the command's own text: Z3's `get-value`
prints each term as the client wrote it, so `get-value` is keyed and sent
exactly as written (the solver profile names such commands). Two adjacent
non-parenthesis tokens with nothing between them (such as `abc"def"`) are a
parse error, because the solver's reading of them is not obvious.

Every command falls into exactly one class of the table below (the **class
tables**). A command in no class causes bypass.

| Class | Commands | Handling |
|---|---|---|
| Silent | `declare-*`, `define-*`, `assert`, `set-info`, `set-logic` | Append to the top frame |
| Scope | `push`, `pop` (optional count) | Open or discard frames |
| Option | `set-option` of an allowlisted option | Update the option map (section 6) |
| Local | `echo`, `get-info :version` | Answer without a solver |
| Statistics | `get-info :all-statistics` | Synthesize or rewrite (section 8) |
| Query | `check-sat`, `check-sat-assuming` | Look up; append to the top frame |
| Read | `get-model`, `get-value`, `get-unsat-core`, `eval`, other `get-info`, `get-option` | Look up |
| Exit | `exit`, end of input | Shut down |
| Anything else | `reset`, `get-proof`, other options, unknown commands | Bypass |

`echo` prints the string contents without quotes, as Z3 does. The reply to
`get-info :version` is recorded the first time a solver gives it and answered
locally afterwards.

**Options** are cached only if they are on an explicit allowlist in the
solver profile, matched by exact spelling. An option is listed only if it is
plain configuration: its value goes into the key, `pop` does not interact with
it, and it writes nothing to stdout or to files. The Z3 list is the set of
options Verus sends:

`auto_config`, `pi.enabled`, `rewriter.sort_disjunctions`, `rlimit`,
`smt.arith.nl`, `smt.arith.solver`, `smt.case_split`, `smt.delay_units`,
`smt.mbqi`, `smt.qi.eager_threshold`.

Any other option bypasses. That includes other spellings of listed options
(`:RLIMIT`), options with scope effects such as `global-decls` (definitions
then survive `pop`, which the key would not see), and options that change the
output format such as `print-success` or `smtlib2_compliant`. The same list
decides which command-line `name=value` parameters keep proxy mode.

## 6. State and cache key

The session state is:

- `options`: each option's current value.
- `frames`: a stack. Each frame holds its commands in order and a running
  BLAKE3 hash of their canonical forms, option commands excluded. For a
  query, the hash also covers the response given to the client, because what
  follows a query (a read such as `get-model`, or a later query in the same
  frame) depends on its answer.
- A cursor: how much of the path the live solver has received.

```
key = BLAKE3(
    "z3cache/key" ‖ FORMAT_VERSION ‖
    solver_id ‖ argv ‖
    options            -- (name, value) pairs sorted by name
    ‖ frame_hash[0] ‖ … ‖ frame_hash[n]
    ‖ canonical(command))
```

Every field is length-prefixed. `solver_id` is the BLAKE3 hash of the Z3
binary. `FORMAT_VERSION` changes whenever canonicalisation, classification or
the key layout changes, which makes old entries unreachable.

Properties:

- **Sibling independence.** After `pop` the state is what it was before the
  matching `push`, so a function's key does not depend on the queries of
  functions checked before it. It does depend on top-level declarations made
  before it (section 15).
- **Whitespace independence.** Reformatting the input does not change keys.
- **Exactness.** A hit means the same Z3 binary once received the same
  commands, with the same answers, in the same order.

### Options

Z3 options are global: `pop` does not undo them. Verus sets `rlimit` to N
just before `check-sat` and back to 0 just after, inside the function's frame.

- For the **key**, options are taken from the map, not from frame hashes, so
  each query sees exactly the values in effect.
- For **replay**, `set-option` commands stay in their frame at their original
  position, because some options must precede assertions.
- On **pop**, option changes made inside the popped frame are appended to the
  parent frame's replay list (not its hash), unless the parent already had the
  same value.
- The proxy records the option values the **live solver** has received. When
  it sends `(pop)` for a frame the solver had, it follows it at once with the
  client's current value for every option that differs, so the solver's
  options match the client's at the same point in the command sequence.

## 7. Session

**Silent, scope and option commands** update the state only. If a `pop`
discards a frame the live solver has, the solver is sent `(pop)` too.

**Queries and reads:**

1. Compute the key and look it up.
2. **Hit:** write the stored response. For a query, add its recorded resource
   cost to the virtual counter (section 8) and append it to the top frame.
3. **Miss:** materialize, send the command followed by a private sentinel
   `echo`, read lines up to the sentinel, write them to the client, and store
   the entry if the store policy allows. A query is bracketed by two
   statistics reads to measure its resource cost.

**Materializing** sends the unsent part of the path in order, with `(push)`
at each frame boundary. Queries on the path that were answered from the cache
are re-run; each is framed by sentinels so its answer can be compared with the
one served. A difference is a replay mismatch: it is counted, logged, and
stops storing for the session. Anything else the solver prints during replay
(such as errors) goes to the client unchanged.

A fully cached module therefore never starts Z3, and a hit costs one database
read. A miss costs what it would without the cache, plus loading the context
once per session.

**Errors appear late.** Silent commands are sent only when the solver is
needed, so an error the solver reports for one reaches the client with the
next solver response, and its line and column refer to the replayed text. An
error in a frame that is popped before the solver is needed is never
reported. No stored entry has a failing command on its path, so a hit never
hides an error.

**Store policy** (`Z3CACHE_STORE`):

- `unsat` (default): store a query only when its response is `unsat`, and a
  read only when the latest query on the path was answered `unsat`. Failures
  are re-solved every time, so a query near its resource limit is retried
  rather than pinned as failing.
- `all`: also store `sat` and `unknown` and the reads that follow them, except
  while a wall-clock limit is set (results then depend on machine load). The
  profile lists the solver's wall-clock options; none is on the allowlist.

The policy also applies to lookups: under `unsat`, entries that `all` stored
for failing queries, and the reads after them, are not used. A query response
is stored only if it is exactly its verdict line, and nothing is stored after
the solver reports an error.

**Solver lifetime** (`Z3CACHE_SOLVER`):

- `keep` (default): the solver stays alive across functions, as in plain
  Verus.
- `fresh`: when the client pops a frame the solver had, the solver is ended,
  so every miss runs in a solver that has seen only the current path. This
  makes each answer independent of earlier queries at the cost of reloading
  the context per miss.

**Errors and exit status.** Solver responses are checked for `(error` lines,
except the output of `echo` (and, in framed bypass, of `get-info :version`),
which is data. A raw bypass is not checked; the solver's exit status reports
its errors. Z3 exits 1 after
reporting an error, so a session that passed on an error exits 1 even if its
last solver exits 0. Otherwise the session exits with its last solver's
status, or 0 if none was started.

**Bypass** happens when a command is not in the class tables or sets an
option outside the allowlist (section 5), when a `pop` would go below the base frame, when the solver
reports an error, or when the input cannot be parsed. Bypass first
materializes, and is then normally **raw**: bytes are copied unchanged in both
directions until the solver's output ends.

Bypass is **framed** only when it must keep `rlimit-count` from going down:
the trigger was a `pop` below the base frame or a solver error, and the client
has already been given a count ahead of the solver's. Framed bypass forwards
the client's commands one at a time with a sentinel after each command that
may answer, and shifts `rlimit-count` in statistics responses (section 8). The
first command or option outside the class tables or allowlist switches it to
raw.

Framing relies on the sentinel `echo` coming back on a line of its own. Only
commands in the class tables and allowlisted options reach the solver while
framing is in use, and none of them changes `echo` output. As a safeguard the proxy
also accepts the quoted form of the sentinel.

## 8. Statistics

Verus asks for `(get-info :all-statistics)` three times per function and reads
`rlimit-count`. Responses recorded from different solver runs would not be
monotone, so the proxy owns the counter:

- A virtual counter `V` starts at 0.
- Around every missed query the proxy reads the real `rlimit-count` before and
  after, adds the difference to `V`, and stores it with the entry.
- A hit adds the stored difference to `V`.
- With no solver running, a statistics request is answered with
  `(:rlimit-count V)`. With one running, the request is forwarded and the
  value in the response replaced by `V`.
- In framed bypass, statistics responses are shifted up by `V` minus the real
  count when bypass began.
- A raw bypass changes nothing, so after cache hits a later `rlimit-count`
  can be lower than an earlier one. This happens only when a command or
  option outside the class tables or allowlist forces a raw bypass, because
  framing such output could hang. Verus sends no such command or option.

Verus therefore reports the original solve cost for cached queries.

## 9. Store

One SQLite file, `cache-v1.sqlite3`, in `Z3CACHE_DIR`, by default
`~/Library/Caches/z3cache` on macOS and `${XDG_CACHE_HOME:-~/.cache}/z3cache`
elsewhere. It must be on a local filesystem. It uses write-ahead logging, so
readers never wait for writers.

Tables:

- `entry`: key, the exact response bytes, whether it is a query, the resource
  cost, the original solve time, and creation and last-use times.
- `solver`: the identity of each Z3 binary (BLAKE3 of the executable),
  memoised by path, size and modification time so a binary is hashed once,
  plus its `get-info :version` reply once seen.
- `counters`: daily totals for `stats`.

Behaviour:

- **Schema changes:** the schema version is `PRAGMA user_version`. A writer
  that finds another version empties and recreates the file in one
  transaction; the cache is disposable.
- **Writes:** one small transaction per stored entry, so other worktrees see
  it immediately; if two sessions store the same key, the first entry is
  kept. Hits write nothing; counters and last-use times are written once at
  session end, and a last-use time only when it is a day old.
- **Read-only use** (`ro` and `enforce` sessions, `stats`) opens the database
  read-only and never creates or migrates it. SQLite may still create its
  `-wal` and `-shm` files next to an existing database. A missing or
  incompatible store means every lookup misses.
- **Eviction:** when the database exceeds `Z3CACHE_MAX_SIZE` (default 2 GB),
  least-recently-used entries are deleted, in batches, until it is at most 80%
  of the cap, and the space is returned to the file system. This runs on
  `z3cache gc`, and in the background when one session in `Z3CACHE_GC_EVERY`
  (default 100) finds the store over the cap at exit. Only one `gc` runs at a
  time.
- **Failure:** if the store is locked, corrupt or unwritable, the session
  continues with it disabled: lookups miss and nothing is written. Failing to
  record a solver's identity or version is only logged; it is a memo, and the
  store stays in use.
- `Z3CACHE_SOLVER_SALT` is mixed into `solver_id` for setups where Z3's
  behaviour depends on something other than its executable, such as a shared
  library.

## 10. Concurrent sessions

Worktrees started from the same commit miss the same queries at the same
moment. On a miss, a read-write session takes an exclusive `flock` on a lock
file named after the key before starting or feeding any solver. Another
session missing the same key waits, then usually finds the answer stored.

- After taking the lock, the session looks the key up again, since another
  session may have stored it in the meantime.
- Nothing is written to the client while a lock is held: output is kept and
  written after the lock is released, so a client that stops reading cannot
  hold up other sessions. The holder stores its entry before releasing.
- Waits are bounded by `Z3CACHE_LOCK_WAIT` (default 60 s). After that the
  waiter solves the query itself. A healthy holder takes about as long as the
  query, and Verus's resource limits keep queries short, so a hung or stopped
  holder costs each waiter at most one wait.
- A session holds at most one lock and never waits while holding one, so
  sessions cannot deadlock. The operating system releases the lock if the
  holder dies.
- If the holder's answer was not stored (for example `sat` under the default
  policy), the waiter solves the query itself.
- A lock that cannot be created is skipped. `gc` removes lock files that have
  been idle for an hour and are not held. A session that opened a lock file
  just as `gc` removed it may solve a key another session is also solving:
  duplicate work, never a wrong answer.

Counters `lock_waits`, `lock_hits` and `lock_timeouts` record waiting.

## 11. Configuration

All configuration is by environment variable, because the command line
belongs to Z3.

| Variable | Values | Default |
|---|---|---|
| `Z3CACHE_REAL_Z3` | Path to the real Z3 | First other `z3` on `PATH` |
| `Z3CACHE_DIR` | Cache directory | Platform cache directory |
| `Z3CACHE_MODE` | `rw`, `ro`, `enforce`, `off` | `rw` |
| `Z3CACHE_STORE` | `unsat`, `all` | `unsat` |
| `Z3CACHE_SOLVER` | `keep`, `fresh` | `keep` |
| `Z3CACHE_MAX_SIZE` | Size such as `2G`, `500M` | `2G` |
| `Z3CACHE_VERIFY` | Fraction of query hits to audit, 0–1 | 0 |
| `Z3CACHE_GC_EVERY` | One session in this many checks the size at exit; `0` never | 100 |
| `Z3CACHE_LOCK_WAIT` | Longest wait for another session's answer, seconds | 60 |
| `Z3CACHE_RECORD` | Directory for transcripts and per-query records | Unset |
| `Z3CACHE_LOG` | Diagnostic log file | Unset |
| `Z3CACHE_SOLVER_SALT` | Extra bytes mixed into `solver_id` | Empty |

An invalid value is never an error: z3cache logs a warning and uses the
default.

Modes:

- `rw`: read and write.
- `ro`: read only; misses are solved but not stored. Nothing in the store
  changes, so these sessions do not appear in `stats`.
- `enforce`: read only, and a miss prints an `(error …)` line and exits
  non-zero without starting a solver.
- `off`: run the real Z3 directly.

## 12. Command-line tools

- `z3cache stats [--json]`: hits, misses, stores, bypassed sessions with
  reasons, replay mismatches, audit results, solver time saved, entry count
  and size.
- `z3cache gc`: evict to the size cap and remove idle lock files, then report
  what was done.
- `z3cache clear`: delete all entries.
- `z3cache doctor`: locate the real Z3, show its identity, check the cache
  directory, and run one small query cold and warm.
- `z3cache analyze <record-dir>…`: compare recorded runs (see below).

**Recording.** With `Z3CACHE_RECORD=<dir>`, each session saves its input and
output byte for byte, session metadata, and one record per query or read: the
key, a context hash (solver, arguments and every frame below the top), a query
hash (options, top frame and command), the verdict, the time taken, and
whether the answer came from the cache, the solver, or the cache after an
audit. Invocations that are not proxied are listed in `exec.log`.

`z3cache analyze` takes one record directory per run, in order. For each run
after the first it projects how many queries a cache filled by the earlier
runs would have answered, and for each projected miss whether the query itself
was new or only its context had changed. Projections are labelled as such.

## 13. Soundness and trust

- **What a hit claims:** this Z3 binary once answered this exact command
  sequence with this response. Nothing is inferred.
- **Added trust:** the proxy's parser and state tracking, BLAKE3, and everyone
  who can write to the cache directory. A forged entry is a forged proof.
- **Solver history.** A miss runs in a solver that skipped cached queries and
  popped frames, and that may not have seen a `push` where plain Z3 would
  have. This cannot produce a false `unsat`, but Z3's answers to failing or
  borderline queries can depend on that history. Observed with Z3 4.16.0:
  - a query near its resource limit can land on the other side of it;
  - a quantified query with `smt.mbqi` off can be `unknown` instead of
    `sat`, because Z3 preprocesses differently before its first `push`;
  - `:reason-unknown` wording (`canceled` after a push/pop, `max. resource
    limit exceeded` without one);
  - the model returned after a failure. Verus reads the labels of failing
    assertions from that model, so when a failure has several possible
    counterexamples, the errors Verus reports can differ from a direct run.
    Which proofs pass is unaffected.
- **Bias.** With the default policy, a query that passes once is recorded as
  passing even if it would fail on another run. The cache hides proof
  instability; it does not create unsoundness.
- **Audit.** `Z3CACHE_VERIFY=p` re-checks a fraction `p` of query hits, before
  answering, in a separate fresh solver given the whole current path:
  - same verdict: the cached answer is given;
  - `sat` against `unsat`: a contradiction. The entry is deleted and the
    client gets the fresh answer;
  - `unknown` on one side: instability. The entry is kept and the cached
    answer is given;
  - the audit solver fails: the cached answer is given.

  Audited answers are not counted as hits. Audits run only in `rw` mode.
- **Verification gates.** Merge gates and release verification should run
  with `Z3CACHE_MODE=off`.

## 14. Robustness

- stdout carries only bytes a real Z3 could have produced; diagnostics go to
  `Z3CACHE_LOG`. No empty lines are added or removed.
- Reports never overstate the cache: a response counts as a hit only if it was
  written from a stored entry without consulting a solver.
- Any internal failure degrades to a slower correct path (store disabled, or
  bypass). It never changes an answer and never exits non-zero on its own,
  except in `enforce` mode.
- At end of input the proxy closes the solver's stdin, passes on its remaining
  output, writes its bookkeeping, and exits with the status described in
  section 7.
- On SIGINT, SIGTERM or SIGHUP the proxy kills every solver it started (the
  live one and any audit solver) and exits.
- Solver output is drained by its own thread, so a large replay cannot
  deadlock on full pipes.
- Client input is read by its own thread through a small bounded queue, so a
  solver that stops reading pushes back on the client, and a solver that dies
  while the client is idle ends the session promptly.

Measured costs: a session that needs no solver starts in about 3 ms, and a
hit takes a few microseconds.

## 15. Limitations

- **Module context.** A key covers the whole path, and Verus adds each
  function's declarations to the module's base frame as it goes (section 2).
  An edit early in a module therefore changes the keys of the functions
  checked after it in that module, even if their own proofs are unchanged.
  Keys built from the axioms Z3 actually used (its unsat core) would avoid
  this. On a large Verus project such misses cost at most about 10% of solver
  time, so they are not implemented.
- **Verus nondeterminism.** For an enum whose variants have different named
  fields, Verus 0.2026.10.26 emits the field accessors in an order that
  varies between runs, so queries in that module miss even when nothing
  changed. Tuple variants are not affected.
- **Output kept under a lock.** Client output is held in memory while an
  in-flight lock is held (section 10), without a size cap. Verus prints
  nothing during a normal replay.
- **Late errors.** Errors in silent commands are reported with the next
  solver response, with line numbers from the replay (section 7).

## 16. Solver assumptions

| # | Assumption | Evidence |
|---|---|---|
| A1 | `set-option` is not undone by `pop` | Z3 4.16.0, differential test `a1_options_survive_pop` |
| A2 | `echo` prints its string unquoted on one line, with `""` unescaped and `\` not an escape | Z3 4.16.0, differential test `a2_echo_is_unquoted` |
| A3 | Verus reads only `rlimit-count` from statistics and accepts a one-entry list | A warm full run answered with synthesized statistics gave identical results |
| A4 | Verus follows the protocol in section 2 | Recorded transcripts of full runs; output byte-identical to Z3 on every session |
| A5 | Verus passes no arguments beyond `-smt2 -in` | Recorded invocations of full runs |
| A6 | Verus emits the same commands for identical input | Mostly: repeated runs give identical keys, except that Verus 0.2026.10.26 orders the accessor declarations of enum variants with differing named fields differently from run to run (section 15) |
| A7 | Reading statistics around a query does not change the solver's answer | Differential tests; not checked systematically |

## 17. Solver profiles

The splitter, frames, key, replay and store work for any SMT-LIB solver
driven over a pipe. Everything that depends on the solver goes through a
`SolverProfile`. Only a Z3 profile exists. A profile decides:

| Concern | Z3 |
|---|---|
| Arguments that select proxy mode | `-smt2 -in` |
| `echo` output | Unquoted |
| Statistics | `rlimit-count`, synthesized (section 8) |
| Resource limit | `set-option :rlimit`, deterministic |
| Option allowlist | Section 5 |
| Wall-clock options (never stored except `unsat`) | Section 7 |

Another solver would need its own answers to each row. For example, cvc5
quotes `echo` output, and Verus runs it under a wall-clock timeout, so only
`unsat` could safely be stored for it. `solver_id` keeps different solvers'
entries apart in one store.

## 18. Testing

1. **Unit:** the command splitter (strings, quoted symbols, comments, commands
   split across reads), state hashing, classification, statistics, the store.
   Property tests run fixed seeds: generated commands, written with random
   whitespace and comments, split the same at any chunk boundaries; raw bytes
   and the remainder reproduce the input; the canonical form is a fixed point
   and does not depend on formatting; a balanced push…pop leaves the key
   unchanged; changing any command still on the path changes the key, and
   changing one in a popped frame does not.
2. **Stub solver** (`examples/stub_solver.rs`): a scripted stand-in for Z3
   that answers from a script and logs its launches. Covers lazy start,
   replay order, `pop` handling, bypass, policies, modes, and "a warm run
   starts no solver".
3. **Differential:** a corpus of transcripts run through real Z3, the proxy
   cold and the proxy warm, under each store policy and solver lifetime.
   Output must match apart from statistics and error positions. The corpus
   includes sessions recorded from the end-to-end sample crates
   (`verus_*.smt2`), so real Verus traffic is covered without Verus.
   **Randomized** (`tests/random.rs`): a few hundred sessions generated from
   fixed seeds (declarations, an uninterpreted function, nested push/pop,
   linear arithmetic and boolean assertions, quantifiers, `check-sat`,
   `check-sat-assuming`, `get-value`, `get-unsat-core`, `echo`, allowlisted
   options), in families that share a prefix, with one cache per
   configuration shared by all sessions. The generator avoids what solver
   history may legitimately change (section 13): checks are made inside a
   frame, as Verus makes them; no check runs near its resource limit;
   `get-value` is asked only where the model is unique; `:reason-unknown`
   is compared only after `unknown`.
4. **Concurrency:** many processes on one store; one solve per key; sessions
   killed during writes leave a valid store; `gc` running alongside sessions.
   Tests synchronise on events rather than timing.
5. **End to end** (`tests/e2e.rs`, sample crates in `tests/verus/`): Verus
   directly and through the proxy, cold and warm, with identical results,
   diagnostics and exit status, and the counters to match:
   - passing proofs of many kinds: the warm run starts no solver;
   - intentional failures: re-solved under `unsat`, cached under `all`;
   - an exceeded `#[verifier::rlimit]`;
   - an edit: only the edited module misses, and a breaking edit fails;
   - two worktrees at once on one cache, held at a gate until one waits for
     the other's lock: each unsat query is solved once;
   - `--time` reports the same resource totals cold and warm;
   - profiling sessions bypass, with the reason in `stats`;
   - `off`, `enforce` and `ro` modes; several threads; `analyze` on two
     recorded runs.

   Each sample module is written so that Verus emits the same commands on
   every run: Verus 0.2026.10.26 orders the accessors of named enum fields
   that differ between variants differently from run to run, which changes
   the module's keys.

Tiers 3 and 5 need a real Z3 or Verus and skip with a message when none is
configured.

## 19. Prior art

| Source | What z3cache takes |
|---|---|
| ccache | Hash the compiler binary; masquerade under the tool's name; never fail the build; `stats`; size-capped LRU |
| sccache | Do not cache failures by default; storage behind a trait |
| `cranelift_isle_veri_caching` (Wasmtime) | A stack of push/pop frames, key = solver + live path + command, no solver until a miss, replay the path on a miss, an enforcing read-only mode |
| cargo-verus | Caches per crate through Cargo's freshness check, so it neither shares across worktrees nor survives a one-function edit |

The Wasmtime crate cannot be used directly: it replaces the `easy_smt` library
in-process, while Verus talks to Z3 through its own pipe code.
