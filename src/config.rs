//! Configuration from environment variables (DESIGN section 11). An invalid
//! value is logged and replaced by the default, never an error.

use std::env;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::profile::SolverProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Rw,
    Ro,
    Enforce,
    Off,
}

/// `Z3CACHE_STORE` (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePolicy {
    Unsat,
    All,
}

/// `Z3CACHE_SOLVER` (section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    Keep,
    Fresh,
}

pub const DEFAULT_MAX_SIZE: u64 = 2 << 30;

/// How long a miss waits for another session's in-flight lock, by default.
/// A holder normally takes as long as the query itself: the slowest query in
/// a large Verus project took 33 s, and Verus's resource limits keep
/// queries in that range. Twice that covers it; a hung or stopped holder
/// then costs a waiting session one minute, after which it solves the key
/// itself (duplicate work, never a wrong answer).
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(60);

/// One session in this many runs eviction at exit, by default.
pub const DEFAULT_GC_EVERY: u32 = 100;

#[derive(Debug, Clone)]
pub struct Config {
    pub real_z3: Option<PathBuf>,
    pub mode: Mode,
    /// Cache directory; `None` if no default could be determined.
    pub dir: Option<PathBuf>,
    pub record: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub solver_salt: Vec<u8>,
    pub store_policy: StorePolicy,
    pub lifetime: Lifetime,
    /// `Z3CACHE_MAX_SIZE`, in bytes.
    pub max_size: u64,
    /// `Z3CACHE_VERIFY`: the fraction of query hits to audit.
    pub verify: f64,
    /// `Z3CACHE_GC_EVERY`: one session in this many checks the size at exit;
    /// 0 never does.
    pub gc_every: u32,
    /// `Z3CACHE_LOCK_WAIT`: the longest wait for an in-flight lock.
    pub lock_wait: Duration,
    /// Problems found while reading the environment, to be logged once the
    /// log is open. They never stop the program.
    pub warnings: Vec<String>,
}

fn non_empty(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Config {
        let mut warnings = Vec::new();
        let mode = match non_empty("Z3CACHE_MODE").as_deref().and_then(|v| v.to_str()) {
            None | Some("rw") => Mode::Rw,
            Some("ro") => Mode::Ro,
            Some("enforce") => Mode::Enforce,
            Some("off") => Mode::Off,
            Some(other) => {
                warnings.push(format!("unknown Z3CACHE_MODE {other:?}; using rw"));
                Mode::Rw
            }
        };
        let text = |name: &str| non_empty(name).map(|v| v.to_string_lossy().into_owned());
        let store_policy = match text("Z3CACHE_STORE").as_deref() {
            None | Some("unsat") => StorePolicy::Unsat,
            Some("all") => StorePolicy::All,
            Some(other) => {
                warnings.push(format!("unknown Z3CACHE_STORE {other:?}; storing unsat only"));
                StorePolicy::Unsat
            }
        };
        let lifetime = match text("Z3CACHE_SOLVER").as_deref() {
            None | Some("keep") => Lifetime::Keep,
            Some("fresh") => Lifetime::Fresh,
            Some(other) => {
                warnings.push(format!("unknown Z3CACHE_SOLVER {other:?}; using keep"));
                Lifetime::Keep
            }
        };
        let max_size = match text("Z3CACHE_MAX_SIZE") {
            None => DEFAULT_MAX_SIZE,
            Some(v) => parse_size(&v).unwrap_or_else(|| {
                warnings.push(format!("invalid Z3CACHE_MAX_SIZE {v:?}; using 2G"));
                DEFAULT_MAX_SIZE
            }),
        };
        let verify = match text("Z3CACHE_VERIFY") {
            None => 0.0,
            Some(v) => parse_fraction(&v).unwrap_or_else(|| {
                warnings.push(format!("invalid Z3CACHE_VERIFY {v:?}; auditing nothing"));
                0.0
            }),
        };
        let lock_wait = match text("Z3CACHE_LOCK_WAIT") {
            None => DEFAULT_LOCK_WAIT,
            Some(v) => parse_seconds(&v).unwrap_or_else(|| {
                warnings.push(format!("invalid Z3CACHE_LOCK_WAIT {v:?}; using 60 seconds"));
                DEFAULT_LOCK_WAIT
            }),
        };
        let gc_every = match text("Z3CACHE_GC_EVERY") {
            None => DEFAULT_GC_EVERY,
            Some(v) => parse_count(&v).unwrap_or_else(|| {
                warnings.push(format!("invalid Z3CACHE_GC_EVERY {v:?}; using 100"));
                DEFAULT_GC_EVERY
            }),
        };
        Config {
            real_z3: non_empty("Z3CACHE_REAL_Z3").map(PathBuf::from),
            mode,
            dir: non_empty("Z3CACHE_DIR").map(PathBuf::from).or_else(default_dir),
            record: non_empty("Z3CACHE_RECORD").map(PathBuf::from),
            log: non_empty("Z3CACHE_LOG").map(PathBuf::from),
            solver_salt: env::var_os("Z3CACHE_SOLVER_SALT")
                .map(|v| v.into_encoded_bytes())
                .unwrap_or_default(),
            store_policy,
            lifetime,
            max_size,
            verify,
            gc_every,
            lock_wait,
            warnings,
        }
    }
}

