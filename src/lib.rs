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

/// Time since the Unix epoch; zero if the clock is set before it.
pub fn since_epoch() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// A fresh, empty path for a unit test's files: `z3cache-<kind>-<pid>-<name>`
/// in the temporary directory.
#[cfg(test)]
pub(crate) fn test_dir(kind: &str, name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("z3cache-{kind}-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A small deterministic generator for property tests (SplitMix64). Tests
/// that use it run fixed seeds and name the failing one.
#[cfg(test)]
pub(crate) struct TestRng(u64);

#[cfg(test)]
impl TestRng {
    pub(crate) fn new(seed: u64) -> TestRng {
        TestRng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub(crate) fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}
