# z3cache

A drop-in replacement for the `z3` binary that caches solver answers, so that
[Verus](https://github.com/verus-lang/verus) proofs already checked once are not
proved again: not in the next run, and not in another git worktree of the same
project.

```
verus ──▶ z3cache ──(only on a cache miss)──▶ z3
              │
              ▼
     one shared cache on disk
```

On a large Verus project, a full verification run that took 134 s took 28 s
with a warm cache. Three worktrees of the same commit verified at the same
time solved each cacheable query only once between them.

## How it works

Verus talks to Z3 through a pipe, one solver process per module. z3cache sits
on that pipe. It follows the commands Verus sends and builds a key from
everything the solver has seen: the Z3 binary, its options and every
declaration and assertion still in scope. When Verus asks `check-sat`, z3cache
looks the key up:

- **Hit:** it answers from the cache. A module whose queries are all cached
  never starts Z3.
- **Miss:** it starts Z3 (once per session), sends it what it has not yet
  seen, passes the answer on, and stores it.

By default only `unsat` answers (proofs that succeed) are stored, so failing
proofs are always re-checked. The cache is one SQLite file shared by every
worktree and every concurrent Verus process on the machine.

## Quick start

Requirements: macOS (tested) or Linux (expected to work, not yet tested), a
recent stable Rust toolchain, and Verus. Verus
releases ship their own Z3 next to the `verus` binary; z3cache uses that one
as the real solver.

```sh
# Install
cargo install --path .

# Point Verus at z3cache, and z3cache at the real Z3
export VERUS_Z3_PATH="$HOME/.cargo/bin/z3cache"
export Z3CACHE_REAL_Z3="/path/to/verus-dir/z3"

# Check the setup: finds Z3, checks the cache, runs one query cold and warm
z3cache doctor

# Use Verus as usual
verus src/lib.rs

# See what the cache did
z3cache stats
```

Put the two `export` lines in your shell profile to use the cache in every
worktree. To turn it off for one run, set `Z3CACHE_MODE=off`.

## Is it safe?

A cache hit means: this exact Z3 binary once answered this exact sequence of
commands, and the answer was `unsat`. z3cache never infers an answer, never
reports `unsat` that a real Z3 did not produce for the same input, and never
fails or hangs a build because of the cache. When anything goes wrong inside
z3cache, it falls back to running Z3 normally.

Two things to keep in mind:

- **You trust the cache directory.** Anyone who can write to it can forge a
  proof. Keep it private to your user.
- **A proof that passed once stays passed.** If a proof is flaky (it passes
  only sometimes, for example near its resource limit), the cache keeps the
  passing answer. For merge gates, CI and releases, run Verus with
  `Z3CACHE_MODE=off`. `Z3CACHE_VERIFY=0.05` re-checks 5% of cache hits against
  Z3 to catch disagreements.

## Configuration

Everything is set by environment variable; the command line belongs to Z3.

| Variable | What it does | Default |
|---|---|---|
| `Z3CACHE_REAL_Z3` | Path to the real Z3 | the first other `z3` on `PATH` |
| `Z3CACHE_DIR` | Cache directory | `~/Library/Caches/z3cache` (macOS), `~/.cache/z3cache` (Linux) |
| `Z3CACHE_MODE` | `rw` read and write; `ro` read only; `enforce` fail on any miss; `off` plain Z3 | `rw` |
| `Z3CACHE_STORE` | `unsat` stores only successful proofs; `all` also stores failures | `unsat` |
| `Z3CACHE_SOLVER` | `keep` one solver per session; `fresh` a clean solver for every miss | `keep` |
| `Z3CACHE_MAX_SIZE` | Cache size cap, such as `2G` or `500M` | `2G` |
| `Z3CACHE_VERIFY` | Fraction of hits to re-check against Z3 | `0` |
| `Z3CACHE_LOCK_WAIT` | Seconds to wait for another process solving the same query | `60` |
| `Z3CACHE_LOG` | File for diagnostics | none |
| `Z3CACHE_RECORD` | Directory to record sessions for `z3cache analyze` | none |

An invalid value is never fatal: z3cache logs a warning and uses the default.
[docs/DESIGN.md](docs/DESIGN.md) section 11 lists every variable.

## Commands

| Command | What it does |
|---|---|
| `z3cache stats [--json]` | Hits, misses, solver time saved, cache size |
| `z3cache doctor` | Check the setup end to end |
| `z3cache gc` | Shrink the cache to `Z3CACHE_MAX_SIZE` (also runs automatically) |
| `z3cache clear` | Delete every cached answer |
| `z3cache analyze <dir>…` | Compare recorded runs and estimate what a cache would save |

When invoked with Z3's own arguments, or through a symlink named `z3`, the
binary behaves as Z3. Arguments it does not handle, such as `--version`, are
passed straight to the real Z3.

## Troubleshooting

- **No speedup:** run `z3cache stats`. If it shows bypassed sessions, the
  listed reason is a command or option z3cache does not cache; those sessions
  run plain Z3. Set `Z3CACHE_LOG` for details.
- **"no real z3 found on PATH":** set `Z3CACHE_REAL_Z3` to the `z3` that
  ships with your Verus.
- **Want a clean slate:** `z3cache clear`, or delete the cache directory. The
  cache is disposable.

## Limitations

- Z3 only, driven over a pipe the way Verus drives it.
- One machine: the cache is not shared over a network.
- Only solver time is saved. rustc and Verus's own passes still run.
- An edit early in a module can change the cache keys of the functions checked
  after it in the same module, even if their proofs did not change. On a
  large project this cost at most about 10% of solver time.

## For contributors and coding agents

- [AGENTS.md](AGENTS.md): the rules every change must keep, conventions, and
  how to build and test. Read it before changing code.
- [docs/DESIGN.md](docs/DESIGN.md): how z3cache works, what it guarantees, and
  why.

```sh
cargo build --release
cargo test                                                     # no Z3 needed
Z3CACHE_REAL_Z3=/path/to/z3 cargo test --test differential     # against real Z3
Z3CACHE_VERUS=/path/to/verus cargo test --test e2e             # with Verus
```

## License

[MIT](LICENSE)