// Numeric settings are parsed by these functions, which reject rather than
// panic on anything out of range (rule 2): the caller logs a warning and
// uses the default.

/// A non-negative number of seconds, fractions allowed. Rejects NaN,
/// infinities, negatives and values too large for a `Duration`.
pub fn parse_seconds(text: &str) -> Option<Duration> {
    let secs: f64 = text.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(secs).ok()
}

/// A fraction in [0, 1]. Rejects NaN and anything outside the range.
pub fn parse_fraction(text: &str) -> Option<f64> {
    let p: f64 = text.trim().parse().ok()?;
    (0.0..=1.0).contains(&p).then_some(p)
}

/// A non-negative whole number that fits in 32 bits.
pub fn parse_count(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// Parses a size such as `2G`, `500M`, `1024K` or `123456` (bytes). Units
/// are binary (K = 1024) and case-insensitive, optionally followed by `B` or
/// `iB`. Zero and overflow are invalid.
pub fn parse_size(text: &str) -> Option<u64> {
    let t = text.trim();
    let upper = t.to_ascii_uppercase();
    let unit_start = upper.find(|c: char| !c.is_ascii_digit()).unwrap_or(upper.len());
    let (digits, unit) = upper.split_at(unit_start);
    let n: u64 = digits.parse().ok()?;
    let shift = match unit {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 10,
        "M" | "MB" | "MIB" => 20,
        "G" | "GB" | "GIB" => 30,
        "T" | "TB" | "TIB" => 40,
        _ => return None,
    };
    let size = n.checked_mul(1u64 << shift)?;
    (size > 0).then_some(size)
}

/// `~/Library/Caches/z3cache` on macOS, `${XDG_CACHE_HOME:-~/.cache}/z3cache`
/// elsewhere.
fn default_dir() -> Option<PathBuf> {
    let home = non_empty("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches/z3cache"));
    }
    non_empty("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|d| d.join("z3cache"))
}

/// Finds the real solver: `Z3CACHE_REAL_Z3`, or else the first executable
/// with the profile's binary name on `PATH` that is not this program.
pub fn find_real_solver(cfg: &Config, profile: &dyn SolverProfile) -> Result<PathBuf, String> {
    let me = env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| (m.dev(), m.ino()))
        .ok();
    let is_me = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| Some((m.dev(), m.ino())) == me)
            .unwrap_or(false)
    };
    if let Some(p) = &cfg.real_z3 {
        if !is_executable(p) {
            return Err(format!("Z3CACHE_REAL_Z3={} is not an executable file", p.display()));
        }
        if is_me(p) {
            return Err(format!("Z3CACHE_REAL_Z3={} points at z3cache itself", p.display()));
        }
        return Ok(p.clone());
    }
    let path = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path) {
        let candidate = dir.join(profile.binary_name());
        if is_executable(&candidate) && !is_me(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "no real {} found on PATH; set Z3CACHE_REAL_Z3",
        profile.binary_name()
    ))
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds() {
        assert_eq!(parse_seconds("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse_seconds("0"), Some(Duration::ZERO));
        assert_eq!(parse_seconds("0.5"), Some(Duration::from_millis(500)));
        assert_eq!(parse_seconds(" 2.25 "), Some(Duration::from_millis(2250)));
        for bad in ["1e100", "1e20", "NaN", "nan", "inf", "-inf", "infinity", "-1", "-0.5", "", "abc", "1s"] {
            assert_eq!(parse_seconds(bad), None, "{bad}");
        }
    }

    #[test]
    fn fractions() {
        assert_eq!(parse_fraction("0"), Some(0.0));
        assert_eq!(parse_fraction("1"), Some(1.0));
        assert_eq!(parse_fraction("0.01"), Some(0.01));
        for bad in ["1.0001", "-0.1", "NaN", "inf", "-inf", "1e100", "", "half"] {
            assert_eq!(parse_fraction(bad), None, "{bad}");
        }
    }

    #[test]
    fn counts() {
        assert_eq!(parse_count("0"), Some(0));
        assert_eq!(parse_count("100"), Some(100));
        assert_eq!(parse_count("4294967295"), Some(u32::MAX));
        for bad in ["4294967296", "1e3", "-1", "1.5", "NaN", "", "x"] {
            assert_eq!(parse_count(bad), None, "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2G"), Some(2 << 30));
        assert_eq!(parse_size("500M"), Some(500 << 20));
        assert_eq!(parse_size("1024K"), Some(1 << 20));
        assert_eq!(parse_size("1024k"), Some(1 << 20));
        assert_eq!(parse_size("2GiB"), Some(2 << 30));
        assert_eq!(parse_size("123456"), Some(123456));
        assert_eq!(parse_size(" 7MB "), Some(7 << 20));
        for bad in ["", "G", "0", "-1", "1.5G", "2X", "99999999999999999999T"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }
}
