//! Configuration (DESIGN section 11): each setting comes from its
//! environment variable if that is set and non-empty, else from the config
//! file, else its default. Nothing here is ever an error: an unreadable file,
//! a malformed line or an invalid value is a warning, and the setting falls
//! back to the next source.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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

/// The config file's keys, in the order `doctor` shows them, and whether
/// each is a path (a leading `~/` in the file expands to `$HOME`). Each key's
/// environment variable is `Z3CACHE_` followed by the key in upper case.
pub const KEYS: &[(&str, bool)] = &[
    ("real_z3", true),
    ("dir", true),
    ("mode", false),
    ("store", false),
    ("solver", false),
    ("max_size", false),
    ("verify", false),
    ("gc_every", false),
    ("lock_wait", false),
    ("log", true),
    ("record", true),
    ("solver_salt", false),
];

/// The environment variable for a config file key.
pub fn env_name(key: &str) -> String {
    format!("Z3CACHE_{}", key.to_ascii_uppercase())
}

/// A config file larger than this is ignored: it cannot be a hand-written
/// list of a dozen settings, and reading it must not slow every solver start.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// Where a setting's effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Env,
    /// The config file, at this line (from 1).
    File(usize),
    Default,
}

/// One setting's effective value, as `doctor` shows it.
#[derive(Debug, Clone)]
pub struct Setting {
    pub key: &'static str,
    pub value: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Found,
    Missing,
    /// It exists but could not be used; the reason is also a warning.
    Unreadable(String),
}

/// The config file: where it was looked for and what was found.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// `None` if no location could be determined (no `HOME`).
    pub path: Option<PathBuf>,
    pub status: FileStatus,
}

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
    pub file: ConfigFile,
    /// Every setting in [`KEYS`] order, with its source.
    pub settings: Vec<Setting>,
    /// Problems found while reading the configuration, to be logged once the
    /// log is open. They never stop the program.
    pub warnings: Vec<String>,
}

/// One `key = value` line of the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub value: OsString,
    pub line: usize,
}

/// Parses a config file: UTF-8 lines of `key = value`; blank lines and lines
/// starting with `#` are ignored; whitespace around the key and the value is
/// trimmed and a value wrapped in double quotes loses them. A leading `~/`
/// in a path value expands to `home`. Returns the entries by key (the last
/// of duplicates wins) and a message for each line with a problem.
pub fn parse_file(
    text: &[u8],
    home: Option<&Path>,
) -> (BTreeMap<&'static str, FileEntry>, Vec<(usize, String)>) {
    let mut entries: BTreeMap<&'static str, FileEntry> = BTreeMap::new();
    let mut problems = Vec::new();
    let text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    for (i, raw) in text.split(|&b| b == b'\n').enumerate() {
        let n = i + 1;
        let Ok(line) = std::str::from_utf8(raw) else {
            problems.push((n, "not UTF-8; line ignored".into()));
            continue;
        };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            problems.push((n, format!("expected `key = value`, got {line:?}; ignored")));
            continue;
        };
        let key = key.trim();
        let Some(&(key, is_path)) = KEYS.iter().find(|(k, _)| *k == key) else {
            problems.push((n, format!("unknown key {key:?}; ignored")));
            continue;
        };
        let mut value = value.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = &value[1..value.len() - 1];
        }
        let value = match value.strip_prefix("~/").filter(|_| is_path) {
            None => OsString::from(value),
            Some(rest) => match home {
                Some(h) => h.join(rest).into_os_string(),
                None => {
                    problems.push((
                        n,
                        format!("cannot expand ~ in {key}: HOME is unset; ignored"),
                    ));
                    continue;
                }
            },
        };
        if let Some(prev) = entries.get(key) {
            problems.push((
                n,
                format!(
                    "duplicate key {key:?} (also on line {}); this one wins",
                    prev.line
                ),
            ));
        }
        entries.insert(key, FileEntry { value, line: n });
    }
    (entries, problems)
}

/// How the config file is read: `Ok(None)` if it does not exist.
pub type ReadFile<'a> = dyn Fn(&Path) -> Result<Option<Vec<u8>>, String> + 'a;

/// Reads the config file. `Ok(None)` if it does not exist. Anything but a
/// regular file is refused without reading, so that a FIFO or a terminal
/// cannot block a solver start.
pub fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.to_string()),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    let mut text = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("larger than {} KiB", MAX_FILE_BYTES >> 10));
    }
    Ok(Some(text))
}

