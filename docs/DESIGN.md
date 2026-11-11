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
| Mode `off` (section 11) | `exec` the real Z3 |

Invoked as `z3` (a symlink), z3cache never treats its arguments as a
subcommand. Invoked as `z3cache` with no arguments while stdin is a terminal,
it prints a short pointer to `z3cache setup` and `z3cache help` and exits 0,
instead of starting a solver that would wait for input from the terminal.

**Finding the real Z3.** Every invocation that needs the real Z3 (proxy and
`exec` alike) takes the first of these that is an executable file and not
z3cache itself (same device and inode):

1. the `real_z3` setting (`Z3CACHE_REAL_Z3`). A relative path, a bare name
   included, is taken from the current directory. If it is set but is not
   an executable file, or is z3cache, that is an error: an explicit setting
   is never silently replaced;
2. `z3` in the directory of the parent process's executable, symlinks
   resolved. When Verus runs z3cache, the parent is Verus's driver
   (`rust_verify`), which a Verus release keeps next to its own `z3`. The
   executable comes from `/proc/<ppid>/exe` on Linux and `proc_pidpath` on
   macOS;
3. `z3` next to the first `verus` on `PATH`, symlinks resolved, for a Verus
   run some other way (for example through a wrapper script);
4. the first `z3` on `PATH`.

The solver found is resolved to one absolute path with every symlink
resolved, and that one file is what the session identifies (section 9),
hashes into `solver_id` and runs. A relative or bare name could otherwise
name one file when it is hashed and another when it is run, or a different
file in each working directory.

Each rule is a few system calls, and any error in one (no parent, a parent
that has exited, an unreadable `/proc`) only moves on to the next, so the
lookup can neither fail nor hang a build (rule 2). If nothing is found,
z3cache prints an error naming `z3cache setup` and `real_z3`, and exits 127
as a shell does for a missing command. The rule that found the solver is
written to the log, and `doctor` shows it.

Rule 2 is what makes `VERUS_Z3_PATH` the only setting a user needs: it finds
the `z3` of whichever Verus is running, so several Verus versions on one
machine each get their own. Run from a shell, `doctor` has no Verus parent;
it reports that no solver is reachable from there without treating it as a
fault, unless `real_z3` is set and wrong.

## 5. Command handling

A streaming reader splits the input into top-level commands. It understands
nested parentheses, string literals (with `""` as the only escape), `|quoted
symbols|` and `;` comments, and copes with commands split across reads.

Which bytes separate tokens comes from the solver profile (assumption A8).
For Z3 4.16 they are space, tab, line feed and carriage return. Any other
ASCII control byte, or DEL, outside a string literal, quoted symbol or
comment makes the input unparseable: Z3 reports it as an unexpected
character and reads on without it, so dropping it as whitespace would change
what Z3 is asked (`(assert<form feed>false)` is an error in Z3, not
`(assert false)`). Inside string literals and quoted symbols such bytes are
part of the token, except NUL, which Z3 does not keep (`(echo "a<NUL>b")`
prints `a`) and which is therefore unparseable too. Comments take any byte.

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
| Silent | `declare-*`, `define-*`, `assert` | Append to the top frame |
| Scope | `push`, `pop` (optional count) | Open or discard frames |
| Option | `set-option` of an allowlisted option | Update the option map (section 6) |
| Local | `echo`, `get-info :version` | Answer without a solver |
| Statistics | `get-info :all-statistics` | Synthesize or rewrite (section 8) |
| Query | `check-sat`, `check-sat-assuming` | Look up; append to the top frame |
| Read | `get-model`, `get-value`, `get-unsat-core`, other `get-info`, `get-option` | Look up while the latest result is valid (section 7) |
| Exit | `exit`, end of input | Shut down |
| Anything else | `set-info`, `set-logic`, `eval`, `reset`, `get-proof`, other options, unknown commands | Bypass |

`set-info` and `set-logic` bypass because their effects are global, not
scoped: `pop` does not undo `(set-info :status sat)`, whose check Z3 still
applies to later queries, and `:smt-lib-version 2.0` changes how string
literals are read. Verus sends neither. `eval` bypasses because
`:completion true` installs interpretations in the model, which changes what
a later `eval` prints.

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
- The **result**: the verdict of the latest query while its result is
  valid. Z3 discards the result on `push`, `pop` (even `(pop 0)`) and
  `assert`, and keeps it across declarations and the allowlisted options
  (assumption A9); the proxy discards it on `push`, `pop` and every silent
  command. It is not part of a frame: popping a frame does not bring back
  the result of a query made before the frame was pushed. A flag records
  whether a `pop` has come since the latest query.
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

