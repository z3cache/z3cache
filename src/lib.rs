//! z3cache: a drop-in replacement for the `z3` binary that caches solver
//! answers. See `docs/DESIGN.md`.

pub mod analyze;
pub mod cli;
pub mod command;
pub mod config;
pub mod lock;
pub mod log;
pub mod profile;
pub mod record;
pub mod rlimit;
pub mod session;
pub mod sexp;
pub mod solver;
pub mod state;
pub mod store;

/// Bump on any change to canonicalisation, command classification or key
/// layout. Old cache entries then become unreachable.
pub const FORMAT_VERSION: u32 = 4;
