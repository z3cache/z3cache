# z3cache

[![CI](https://github.com/z3cache/z3cache/actions/workflows/ci.yml/badge.svg)](https://github.com/z3cache/z3cache/actions/workflows/ci.yml)

z3cache makes [Verus](https://github.com/verus-lang/verus) skip proofs it has
already checked. It is a drop-in replacement for the `z3` binary that Verus
runs: answers to solver queries are cached, so a proof that passed once is not
proved again, in the next run or in another git worktree of the same project.

```
verus ──▶ z3cache ──(only on a cache miss)──▶ z3
              │
              ▼
     one shared cache on disk
```

On a large Verus project, a full verification run takes 134 s without the
cache and 28 s with it. When several worktrees verify the same code at once,
each query is solved only once: the other worktrees wait for the answer and
reuse it.

## How it works

Verus starts one Z3 process per module and talks to it through a pipe.
z3cache sits on that pipe and follows the commands Verus sends. For each
`check-sat` it builds a key from everything the solver has seen (the Z3
binary, the options, and every declaration and assertion in scope) and looks
it up:

- **Hit:** the answer comes from the cache. A module whose queries are all
  cached never starts Z3.
- **Miss:** z3cache starts Z3, sends it everything it has not seen yet, passes
  the answer back to Verus, and stores it.

By default only `unsat` answers, meaning proofs that succeed, are stored, so
failing proofs are always checked again. The cache is a single SQLite file
shared by every worktree and every Verus process on the machine.

## Quick start

You need macOS or Linux, a recent stable Rust toolchain, and Verus. Verus
releases include their own `z3` next to the `verus` binary; z3cache uses that
one as the real solver.

```sh
# Install
cargo install --git https://github.com/z3cache/z3cache

# Point Verus at z3cache, and z3cache at the z3 that ships with Verus
export VERUS_Z3_PATH="$HOME/.cargo/bin/z3cache"
export Z3CACHE_REAL_Z3="/path/to/verus/z3"

# Check the setup: finds Z3, checks the cache, runs one query cold and warm
z3cache doctor

# Use Verus as usual
verus src/lib.rs

# See what the cache did
z3cache stats
```

To use the cache in every worktree, put the two `export` lines in your shell
profile. To turn it off for one run, set `Z3CACHE_MODE=off`.

### Or: a one-line alias

If the Verus release directory is on your `PATH`, z3cache finds the `z3` next
to `verus` by itself, and one alias in `~/.zshrc` or `~/.bashrc` is enough:

```sh
alias verus='VERUS_Z3_PATH="$HOME/.cargo/bin/z3cache" verus'
```

Aliases only apply to commands you type in an interactive shell. If your
project runs Verus from a script, a Makefile or `cargo verus`, use the
`export` lines above instead. To bypass the alias for one run, type
`command verus`.

## Is it safe?

A cache hit means that this exact Z3 binary once gave this answer to this
exact sequence of commands. z3cache never invents an answer, never reports
`unsat` that a real Z3 did not produce for the same input, and never makes a
build fail or hang. If anything goes wrong inside z3cache, it falls back to
running Z3 normally.

Two things to keep in mind:

- **You trust the cache directory.** Anyone who can write to it can forge a
  proof, so keep it private to your user.
- **A proof that passed once stays passed.** If a proof is flaky, for example
  close to its resource limit, the cache keeps the run where it passed. Run
  merge gates, CI and release builds with `Z3CACHE_MODE=off`. To spot-check
  the cache, `Z3CACHE_VERIFY=0.05` re-checks 5% of hits against Z3.

## Configuration

Everything is set through environment variables, because the command line
belongs to Z3.

| Variable | What it does | Default |
|---|---|---|
| `Z3CACHE_REAL_Z3` | Path to the real Z3 | the first other `z3` on `PATH` |
| `Z3CACHE_DIR` | Cache directory | `~/Library/Caches/z3cache` (macOS), `~/.cache/z3cache` (Linux) |
| `Z3CACHE_MODE` | `rw` read and write; `ro` read only; `enforce` fail on any miss; `off` plain Z3 | `rw` |
| `Z3CACHE_STORE` | `unsat` stores only successful proofs; `all` also stores failures | `unsat` |
| `Z3CACHE_SOLVER` | `keep` one solver per session; `fresh` a clean solver for every miss | `keep` |
| `Z3CACHE_MAX_SIZE` | Size cap, such as `2G` or `500M` | `2G` |
| `Z3CACHE_VERIFY` | Fraction of hits to re-check against Z3 | `0` |
| `Z3CACHE_LOCK_WAIT` | Seconds to wait for another process solving the same query | `60` |
| `Z3CACHE_LOG` | File for diagnostics | none |
| `Z3CACHE_RECORD` | Directory to record sessions for `z3cache analyze` | none |

An invalid value is never fatal: z3cache logs a warning and uses the default.
Every variable is described in [docs/DESIGN.md](docs/DESIGN.md), section 11.

## Commands

| Command | What it does |
|---|---|
| `z3cache stats [--json]` | Hits, misses, solver time saved, cache size |
| `z3cache doctor` | Check the setup end to end |
| `z3cache gc` | Shrink the cache to `Z3CACHE_MAX_SIZE` (also runs automatically) |
| `z3cache clear` | Delete every cached answer |
| `z3cache analyze <dir>…` | Compare recorded runs and estimate what a cache would save |

Run with Z3's own arguments, or through a symlink named `z3`, the binary
behaves as Z3. Arguments it does not handle, such as `--version`, go straight
to the real Z3.

## Troubleshooting

- **No speedup:** run `z3cache stats`. Bypassed sessions list the command or
  option that z3cache does not cache; those sessions run plain Z3. Set
  `Z3CACHE_LOG` for details.
- **"no real z3 found on PATH":** set `Z3CACHE_REAL_Z3` to the `z3` that ships
  with your Verus.
- **Starting over:** run `z3cache clear`, or delete the cache directory. The
  cache only ever saves time, so nothing is lost.

## Limitations

- Z3 only, driven over a pipe the way Verus drives it.
- One machine: the cache is not shared over a network.
- Only solver time is saved; rustc and Verus's own passes still run.
- An edit early in a module can change the keys of the functions checked after
  it in the same module, even if their proofs did not change. On the large
  project above, this cost at most about 10% of solver time.

## Contributing

Read [AGENTS.md](AGENTS.md) before changing code: it lists the rules every
change must keep, the conventions, and how to build and test. It is written
for human contributors and coding agents alike.
[docs/DESIGN.md](docs/DESIGN.md) explains how z3cache works and why.

```sh
cargo build --release
cargo test                                                     # no Z3 needed
Z3CACHE_REAL_Z3=/path/to/z3 cargo test --test differential     # against real Z3
Z3CACHE_VERUS=/path/to/verus cargo test --test e2e             # with Verus
```

## License

[MIT](LICENSE)