fn non_empty(v: Option<OsString>) -> Option<OsString> {
    v.filter(|v| !v.is_empty())
}

/// `$Z3CACHE_CONFIG`, else `${XDG_CONFIG_HOME:-~/.config}/z3cache/config`,
/// on every platform.
fn file_location(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(p) = non_empty(env("Z3CACHE_CONFIG")) {
        return Some(PathBuf::from(p));
    }
    non_empty(env("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| non_empty(env("HOME")).map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("z3cache/config"))
}

/// Looks settings up in the environment, then the file, then the default,
/// and records where each came from.
struct Resolver<'a> {
    env: &'a dyn Fn(&str) -> Option<OsString>,
    entries: BTreeMap<&'static str, FileEntry>,
    file_name: String,
    settings: Vec<Setting>,
    warnings: Vec<String>,
}

impl Resolver<'_> {
    /// The values to try, in order: the environment variable if non-empty,
    /// then the file's value if non-empty.
    fn candidates(&self, key: &str) -> Vec<(Source, OsString)> {
        let mut c = Vec::new();
        if let Some(v) = non_empty((self.env)(&env_name(key))) {
            c.push((Source::Env, v));
        }
        if let Some(e) = self.entries.get(key).filter(|e| !e.value.is_empty()) {
            c.push((Source::File(e.line), e.value.clone()));
        }
        c
    }

    /// The first candidate that `parse` accepts, or else `default`. Each
    /// rejected candidate is a warning: `word` (`invalid`, `unknown`), what
    /// was rejected, and what is used instead; `fallback` describes the
    /// default.
    fn get<T>(
        &mut self,
        key: &'static str,
        word: &str,
        parse: impl Fn(&OsStr) -> Option<T>,
        default: T,
        fallback: &str,
        show: impl Fn(&T) -> String,
    ) -> T {
        let mut rejected = Vec::new();
        let mut chosen = None;
        for (source, raw) in self.candidates(key) {
            match parse(&raw) {
                Some(v) => {
                    chosen = Some((source, v));
                    break;
                }
                None => {
                    let raw = raw.to_string_lossy();
                    rejected.push(match source {
                        Source::File(n) => {
                            format!("{word} {key} {raw:?} in {} line {n}", self.file_name)
                        }
                        _ => format!("{word} {} {raw:?}", env_name(key)),
                    });
                }
            }
        }
        let (source, value) = chosen.unwrap_or((Source::Default, default));
        let instead = match source {
            Source::File(n) => format!("using {key} from {} line {n}", self.file_name),
            _ => fallback.to_string(),
        };
        for r in rejected {
            self.warnings.push(format!("{r}; {instead}"));
        }
        self.settings.push(Setting {
            key,
            value: show(&value),
            source,
        });
        value
    }

    /// A path setting; any non-empty value is accepted.
    fn path(&mut self, key: &'static str, default: Option<PathBuf>, none: &str) -> Option<PathBuf> {
        let show = |p: &Option<PathBuf>| match p {
            Some(p) => p.display().to_string(),
            None => none.to_string(),
        };
        self.get(key, "", |v| Some(Some(PathBuf::from(v))), default, "", show)
    }

    /// A setting parsed from text by one of the `parse_*` functions.
    fn text<T>(
        &mut self,
        key: &'static str,
        word: &str,
        parse: impl Fn(&str) -> Option<T>,
        default: T,
        fallback: &str,
        show: impl Fn(&T) -> String,
    ) -> T {
        let parse = |v: &OsStr| v.to_str().and_then(&parse);
        self.get(key, word, parse, default, fallback, show)
    }
}

impl Config {
    /// The configuration of this process: its environment and config file.
    pub fn load() -> Config {
        Config::from_sources(&|name| env::var_os(name), &read_file)
    }

