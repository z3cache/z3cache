# Security policy

z3cache sits between Verus and Z3, so a bug in it can make Verus accept a
proof that Z3 would reject. Treat such bugs as security issues.

## What to report privately

- z3cache answers a query differently from a real Z3 given the same input,
  above all a wrong `unsat`, which makes a failing proof pass.
- A way for one user or process to plant or alter answers in another user's
  cache.
- Anything that lets z3cache write to stdout what Z3 would not, or crash or
  hang Verus.

Ordinary bugs (slow runs, unexpected misses, confusing messages) can go in
public issues.

## How to report

Use **Report a vulnerability** on the repository's Security tab
(<https://github.com/z3cache/z3cache/security/advisories/new>). Please include
the z3cache, Verus and Z3 versions, the output of `z3cache doctor`, and if you
can, a recording made with `Z3CACHE_RECORD=<dir>` or a solver transcript that
reproduces the problem.

You can expect an acknowledgement within a week. Fixes go into the next
release, and the advisory is published once a fixed version is out.

## Supported versions

Only the latest release receives fixes.
