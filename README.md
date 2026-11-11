# z3cache

[![CI](https://github.com/z3cache/z3cache/actions/workflows/ci.yml/badge.svg)](https://github.com/z3cache/z3cache/actions/workflows/ci.yml)

**Prove it once, reuse it in every worktree.** z3cache makes
[Verus](https://github.com/verus-lang/verus) skip proofs it has already
checked. It is a drop-in replacement for the `z3` binary that Verus runs:
answers to solver queries go into one cache shared by every git worktree and
every Verus process on your machine.

```
worktree A ─┐
worktree B ─┼─ verus ──▶ z3cache ──(only on a cache miss)──▶ z3
worktree C ─┘                │
                             ▼
                   one shared cache on disk
```

## Why it helps with many worktrees

If you keep several worktrees of one Verus project (for feature branches,
reviews, or coding agents working in parallel), each of them normally proves
everything from scratch. With z3cache, measured on a large Verus project:

- **A new worktree starts warm.** Every proof another worktree has already
  checked comes from the cache, so a fresh checkout of `main` does not re-prove
  `main`.
- **A branch re-proves only what changed.** A branch 22 commits ahead of
  `main` verified in 31 s instead of about 150 s: 99% of its solver time was
  answered from the cache.
- **Parallel worktrees share the work.** When several worktrees verify the
  same code at once, each query is solved by only one of them; the others
  wait for the answer and reuse it.
- **Re-running in one checkout is fast too.** A full verification run took
  28 s with a warm cache instead of 134 s.

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

Install z3cache:

```sh
cargo install z3cache
```

Tell z3cache where the real Z3 is, in `~/.config/z3cache/config.toml`:

```toml
real_z3 = "/path/to/verus/z3"
```

Then point Verus at z3cache and check the setup:

```sh
export VERUS_Z3_PATH="$HOME/.cargo/bin/z3cache"

# Check the setup: finds Z3, checks the cache, runs one query cold and warm
z3cache doctor

# Use Verus as usual
verus src/lib.rs

# See what the cache did
z3cache stats
```

To use the cache in every worktree, put the `export` line in your shell
profile. To turn it off for one run, set `Z3CACHE_MODE=off`.

### Or: a one-line alias

With `real_z3` in the config file, or the Verus release directory on your
`PATH` (z3cache then finds the `z3` next to `verus` by itself), one alias in
`~/.zshrc` or `~/.bashrc` can replace the `export` line:

```sh
alias verus='VERUS_Z3_PATH="$HOME/.cargo/bin/z3cache" verus'
```

Aliases only apply to commands you type in an interactive shell. If your
project runs Verus from a script, a Makefile or `cargo verus`, use the
`export` line above instead. To bypass the alias for one run, type
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

If you find z3cache giving an answer a real Z3 would not, please report it
privately as described in [SECURITY.md](SECURITY.md).

## Configuration

Settings live in `~/.config/z3cache/config.toml` (on macOS too), a
[TOML](https://toml.io) file:

```toml
# ~/.config/z3cache/config.toml
real_z3 = "~/verus/z3"
dir = "~/caches/z3cache"
max_size = "2G"
verify = 0.05
```

Text values are quoted strings, and a path may start with `~/`. `verify`,
`lock_wait` and `gc_every` are numbers; `max_size` is a size string such as
`"500M"` or a number of bytes. The file is
`$XDG_CONFIG_HOME/z3cache/config.toml` if you set `XDG_CONFIG_HOME`, and
`Z3CACHE_CONFIG` names another file. Without a file, every setting has its
default.

Each setting can also be given as an environment variable, which overrides
the file: `Z3CACHE_` followed by the key in upper case, such as
`Z3CACHE_MODE=off` for one run. There are no command-line flags, because the
command line belongs to Z3.

| Key | Variable | What it does | Default |
|---|---|---|---|
| `real_z3` | `Z3CACHE_REAL_Z3` | Path to the real Z3 | the first other `z3` on `PATH` |
| `dir` | `Z3CACHE_DIR` | Cache directory | `~/Library/Caches/z3cache` (macOS), `~/.cache/z3cache` (Linux) |
| `mode` | `Z3CACHE_MODE` | `rw` read and write; `ro` read only; `enforce` fail on any miss; `off` plain Z3 | `rw` |
| `store` | `Z3CACHE_STORE` | `unsat` stores only successful proofs; `all` also stores failures | `unsat` |
| `solver` | `Z3CACHE_SOLVER` | `keep` one solver per session; `fresh` a clean solver for every miss | `keep` |
| `max_size` | `Z3CACHE_MAX_SIZE` | Size cap, such as `2G` or `500M` | `2G` |
| `verify` | `Z3CACHE_VERIFY` | Fraction of hits to re-check against Z3 | `0` |
| `gc_every` | `Z3CACHE_GC_EVERY` | One session in this many checks the cache size at exit; `0` never | `100` |
| `lock_wait` | `Z3CACHE_LOCK_WAIT` | Seconds to wait for another process solving the same query | `60` |
| `log` | `Z3CACHE_LOG` | File for diagnostics | none |
| `record` | `Z3CACHE_RECORD` | Directory to record sessions for `z3cache analyze` | none |

A mistake in the file is never fatal: z3cache logs a warning and carries
on. A file that is not valid TOML is ignored as a whole; an unknown key or a
bad value is ignored on its own, and that setting has its default. An invalid
environment variable falls back to the file's value, then the default.
`z3cache doctor` shows the warnings, and each setting's value and where it
came from. Every setting is described in
[docs/DESIGN.md](docs/DESIGN.md), section 11.

## Commands

| Command | What it does |
|---|---|
| `z3cache stats [--all\|--json]` | Hit rate, hits, misses, solver time saved, cache size, and anything that needs attention |
| `z3cache doctor` | Check the setup end to end |
| `z3cache gc` | Shrink the cache to `max_size` (also runs automatically) |
| `z3cache clear` | Delete every cached answer |
| `z3cache analyze <dir>…` | Compare recorded runs and estimate what a cache would save |

Run with Z3's own arguments, or through a symlink named `z3`, the binary
behaves as Z3. Arguments it does not handle, such as `--version`, go straight
to the real Z3.

## Troubleshooting

- **No speedup:** run `z3cache stats` and look at the hit rate. If sessions
  were bypassed, they are listed under "needs attention" with the command or
  option that z3cache does not cache; those sessions run plain Z3. Set `log`
  in the config file for details.
- **"no real z3 found on PATH":** set `real_z3` in the config file to the `z3`
  that ships with your Verus.
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