    /// The configuration from `env` (a variable lookup) and the config file
    /// as `read` returns it.
    pub fn from_sources(env: &dyn Fn(&str) -> Option<OsString>, read: &ReadFile<'_>) -> Config {
        let home = non_empty(env("HOME")).map(PathBuf::from);
        let path = file_location(env);
        let mut warnings = Vec::new();
        let mut entries = BTreeMap::new();
        let status = match path.as_deref().map(read) {
            None | Some(Ok(None)) => FileStatus::Missing,
            Some(Err(e)) => {
                let p = path.as_deref().unwrap_or(Path::new("")).display();
                warnings.push(format!("cannot read config file {p}: {e}; ignoring it"));
                FileStatus::Unreadable(e)
            }
            Some(Ok(Some(text))) => {
                let p = path.as_deref().unwrap_or(Path::new("")).display();
                let (e, problems) = parse_file(&text, home.as_deref());
                entries = e;
                for (n, msg) in problems {
                    warnings.push(format!("config file {p} line {n}: {msg}"));
                }
                FileStatus::Found
            }
        };
        let file_name = path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut r = Resolver {
            env,
            entries,
            file_name,
            settings: Vec::new(),
            warnings,
        };

        let real_z3 = r.path("real_z3", None, "none (first other z3 on PATH)");
        let dir = r.path("dir", default_dir(env), "none");
        let mode = r.text(
            "mode",
            "unknown",
            |v| match v {
                "rw" => Some(Mode::Rw),
                "ro" => Some(Mode::Ro),
                "enforce" => Some(Mode::Enforce),
                "off" => Some(Mode::Off),
                _ => None,
            },
            Mode::Rw,
            "using rw",
            |m| {
                match m {
                    Mode::Rw => "rw",
                    Mode::Ro => "ro",
                    Mode::Enforce => "enforce",
                    Mode::Off => "off",
                }
                .into()
            },
        );
        let store_policy = r.text(
            "store",
            "unknown",
            |v| match v {
                "unsat" => Some(StorePolicy::Unsat),
                "all" => Some(StorePolicy::All),
                _ => None,
            },
            StorePolicy::Unsat,
            "storing unsat only",
            |p| {
                match p {
                    StorePolicy::Unsat => "unsat",
                    StorePolicy::All => "all",
                }
                .into()
            },
        );
        let lifetime = r.text(
            "solver",
            "unknown",
            |v| match v {
                "keep" => Some(Lifetime::Keep),
                "fresh" => Some(Lifetime::Fresh),
                _ => None,
            },
            Lifetime::Keep,
            "using keep",
            |l| {
                match l {
                    Lifetime::Keep => "keep",
                    Lifetime::Fresh => "fresh",
                }
                .into()
            },
        );
        let max_size = r.text(
            "max_size",
            "invalid",
            parse_size,
            DEFAULT_MAX_SIZE,
            &format!("using {}G", DEFAULT_MAX_SIZE >> 30),
            |&n| show_size(n),
        );
        let verify = r.text(
            "verify",
            "invalid",
            parse_fraction,
            0.0,
            "auditing nothing",
            |p| p.to_string(),
        );
        let gc_every = r.text(
            "gc_every",
            "invalid",
            parse_count,
            DEFAULT_GC_EVERY,
            &format!("using {DEFAULT_GC_EVERY}"),
            |n| n.to_string(),
        );
        let lock_wait = r.text(
            "lock_wait",
            "invalid",
            parse_seconds,
            DEFAULT_LOCK_WAIT,
            &format!("using {} seconds", DEFAULT_LOCK_WAIT.as_secs()),
            |d| d.as_secs_f64().to_string(),
        );
        let log = r.path("log", None, "none");
        let record = r.path("record", None, "none");
        let solver_salt = r.get(
            "solver_salt",
            "",
            |v| Some(v.as_encoded_bytes().to_vec()),
            Vec::new(),
            "",
            |s| {
                if s.is_empty() {
                    "none".into()
                } else {
                    format!("{:?}", String::from_utf8_lossy(s))
                }
            },
        );
        Config {
            real_z3,
            mode,
            dir,
            record,
            log,
            solver_salt,
            store_policy,
            lifetime,
            max_size,
            verify,
            gc_every,
            lock_wait,
            file: ConfigFile { path, status },
            settings: r.settings,
            warnings: r.warnings,
        }
    }

    /// Where the setting `key` came from.
    pub fn source(&self, key: &str) -> Source {
        self.settings
            .iter()
            .find(|s| s.key == key)
            .map_or(Source::Default, |s| s.source.clone())
    }