1. Compute the key and look it up. A read is answered from the cache, and
   stored, only while the latest result is valid; otherwise it goes to the
   solver.
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

**A read after a pop.** The live solver may not have had a popped frame (it
was pushed and popped while the session answered from the cache), or may be
a new one that has just re-run the latest query, so it may still have the
result that Z3 would have discarded at the client's `pop`. Before a read
goes to the solver, and before bypass starts, the solver is therefore sent
the profile's discard commands (`(push)` and `(pop)` for Z3) if a `pop` has
come since the latest query, and answers as Z3 would (`model is not
available`). Every other way a result is discarded (`push`, `assert`) is on
the path the solver receives. Forwarding the read, rather than bypassing,
keeps the session caching.

**Errors appear late.** Silent commands are sent only when the solver is
needed, so an error the solver reports for one reaches the client with the
next solver response, and its line and column refer to the replayed text. An
error in a frame that is popped before the solver is needed is never
reported. No stored entry has a failing command on its path, so a hit never
hides an error.

**Store policy** (`Z3CACHE_STORE`):

- `unsat` (default): store a query only when its response is `unsat`, and a
  read only when the latest query was answered `unsat` and its result is
  still valid. Failures
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

The counter arithmetic is in `src/rlimit/counter.rs`: adding a query's cost,
the bypass offset, and the shifted value, each saturating at the limits of
`u64`. The session changes its counter and computes every value it gives the
client only through these functions. The same file models a session as a
sequence of events (a query answered, the live solver's own count changing,
a statistics request, the start of bypass) and proves with Verus that, for
every sequence, the values given to the client never decrease. The proof
assumes A10 (section 16) and leaves out raw bypass forced by an unknown
command, the exception above. Specifications are attributes under
`cfg(verus_keep_ghost)`, which only Verus sets, so ordinary builds neither
see them nor depend on Verus.

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

The command line belongs to Z3, so settings come from environment variables
and a config file. For each setting, a non-empty environment variable wins
over the config file, and the config file wins over the default. An empty
variable counts as unset.

| Variable | File key | Values | Default |
|---|---|---|---|
| `Z3CACHE_REAL_Z3` | `real_z3` | Path to the real Z3 | Found as in section 4 |
| `Z3CACHE_DIR` | `dir` | Cache directory | Platform cache directory |
| `Z3CACHE_MODE` | `mode` | `rw`, `ro`, `enforce`, `off` | `rw` |
| `Z3CACHE_STORE` | `store` | `unsat`, `all` | `unsat` |
| `Z3CACHE_SOLVER` | `solver` | `keep`, `fresh` | `keep` |
| `Z3CACHE_MAX_SIZE` | `max_size` | Size such as `2G`, `500M` | `2G` |
| `Z3CACHE_VERIFY` | `verify` | Fraction of query hits to audit, 0–1 | 0 |
| `Z3CACHE_GC_EVERY` | `gc_every` | One session in this many checks the size at exit; `0` never | 100 |
| `Z3CACHE_LOCK_WAIT` | `lock_wait` | Longest wait for another session's answer, seconds | 60 |
| `Z3CACHE_RECORD` | `record` | Directory for transcripts and per-query records | Unset |
| `Z3CACHE_LOG` | `log` | Diagnostic log file | Unset |
| `Z3CACHE_SOLVER_SALT` | `solver_salt` | Extra bytes mixed into `solver_id` | Empty |
| `Z3CACHE_CONFIG` | none | Config file path | See below |

**Config file.** The file is `$Z3CACHE_CONFIG` if that is set and non-empty;
otherwise `$XDG_CONFIG_HOME/z3cache/config.toml`, or
`~/.config/z3cache/config.toml` when `XDG_CONFIG_HOME` is unset, on every
platform. `Z3CACHE_CONFIG` can only be set in the environment. A missing file
is normal and silent. Every invocation reads it: the proxy, the decision to
`exec` the real Z3 (section 4), and the command-line tools (section 12).

