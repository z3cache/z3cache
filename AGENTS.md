# z3cache

A drop-in replacement for the `z3` binary that caches solver answers, so that
Verus proofs already checked in one git worktree are not re-proved in another.

The design is in [docs/DESIGN.md](docs/DESIGN.md); read it before changing
anything. Section numbers below refer to it.

## Rules that must hold

These come from how Verus consumes solver output (section 2) and from the
soundness argument (section 13). A change that breaks one is a bug even if the
tests pass.

1. **stdout is the solver's voice.** Write only bytes a real Z3 could have
   produced. Verus treats any unrecognised line as an error and panics on more
   than two consecutive empty lines. Diagnostics go to the `Z3CACHE_LOG` file.
2. **Never fail or hang the build.** Any internal error degrades to a slower
   correct path: store disabled, or bypass. Only `enforce` mode may exit
   non-zero on its own. Never wait for solver output whose format a command
   outside the tables could have changed. Never write to the client while
   holding an in-flight lock, and never wait for a lock without a bound
   (section 10).
3. **Unknown means bypass.** A command or option that is not in the class
   tables or the profile's option allowlist is never guessed at. Add it
   deliberately, with a test. An option is allowlisted only if it is plain
   configuration: keyed by value, no interaction with `pop`, no output or file
   side effects.
4. **A hit is an exact replay.** The key covers the solver binary, arguments,
   options and every command on the live path, with the answers given. Do not
   drop anything from the key to raise the hit rate.
5. **Bump `FORMAT_VERSION`** on any change to canonicalisation, command
   classification or key layout.
6. **Nothing is stored from a session after the solver reports an error.**
7. **`rlimit-count` given to the client never decreases** (section 8). The
   one exception is a raw bypass forced by a command or option outside the
   tables, where framing could hang the session; rule 2 comes first.
8. **`unsat` is the only verdict stored by default.**
9. **Solver-specific behaviour lives in the solver profile** (section 17), not
   in the engine.
10. **Never report a hit that did not happen.** Counters, `stats`, logs,
    records and `analyze` count a response as cached only if it was written
    from a stored entry without consulting a solver. Misses, bypassed or `off`
    sessions, replayed queries and audited answers are never hits, and
    projections are labelled as such. Under-reporting hits is acceptable;
    over-reporting is a bug.

## Conventions

- Keep dependencies to `blake3`, `rusqlite` (bundled SQLite) and `libc`; add
  another only for a strong reason.
- Every behaviour change comes with a test at the lowest level that can catch
  it: a unit test, then the stub solver (`examples/stub_solver.rs`, a scripted
  stand-in for Z3), then real Z3, then Verus. The stub is an example rather
  than a second binary so that `cargo install` installs one program.
- Tests that need a real Z3 or Verus skip with a visible message when none is
  configured.
- Tests synchronise on events (log lines, files, process state), not on
  sleeps.
- `docs/DESIGN.md` describes the current design. Update it in the same change
  as any design decision, and keep the solver assumptions in section 16 in
  step with the tests that check them.

## Verus reference

The protocol in section 2 comes from Verus's `source/air/src/`:

- `smt_process.rs`: spawning, framing, shutdown
- `smt_verify.rs`: the per-query command sequence, statistics
- `context.rs`: startup options, profiling
- `emitter.rs`: what reaches the pipe and what is log-only

Verus is not vendored. Re-read these files at the Verus version in use before
relying on a detail, and update section 2 if it differs.

## Commands

```
cargo build --release                 # target/release/z3cache
cargo test                            # unit, stub-solver and concurrency tests
Z3CACHE_REAL_Z3=/path/to/z3 cargo test --test differential   # against real Z3
Z3CACHE_VERUS=/path/to/verus cargo test --test e2e            # Verus end to end
```

`cargo test` builds the stub solver (`examples/stub_solver.rs`); a single
`cargo test --test proxy` does not, so run `cargo build --examples` first.
Tests use temporary cache directories and never touch the user's cache.

Use a local build with Verus:

```
VERUS_Z3_PATH=$PWD/target/release/z3cache Z3CACHE_REAL_Z3=/path/to/z3 verus …
z3cache stats            # what the cache answered
z3cache doctor           # check the setup
```

Verus distributions ship their own Z3 next to the `verus` binary; use that one
as `Z3CACHE_REAL_Z3`.

To measure what a cache would save, record each run into its own directory
with an empty cache, then compare the runs:

```
Z3CACHE_DIR=$(mktemp -d) Z3CACHE_RECORD=/tmp/rec/run1 VERUS_Z3_PATH=… verus …
z3cache analyze /tmp/rec/run1 /tmp/rec/run2 …
```