    /// Names where the setting `key` was given: its environment variable,
    /// or the key and the config file line. `None` for a default.
    pub fn origin(&self, key: &str) -> Option<String> {
        match self.source(key) {
            Source::Env => Some(env_name(key)),
            Source::File(n) => Some(format!(
                "{key} in {} line {n}",
                self.file.path.as_deref().unwrap_or(Path::new("")).display()
            )),
            Source::Default => None,
        }
    }

    /// How to set `key`, for messages that ask the user to set it.
    pub fn how_to_set(&self, key: &str) -> String {
        match &self.file.path {
            Some(p) => format!("set {key} in {} or {}", p.display(), env_name(key)),
            None => format!("set {}", env_name(key)),
        }
    }
}

/// A size as `parse_size` reads it: the largest exact unit.
fn show_size(n: u64) -> String {
    for (shift, unit) in [(40, "T"), (30, "G"), (20, "M"), (10, "K")] {
        if n.trailing_zeros() >= shift {
            return format!("{}{unit}", n >> shift);
        }
    }
    n.to_string()
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
    let unit_start = upper
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(upper.len());
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
fn default_dir(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let home = non_empty(env("HOME")).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches/z3cache"));
    }
    non_empty(env("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|d| d.join("z3cache"))
}

/// Finds the real solver: the `real_z3` setting, or else the first
/// executable with the profile's binary name on `PATH` that is not this
/// program.
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
        let given = match cfg.source("real_z3") {
            Source::Env => format!("Z3CACHE_REAL_Z3={}", p.display()),
            _ => format!(
                "real_z3 = {} ({})",
                p.display(),
                cfg.origin("real_z3").unwrap_or_default()
            ),
        };
        if !is_executable(p) {
            return Err(format!("{given} is not an executable file"));
        }
        if is_me(p) {
            return Err(format!("{given} points at z3cache itself"));
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
        "no real {} found on PATH; {}",
        profile.binary_name(),
        cfg.how_to_set("real_z3")
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

    /// The configuration from the variables `vars` and, at
    /// `/cfg/z3cache/config`, the file `file` (`None`: no file).
    fn config(vars: &[(&str, &str)], file: Option<&str>) -> Config {
        let mut all = vec![("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/cfg")];
        all.extend_from_slice(vars);
        let lookup = |name: &str| {
            all.iter()
                .rev()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        };
        let read = |p: &Path| {
            assert_eq!(p, Path::new("/cfg/z3cache/config"));
            Ok(file.map(|t| t.as_bytes().to_vec()))
        };
        Config::from_sources(&lookup, &read)
    }

    type Parsed = (Vec<(&'static str, String, usize)>, Vec<(usize, String)>);

    fn parse(text: &str) -> Parsed {
        let (entries, problems) = parse_file(text.as_bytes(), Some(Path::new("/home/u")));
        let entries = entries
            .into_iter()
            .map(|(k, e)| (k, e.value.to_string_lossy().into_owned(), e.line))
            .collect();
        (entries, problems)
    }

    #[test]
    fn file_syntax() {
        let (entries, problems) = parse(
            "# comment\n\n   \n  # indented comment\nmode=ro\n  max_size   =   500M  \n\
             solver_salt = \"  a = b  \"\nverify = \"\"\r\nstore = \"all\n",
        );
        assert_eq!(
            entries,
            [
                ("max_size", "500M".into(), 6),
                ("mode", "ro".into(), 5),
                ("solver_salt", "  a = b  ".into(), 7),
                ("store", "\"all".into(), 9),
                ("verify", "".into(), 8),
            ]
        );
        assert_eq!(problems, []);
    }

    #[test]
    fn file_paths_expand_home() {
        let (entries, problems) = parse(
            "dir = ~/cache\nlog = \"~/z3cache.log\"\nrecord = /abs/~/x\nreal_z3 = ~\n\
             solver_salt = ~/not-a-path\n",
        );
        assert_eq!(
            entries,
            [
                ("dir", "/home/u/cache".into(), 1),
                ("log", "/home/u/z3cache.log".into(), 2),
                ("real_z3", "~".into(), 4),
                ("record", "/abs/~/x".into(), 3),
                ("solver_salt", "~/not-a-path".into(), 5),
            ]
        );
        assert_eq!(problems, []);
        let (entries, problems) = parse_file(b"dir = ~/cache\n", None);
        assert!(entries.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].1.contains("HOME is unset"), "{problems:?}");
    }

    #[test]
    fn file_problems_are_reported_by_line() {
        let (entries, problems) = parse(
            "mode = ro\njust words\n= rw\nMODE = rw\nz3cache_dir = /x\nmode = enforce\n\
             gc_every = 5\n",
        );
        assert_eq!(
            entries,
            [("gc_every", "5".into(), 7), ("mode", "enforce".into(), 6),]
        );
        let lines: Vec<usize> = problems.iter().map(|p| p.0).collect();
        assert_eq!(lines, [2, 3, 4, 5, 6], "{problems:?}");
        assert!(problems[0].1.starts_with("expected `key = value`"));
        assert!(problems[1].1.starts_with("unknown key \"\""));
        assert!(problems[2].1.starts_with("unknown key \"MODE\""));
        assert!(problems[3].1.starts_with("unknown key \"z3cache_dir\""));
        assert!(problems[4].1.contains("also on line 1"), "{problems:?}");
        let (entries, problems) = parse_file(b"\xEF\xBB\xBFmode = ro\nlog = \xff\xfe\n", None);
        assert_eq!(entries.len(), 1);
        assert_eq!(problems, [(2, "not UTF-8; line ignored".to_string())]);
    }

    #[test]
    fn location() {
        let at = |vars: &[(&str, &str)]| {
            file_location(&|n| {
                vars.iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| OsString::from(v))
            })
        };
        let home = ("HOME", "/home/u");
        assert_eq!(at(&[home]), Some("/home/u/.config/z3cache/config".into()));
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "")]),
            Some("/home/u/.config/z3cache/config".into())
        );
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "/x")]),
            Some("/x/z3cache/config".into())
        );
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "/x"), ("Z3CACHE_CONFIG", "/c")]),
            Some("/c".into())
        );
        assert_eq!(
            at(&[home, ("Z3CACHE_CONFIG", "")]),
            Some("/home/u/.config/z3cache/config".into())
        );
        assert_eq!(at(&[]), None);
    }

    #[test]
    fn defaults_without_a_file() {
        let c = config(&[], None);
        assert_eq!(c.file.status, FileStatus::Missing);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.mode, Mode::Rw);
        assert_eq!(c.max_size, DEFAULT_MAX_SIZE);
        assert_eq!(c.real_z3, None);
        assert!(c.solver_salt.is_empty());
        let keys: Vec<&str> = c.settings.iter().map(|s| s.key).collect();
        let all: Vec<&str> = KEYS.iter().map(|k| k.0).collect();
        assert_eq!(keys, all);
        assert!(c.settings.iter().all(|s| s.source == Source::Default));
        let shown = |k: &str| {
            c.settings
                .iter()
                .find(|s| s.key == k)
                .unwrap()
                .value
                .clone()
        };
        assert_eq!(shown("max_size"), "2G");
        assert_eq!(shown("lock_wait"), "60");
        assert_eq!(shown("verify"), "0");
        assert_eq!(shown("log"), "none");
    }

    #[test]
    fn env_over_file_over_default() {
        let file = "mode = ro\nmax_size = 500M\ndir = /from/file\nsolver_salt = s\n";
        let c = config(&[], Some(file));
        assert_eq!(c.file.status, FileStatus::Found);
        assert_eq!(c.mode, Mode::Ro);
        assert_eq!(c.max_size, 500 << 20);
        assert_eq!(c.dir, Some("/from/file".into()));
        assert_eq!(c.solver_salt, b"s");
        assert_eq!(c.source("mode"), Source::File(1));
        assert_eq!(c.source("verify"), Source::Default);
        assert_eq!(
            c.origin("mode").as_deref(),
            Some("mode in /cfg/z3cache/config line 1")
        );

        let c = config(
            &[
                ("Z3CACHE_MODE", "enforce"),
                ("Z3CACHE_DIR", "/from/env"),
                ("Z3CACHE_SOLVER_SALT", "t"),
            ],
            Some(file),
        );
        assert_eq!(c.mode, Mode::Enforce);
        assert_eq!(c.dir, Some("/from/env".into()));
        assert_eq!(c.solver_salt, b"t");
        assert_eq!(c.max_size, 500 << 20);
        assert_eq!(c.source("mode"), Source::Env);
        assert_eq!(c.origin("mode").as_deref(), Some("Z3CACHE_MODE"));
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn empty_values_fall_through() {
        let c = config(
            &[("Z3CACHE_MODE", ""), ("Z3CACHE_DIR", "")],
            Some("mode = off\ndir = /from/file\nmax_size =\n"),
        );
        assert_eq!(c.mode, Mode::Off);
        assert_eq!(c.dir, Some("/from/file".into()));
        assert_eq!(c.max_size, DEFAULT_MAX_SIZE);
        assert_eq!(c.source("max_size"), Source::Default);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn invalid_values_fall_back_to_the_next_source() {
        let c = config(
            &[("Z3CACHE_MAX_SIZE", "lots"), ("Z3CACHE_VERIFY", "2")],
            Some("max_size = 1M\nmode = sometimes\nverify = 0.5\ngc_every = -1\n"),
        );
        assert_eq!(c.max_size, 1 << 20);
        assert_eq!(c.mode, Mode::Rw);
        assert_eq!(c.verify, 0.5);
        assert_eq!(c.gc_every, DEFAULT_GC_EVERY);
        assert_eq!(c.source("mode"), Source::Default);
        assert_eq!(
            c.warnings,
            [
                "unknown mode \"sometimes\" in /cfg/z3cache/config line 2; using rw",
                "invalid Z3CACHE_MAX_SIZE \"lots\"; using max_size from /cfg/z3cache/config line 1",
                "invalid Z3CACHE_VERIFY \"2\"; using verify from /cfg/z3cache/config line 3",
                "invalid gc_every \"-1\" in /cfg/z3cache/config line 4; using 100",
            ]
        );
        // Both invalid: the default, with a warning for each.
        let c = config(&[("Z3CACHE_LOCK_WAIT", "NaN")], Some("lock_wait = -1\n"));
        assert_eq!(c.lock_wait, DEFAULT_LOCK_WAIT);
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
        assert!(c.warnings.iter().all(|w| w.ends_with("using 60 seconds")));
    }

    #[test]
    fn file_problems_are_warnings() {
        let c = config(&[], Some("mode = ro\nwhat\ncolour = blue\nmode = off\n"));
        assert_eq!(c.mode, Mode::Off);
        assert_eq!(c.warnings.len(), 3, "{:?}", c.warnings);
        for (w, n) in c.warnings.iter().zip([2, 3, 4]) {
            assert!(
                w.starts_with(&format!("config file /cfg/z3cache/config line {n}: ")),
                "{w}"
            );
        }
    }

    #[test]
    fn unreadable_file_is_a_warning() {
        let lookup = |n: &str| (n == "Z3CACHE_CONFIG").then(|| OsString::from("/c"));
        let c = Config::from_sources(&lookup, &|_| Err("denied".into()));
        assert_eq!(c.file.status, FileStatus::Unreadable("denied".into()));
        assert_eq!(
            c.warnings,
            ["cannot read config file /c: denied; ignoring it"]
        );
        assert_eq!(c.mode, Mode::Rw);
    }

    #[test]
    fn reading_refuses_anything_but_a_regular_file() {
        let d = crate::test_dir("config", "read");
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(read_file(&d.join("missing")), Ok(None));
        let f = d.join("config");
        std::fs::write(&f, "mode = ro\n").unwrap();
        assert_eq!(read_file(&f), Ok(Some(b"mode = ro\n".to_vec())));
        assert_eq!(read_file(&f.join("below-a-file")), Ok(None));
        assert!(read_file(&d).is_err());
        let fifo = d.join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: mkfifo(3) on a path we own.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_file(&fifo), Err("not a regular file".into()));
        std::fs::write(&f, vec![b'#'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        assert!(read_file(&f).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sizes_are_shown_in_units() {
        assert_eq!(show_size(2 << 30), "2G");
        assert_eq!(show_size(500 << 20), "500M");
        assert_eq!(show_size(1536 << 10), "1536K");
        assert_eq!(show_size(1 << 40), "1T");
        assert_eq!(show_size(1000), "1000");
    }

    #[test]
    fn seconds() {
        assert_eq!(parse_seconds("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse_seconds("0"), Some(Duration::ZERO));
        assert_eq!(parse_seconds("0.5"), Some(Duration::from_millis(500)));
        assert_eq!(parse_seconds(" 2.25 "), Some(Duration::from_millis(2250)));
        for bad in [
            "1e100", "1e20", "NaN", "nan", "inf", "-inf", "infinity", "-1", "-0.5", "", "abc", "1s",
        ] {
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