The file is a [TOML](https://toml.io) document. Only top-level keys are read:

```toml
# ~/.config/z3cache/config.toml
real_z3 = "~/verus/z3"    # only if section 4's lookup cannot find it
dir = "/var/cache/z3cache"
max_size = "10G"
verify = 0.05
```

- A key is the variable name without `Z3CACHE_`, in lower case, as in the
  table. Keys are case-sensitive.
- `real_z3`, `dir`, `log`, `record`, `mode`, `store`, `solver` and
  `solver_salt` are strings. `max_size` is a size string such as `"2G"` or
  an integer number of bytes. `verify` and `lock_wait` are numbers, integer
  or float. `gc_every` is an integer.
- In `real_z3`, `dir`, `log` and `record`, a leading `~/` stands for `$HOME`.
- A key set to the empty string is unset, as an empty variable is.

Parsing uses the `toml` crate's parser alone (no serde): the document becomes
a table of spanned values, so each key's line is known. A number becomes the
decimal text its variable would hold, and from there a file value is checked
exactly as the variable is.

**Nothing in the configuration is an error** (rule 2). Each problem is a
warning, logged once the log is open (`doctor` and `gc` print them), and the
setting falls back to the next source:

- a config file that exists but cannot be read, is not a regular file (a
  FIFO or a terminal could block every solver start) or is larger than
  1 MiB is ignored as a whole;
- a file that is not UTF-8 or not valid TOML (including a key given twice)
  is ignored as a whole, with one warning giving the parser's line, column
  and message;
- an unknown key, a table or array where a value is expected, a value of
  the wrong type, or a path starting with `~/` when `HOME` is unset is
  ignored, with the file name, key and line;
- an invalid value is checked by the same parser as the variable: an invalid
  variable falls back to the file's value, and an invalid file value to the
  default.

`z3cache doctor` shows the config file and whether it was found (or why it
was ignored), then each setting's effective value and its source:
environment, config file (with the key's line), or default.

Modes:

- `rw`: read and write.
- `ro`: read only; misses are solved but not stored. Nothing in the store
  changes, so these sessions do not appear in `stats`.
- `enforce`: read only, and a miss prints an `(error …)` line and exits
  non-zero without starting a solver.
- `off`: run the real Z3 directly.

## 12. Command-line tools

- `z3cache setup [--shell zsh|bash|fish] [--yes|--print] [--undo]`: makes
  Verus run z3cache by setting `VERUS_Z3_PATH` in the user's shell startup
  file. It reports the `verus` on `PATH` (its directory, the version from
  `verus --version`, given at most 5 seconds and killed with its process
  group after that, and whether a `z3` is next to it; without one, that
  rule 2 of section 4 still finds Verus's `z3`), the z3cache binary
  (`current_exe`, canonical), and the shell.
  - **Shell:** `--shell`, else the parent process's executable if it is
    zsh, bash or fish (the shell setup was typed in; a leading `-` is
    ignored), else the file name of `$SHELL`. Setup says which and how.
  - **File:** zsh `~/.zshenv`, which every zsh reads, so scripts and agents
    get the setting too (`~/.zshrc` covers interactive shells only); bash
    `~/.bashrc`, with a note on macOS that login shells read
    `~/.bash_profile`; fish `${XDG_CONFIG_HOME:-~/.config}/fish/config.fish`.
    Any other shell gets the POSIX line to add by hand.
  - **Line:** `export VERUS_Z3_PATH="$HOME/<path>"` (fish:
    `set -gx VERUS_Z3_PATH "$HOME/<path>"`), with `$HOME` when the binary
    is under the home directory and the absolute path otherwise.
  - **Block:** setup writes the line between `# >>> z3cache >>>` and
    `# <<< z3cache <<<` and never changes anything outside the markers. A
    block with the same line, or the same line anywhere in the file, means
    setup is done and nothing changes; so does `VERUS_Z3_PATH` already
    pointing at this binary. A block with another line is shown with its
    replacement and replaced only when agreed. Markers that do not form one
    block are left alone, and setup asks the user to fix them.
  - **Writing:** only when stdin and stdout are terminals and the user
    answers yes (the default) to the prompt, or with `--yes`. Otherwise, and
    with `--print`, setup prints a ready-to-paste `echo '<line>' >> <file>`
    instead (single quotes, so that `$HOME` is expanded when the shell
    starts). A missing file (and its directory) is created; an existing one
    is first copied to `<file>.z3cache-backup` unless that backup exists. A
    new block is appended, so the rest of the file is untouched; a block
    replaced or removed is written to a temporary file next to the target
    and renamed over it, keeping the permissions, and through a symlink the
    file it points at is replaced.
  - **Check:** once the setting is in place, setup runs `doctor`'s cold and
    warm query (below) with the solver found as in section 4, and reports
    one line. It ends with the next steps: a new terminal (or `source` the
    file), Verus as usual, `z3cache stats`.
  - `--undo` removes the block from the same file, after asking in a
    terminal unless `--yes`; without a terminal it only shows the block. No
    block is not an error.
- `z3cache stats [--all|--json]`: for today, the last 7 days and all time,
  the hit rate (hits over hits plus misses), hits, misses, solver time saved,
  sessions, solver starts and entries stored, with the entry count and size.
  Lock, audit and eviction counts are shown when not zero, and problems
  (replay mismatches, sessions with solver errors, audit contradictions,
  bypassed sessions with their reasons) under "needs attention". `--all`
  shows every row; `--json` gives every counter.
- `z3cache gc`: evict to the size cap and remove idle lock files, then report
  what was done.
- `z3cache clear`: delete all entries.
- `z3cache doctor`: show the config file and every setting with its source
  (section 11), locate the real Z3 and how it was found (section 4), show its
  identity, check the cache directory, and run one small query cold and
  warm. The query runs against a temporary cache and without the config
  file.
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
- **Reads after declarations.** A read is answered from the cache only right
  after its query: every silent command discards the result in the proxy,
  though Z3 keeps it across declarations. Verus reads only right after a
  query.
- **Reads are not replayed.** A read answered from the cache is not sent to
  a solver started later. Z3's `get-model` completes the model, so under
  the `all` policy a later `eval` (which bypasses) can print a value that Z3
  would have printed completed.
- **Option values.** An allowlisted option is cached whatever its value. If
  Z3 rejects a value (it reports the error and keeps the old one) and a
  later `set-option` overwrites it, the key holds only the later value, so
  a warm run can leave out Z3's error.
- **Push counts.** A `push` count is tracked as given: a count in the
  billions makes the proxy allocate frames until memory runs out, where Z3
  would reject it.
- **Solver identity memo.** The memo of a binary's hash trusts its path,
  size and modification time: a binary replaced by one of the same size
  with the modification time kept reuses the old identity.
- **Parent lookup.** Rule 2 of section 4 trusts the directory of any parent
  process: a program that is not Verus but has a `z3` beside it makes
  z3cache use that `z3`.
- **Files that are not regular.** A log, recording or lock file that is a
  FIFO blocks the session that opens it (only the config file is checked).
- **Setup races.** Setup reads the startup file before asking and writes a
  changed copy after the answer, so an edit made in between is lost; the
  backup is made once, and holds the file as it was before the first
  change.
- **`ZDOTDIR`.** Setup uses `~/.zshenv` even when `ZDOTDIR` is set, in which
  case zsh reads `$ZDOTDIR/.zshenv` and does not see the setting.
- **Self-test.** The cold and warm query of `doctor` and `setup` keeps the
  user's other settings: with `Z3CACHE_VERIFY` set, the warm run starts an
  audit solver while it is reported as answered from the cache. Its runs
  have no time limit, so a solver that never answers hangs them.

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
| A8 | Only space, tab, line feed and carriage return separate tokens; other ASCII control bytes and DEL are errors outside literals and comments; inside string literals and quoted symbols they are kept, except NUL | Z3 4.16.0, every byte 0x00-0x1f and 0x7f in five positions: differential test `a8_control_bytes_are_read_as_z3_reads_them` |
| A9 | `push`, `pop` (also `(pop 0)`) and `assert` discard the latest query's result; declarations, `(push 0)`, `echo`, reads, statistics and the allowlisted options do not | Z3 4.16.0, differential test `a9_what_discards_a_result` |
| A10 | One Z3 process's own `rlimit-count` never decreases, across passing, failing and resource-limited queries, `pop`, and changes to `rlimit` | Z3 4.16.0, differential test `a10_rlimit_count_never_decreases` |

## 17. Solver profiles

The splitter, frames, key, replay and store work for any SMT-LIB solver
driven over a pipe. Everything that depends on the solver goes through a
`SolverProfile`. Only a Z3 profile exists. A profile decides:

| Concern | Z3 |
|---|---|
| Arguments that select proxy mode | `-smt2 -in` |
| Bytes that separate tokens, and bytes it rejects | Section 5 |
| `echo` output | Unquoted |
| Statistics | `rlimit-count`, synthesized (section 8) |
| Resource limit | `set-option :rlimit`, deterministic |
| Option allowlist | Section 5 |
| Commands that discard the latest result | `(push)` `(pop)` (section 7) |
| Wall-clock options (never stored except `unsat`) | Section 7 |

Another solver would need its own answers to each row. For example, cvc5
quotes `echo` output, and Verus runs it under a wall-clock timeout, so only
`unsat` could safely be stored for it. `solver_id` keeps different solvers'
entries apart in one store.

## 18. Testing

1. **Unit:** the command splitter (strings, quoted symbols, comments, commands
   split across reads), state hashing, classification, statistics, the store,
   the config file parser and the precedence of settings.
   Property tests run fixed seeds: generated commands, written with random
   whitespace and comments, split the same at any chunk boundaries; raw bytes
   and the remainder reproduce the input; the canonical form is a fixed point
   and does not depend on formatting; a balanced push…pop leaves the key
   unchanged; changing any command still on the path changes the key, and
   changing one in a popped frame does not.
2. **Stub solver** (`examples/stub_solver.rs`): a scripted stand-in for Z3
   that answers from a script and logs its launches. Covers lazy start,
   replay order, `pop` handling, bypass, policies, modes, the config file,
   and "a warm run starts no solver". Copied to a file named `rust_verify`,
   the stub runs its arguments as a command, standing in for Verus's driver
   as the parent process that section 4's lookup looks next to (a copy,
   because the kernel reports a symlinked executable by its target, and a
   script's process is its interpreter). `z3cache setup` runs against a
   temporary `HOME` with a fake `verus` on `PATH`; a test-only variable,
   `Z3CACHE_ASSUME_TTY=1`, lets tests answer its prompt on a pipe.
3. **Differential:** a corpus of transcripts run through real Z3, the proxy
   cold and the proxy warm, under each store policy and solver lifetime.
   Output and exit status must match apart from statistics and error
   positions. The corpus includes sessions recorded from the end-to-end
   sample crates (`verus_*.smt2`), so real Verus traffic is covered without
   Verus, and the inputs of past review findings (form feed, reads after a
   pop, `eval` completion, `set-info`). The assumptions of section 16 that
   Z3 can show are tests here.
   **Randomized** (`tests/random.rs`): a few hundred sessions generated from
   fixed seeds (declarations, an uninterpreted function, nested push/pop,
   linear arithmetic and boolean assertions, quantifiers, `check-sat`,
   `check-sat-assuming`, `get-value`, `get-unsat-core`, `echo`, allowlisted
   options), in families that share a prefix, with one cache per
   configuration shared by all sessions. The generator avoids what solver
   history may legitimately change (section 13): checks are made inside a
   frame, as Verus makes them; no check runs near its resource limit;
   `get-value` is asked only where the model is unique; `:reason-unknown`
   is compared only after `unknown`, or where there is no result to give a
   reason for (it is also asked at random points).
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
     recorded runs;
   - only `VERUS_Z3_PATH` set, with no `real_z3` and no `z3` or `verus` on
     `PATH`: z3cache finds the `z3` next to Verus's driver.

   Each sample module is written so that Verus emits the same commands on
   every run: Verus 0.2026.10.26 orders the accessors of named enum fields
   that differ between variants differently from run to run, which changes
   the module's keys.

6. **Proofs** (`tests/proofs.rs`): Verus checks `src/rlimit/counter.rs`,
   the proof of rule 7 (section 8), with the Z3 next to it.

Tiers 3, 5 and 6 need a real Z3 or Verus and skip with a message when none is
configured. Every test runs z3cache with `Z3CACHE_CONFIG` set to a path that
does not exist, unless it writes its own config file, so the user's config
file is never read, and with `Z3CACHE_REAL_Z3` set (to the stub unless a
test says otherwise), so that only tests of the lookup itself depend on it.

## 19. Prior art

| Source | What z3cache takes |
|---|---|
| ccache | Hash the compiler binary; masquerade under the tool's name; never fail the build; `stats`; size-capped LRU |
| sccache | Do not cache failures by default; storage behind a trait |
| `cranelift_isle_veri_caching` (Wasmtime) | A stack of push/pop frames, key = solver + live path + command, no solver until a miss, replay the path on a miss, an enforcing read-only mode |
| cargo-verus | Caches per crate through Cargo's freshness check, so it neither shares across worktrees nor survives a one-function edit |

The Wasmtime crate cannot be used directly: it replaces the `easy_smt` library
in-process, while Verus talks to Z3 through its own pipe code.
