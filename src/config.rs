//! Configuration (DESIGN section 11): each setting comes from its
//! environment variable if that is set and non-empty, else from the config
//! file (TOML), else its default. Nothing here is ever an error: an
//! unreadable or invalid file, an unknown key, a value of the wrong type or
//! an invalid value is a warning, and the setting falls back to the next
//! source.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use toml::de::{DeInteger, DeTable, DeValue};

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

/// What a config file key accepts, as TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A string; a leading `~/` expands to `$HOME`.
    Path,
    /// A string.
    Text,
    /// A string such as `"2G"`, or an integer number of bytes.
    Size,
    /// An integer or a float.
    Number,
    /// An integer.
    Count,
}

impl Kind {
    fn expected(self) -> &'static str {
        match self {
            Kind::Path | Kind::Text => "a string",
            Kind::Size => "a string or an integer",
            Kind::Number => "a number",
            Kind::Count => "an integer",
        }
    }
}

/// The config file's keys, in the order `doctor` shows them, and what each
/// accepts. Each key's environment variable is `Z3CACHE_` followed by the
/// key in upper case.
pub const KEYS: &[(&str, Kind)] = &[
    ("real_z3", Kind::Path),
    ("dir", Kind::Path),
    ("mode", Kind::Text),
    ("store", Kind::Text),
    ("solver", Kind::Text),
    ("max_size", Kind::Size),
    ("verify", Kind::Number),
    ("gc_every", Kind::Count),
    ("lock_wait", Kind::Number),
    ("log", Kind::Path),
    ("record", Kind::Path),
    ("solver_salt", Kind::Text),
];

/// The environment variable for a config file key.
pub fn env_name(key: &str) -> String {
    format!("Z3CACHE_{}", key.to_ascii_uppercase())
}

/// A config file larger than this is ignored: it cannot be a hand-written
/// file of a dozen settings, and reading it must not slow every solver start.
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
    /// It exists but could not be read; the reason is also a warning.
    Unreadable(String),
    /// It was read but is not valid TOML, so all of it is ignored; the
    /// reason is also a warning.
    Invalid(String),
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

/// One top-level key of the config file, its value as text (what the
/// environment variable would hold) and the line of the key (from 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub value: OsString,
    pub line: usize,
}

/// The settings a config file gives, by key.
pub type FileEntries = BTreeMap<&'static str, FileEntry>;

/// The line and column (from 1) of byte `offset` in `text`.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = before.matches('\n').count() + 1;
    (line, before[line_start..].chars().count() + 1)
}

/// Parses a config file: a TOML document whose top-level keys are those in
/// [`KEYS`]. A value of the right type becomes the text its environment
/// variable would hold (numbers in decimal), and a leading `~/` in a path
/// expands to `home`. Returns the entries by key and, sorted by line, a
/// message for each key that is ignored: unknown, of the wrong type, or a
/// path that needs `HOME`. A document that is not valid TOML is an error, a
/// message giving the line and column.
pub fn parse_file(
    text: &[u8],
    home: Option<&Path>,
) -> Result<(FileEntries, Vec<(usize, String)>), String> {
    // TOML allows a byte order mark; dropping it keeps columns right.
    let text = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    let text = std::str::from_utf8(text).map_err(|e| {
        let valid = String::from_utf8_lossy(&text[..e.valid_up_to()]);
        format!("line {}: not UTF-8", position(&valid, valid.len()).0)
    })?;
    let doc = DeTable::parse(text).map_err(|e| {
        let message = e.message().trim_end();
        match e.span() {
            Some(span) => {
                let (line, column) = position(text, span.start);
                format!("line {line}, column {column}: {message}")
            }
            None => message.to_string(),
        }
    })?;
    let mut entries = FileEntries::new();
    let mut problems = Vec::new();
    for (key, value) in doc.get_ref().iter() {
        let line = position(text, key.span().start).0;
        let name = key.get_ref().as_ref();
        let Some(&(name, kind)) = KEYS.iter().find(|(k, _)| *k == name) else {
            problems.push((line, format!("unknown key {name:?}; ignored")));
            continue;
        };
        let integer = |i: &DeInteger<'_>| {
            i64::from_str_radix(i.as_str(), i.radix())
                .map_or_else(|_| i.to_string(), |n| n.to_string())
        };
        let text = match (kind, value.get_ref()) {
            (Kind::Path | Kind::Text | Kind::Size, DeValue::String(s)) => s.to_string(),
            (Kind::Size | Kind::Number | Kind::Count, DeValue::Integer(i)) => integer(i),
            (Kind::Number, DeValue::Float(f)) => f.as_str().to_string(),
            (_, other) => {
                let found = match other {
                    DeValue::String(_) => "a string",
                    DeValue::Integer(_) => "an integer",
                    DeValue::Float(_) => "a float",
                    DeValue::Boolean(_) => "a boolean",
                    DeValue::Datetime(_) => "a date-time",
                    DeValue::Array(_) => "an array",
                    DeValue::Table(_) => "a table",
                };
                problems.push((
                    line,
                    format!("{name} must be {}, not {found}; ignored", kind.expected()),
                ));
                continue;
            }
        };
        let value = match text.strip_prefix("~/").filter(|_| kind == Kind::Path) {
            None => OsString::from(text),
            Some(rest) => match home {
                Some(h) => h.join(rest).into_os_string(),
                None => {
                    problems.push((
                        line,
                        format!("cannot expand ~ in {name}: HOME is unset; ignored"),
                    ));
                    continue;
                }
            },
        };
        entries.insert(name, FileEntry { value, line });
    }
    problems.sort_by_key(|p| p.0);
    Ok((entries, problems))
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

/// `$Z3CACHE_CONFIG`, else
/// `${XDG_CONFIG_HOME:-~/.config}/z3cache/config.toml`, on every platform.
fn file_location(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(p) = non_empty(env("Z3CACHE_CONFIG")) {
        return Some(PathBuf::from(p));
    }
    non_empty(env("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| non_empty(env("HOME")).map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("z3cache/config.toml"))
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
                match parse_file(&text, home.as_deref()) {
                    Ok((e, problems)) => {
                        entries = e;
                        for (n, msg) in problems {
                            warnings.push(format!("config file {p} line {n}: {msg}"));
                        }
                        FileStatus::Found
                    }
                    Err(e) => {
                        warnings.push(format!(
                            "config file {p} is not valid TOML: {e}; ignoring it"
                        ));
                        FileStatus::Invalid(e)
                    }
                }
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
    /// `/cfg/z3cache/config.toml`, the file `file` (`None`: no file).
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
            assert_eq!(p, Path::new("/cfg/z3cache/config.toml"));
            Ok(file.map(|t| t.as_bytes().to_vec()))
        };
        Config::from_sources(&lookup, &read)
    }

    type Parsed = (Vec<(&'static str, String, usize)>, Vec<(usize, String)>);

    fn parse(text: &str) -> Parsed {
        let (entries, problems) =
            parse_file(text.as_bytes(), Some(Path::new("/home/u"))).expect("valid TOML");
        let entries = entries
            .into_iter()
            .map(|(k, e)| (k, e.value.to_string_lossy().into_owned(), e.line))
            .collect();
        (entries, problems)
    }

    #[test]
    fn file_with_every_key() {
        let (entries, problems) = parse(
            "\u{feff}# z3cache\n\
             real_z3 = \"/opt/z3\"\n\
             dir = '/var/cache/z3cache'   # a comment\n\
             mode = \"ro\"\n\
             store = \"all\"\n\
             solver = \"fresh\"\n\
             max_size = \"500M\"\n\
             verify = 0.05\n\
             gc_every = 0x10\n\
             lock_wait = 2.5\n\
             log = \"\"\"/tmp/z3cache.log\"\"\"\n\
             record = \"/tmp/rec\"\n\
             solver_salt = \"  a = b  \"\n",
        );
        assert_eq!(
            entries,
            [
                ("dir", "/var/cache/z3cache".into(), 3),
                ("gc_every", "16".into(), 9),
                ("lock_wait", "2.5".into(), 10),
                ("log", "/tmp/z3cache.log".into(), 11),
                ("max_size", "500M".into(), 7),
                ("mode", "ro".into(), 4),
                ("real_z3", "/opt/z3".into(), 2),
                ("record", "/tmp/rec".into(), 12),
                ("solver", "fresh".into(), 6),
                ("solver_salt", "  a = b  ".into(), 13),
                ("store", "all".into(), 5),
                ("verify", "0.05".into(), 8),
            ]
        );
        assert_eq!(problems, []);
        let c = config(
            &[],
            Some(
                "real_z3 = \"/opt/z3\"\ndir = \"/d\"\nmode = \"ro\"\nstore = \"all\"\n\
                 solver = \"fresh\"\nmax_size = \"500M\"\nverify = 0.05\ngc_every = 7\n\
                 lock_wait = 2.5\nlog = \"/l\"\nrecord = \"/r\"\nsolver_salt = \"s\"\n",
            ),
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.real_z3, Some("/opt/z3".into()));
        assert_eq!(c.dir, Some("/d".into()));
        assert_eq!(c.mode, Mode::Ro);
        assert_eq!(c.store_policy, StorePolicy::All);
        assert_eq!(c.lifetime, Lifetime::Fresh);
        assert_eq!(c.max_size, 500 << 20);
        assert_eq!(c.verify, 0.05);
        assert_eq!(c.gc_every, 7);
        assert_eq!(c.lock_wait, Duration::from_millis(2500));
        assert_eq!(c.log, Some("/l".into()));
        assert_eq!(c.record, Some("/r".into()));
        assert_eq!(c.solver_salt, b"s");
        assert!(
            c.settings
                .iter()
                .all(|s| matches!(s.source, Source::File(_))),
            "{:?}",
            c.settings
        );
    }

    #[test]
    fn numbers_and_strings() {
        // max_size: a size string or a number of bytes.
        let c = config(&[], Some("max_size = \"2G\"\n"));
        assert_eq!(c.max_size, 2 << 30);
        let c = config(&[], Some("max_size = 1_048_576\n"));
        assert_eq!(c.max_size, 1 << 20);
        assert_eq!(c.settings[5].value, "1M");
        // lock_wait and verify: an integer or a float.
        let c = config(&[], Some("lock_wait = 5\nverify = 1\n"));
        assert_eq!(c.lock_wait, Duration::from_secs(5));
        assert_eq!(c.verify, 1.0);
        let c = config(&[], Some("lock_wait = 0.25\nverify = 0.5\n"));
        assert_eq!(c.lock_wait, Duration::from_millis(250));
        assert_eq!(c.verify, 0.5);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        // Numbers are checked as their variables are.
        let c = config(
            &[],
            Some("max_size = 0\nlock_wait = -1\nverify = 1.5\ngc_every = -3\n"),
        );
        assert_eq!(
            (c.max_size, c.lock_wait, c.verify, c.gc_every),
            (DEFAULT_MAX_SIZE, DEFAULT_LOCK_WAIT, 0.0, DEFAULT_GC_EVERY)
        );
        assert_eq!(
            c.warnings,
            [
                "invalid max_size \"0\" in /cfg/z3cache/config.toml line 1; using 2G",
                "invalid verify \"1.5\" in /cfg/z3cache/config.toml line 3; auditing nothing",
                "invalid gc_every \"-3\" in /cfg/z3cache/config.toml line 4; using 100",
                "invalid lock_wait \"-1\" in /cfg/z3cache/config.toml line 2; using 60 seconds",
            ]
        );
        let c = config(&[], Some("lock_wait = inf\nverify = nan\n"));
        assert_eq!((c.lock_wait, c.verify), (DEFAULT_LOCK_WAIT, 0.0));
        assert_eq!(c.warnings.len(), 2, "{:?}", c.warnings);
    }

    #[test]
    fn file_paths_expand_home() {
        let (entries, problems) = parse(
            "dir = \"~/cache\"\nlog = '~/z3cache.log'\nrecord = \"/abs/~/x\"\nreal_z3 = \"~\"\n\
             solver_salt = \"~/not-a-path\"\nmode = \"~/x\"\n",
        );
        assert_eq!(
            entries,
            [
                ("dir", "/home/u/cache".into(), 1),
                ("log", "/home/u/z3cache.log".into(), 2),
                ("mode", "~/x".into(), 6),
                ("real_z3", "~".into(), 4),
                ("record", "/abs/~/x".into(), 3),
                ("solver_salt", "~/not-a-path".into(), 5),
            ]
        );
        assert_eq!(problems, []);
        let (entries, problems) = parse_file(b"\ndir = \"~/cache\"\n", None).unwrap();
        assert!(entries.is_empty());
        assert_eq!(
            problems,
            [(
                2,
                "cannot expand ~ in dir: HOME is unset; ignored".to_string()
            )]
        );
    }

    #[test]
    fn invalid_toml_is_rejected_as_a_whole() {
        for (text, want) in [
            (&b"mode = ro\n"[..], "line 1, column 8: "),
            (b"mode = \"ro\"\njust words\n", "line 2, column "),
            (
                b"mode = \"ro\"\nmode = \"rw\"\n",
                "line 2, column 1: duplicate key",
            ),
            (b"mode = \"ro\"\nlog = \"\xff\"\n", "line 2: not UTF-8"),
            (b"max_size = 500M\n", "line 1, column 12: "),
            (b"\xEF\xBB\xBFmode = 1x\n", "line 1, column 8: "),
        ] {
            let e = parse_file(text, None).unwrap_err();
            assert!(
                e.starts_with(want),
                "{e:?} for {:?}",
                String::from_utf8_lossy(text)
            );
            assert!(!e.contains('\n'), "{e:?}");
        }
        let c = config(
            &[("Z3CACHE_VERIFY", "0.5")],
            Some("mode = \"ro\"\nmax_size = 500M\n"),
        );
        let FileStatus::Invalid(e) = &c.file.status else {
            panic!("{:?}", c.file.status)
        };
        assert_eq!(
            c.warnings,
            [format!(
                "config file /cfg/z3cache/config.toml is not valid TOML: {e}; ignoring it"
            )]
        );
        // Nothing from the file applies; the environment still does.
        assert_eq!(
            (c.mode, c.max_size, c.verify),
            (Mode::Rw, DEFAULT_MAX_SIZE, 0.5)
        );
        assert_eq!(c.source("mode"), Source::Default);
        assert_eq!(c.source("verify"), Source::Env);
    }

    #[test]
    fn deep_nesting_does_not_crash() {
        // Within the 1 MiB file cap: the parser's recursion limit applies to
        // arrays and inline tables; dotted keys and headers only nest tables.
        let n = 200_000;
        let texts = [
            format!("mode = {}", "[".repeat(n)),
            format!("mode = {}", "{a=".repeat(n)),
            format!("{} = 1\n", ["a"; 200_000].join(".")),
            format!("[{}]\n", ["mode"; 200_000].join(".")),
        ];
        for text in texts {
            assert!(text.len() < MAX_FILE_BYTES as usize);
            let _ = parse_file(text.as_bytes(), None);
        }
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_ignored() {
        let (entries, problems) = parse(
            "colour = \"blue\"\nMODE = \"rw\"\nmode = \"enforce\"\n\
             gc_every = 1.5\nverify = \"0.5\"\nmax_size = 2.0\nlock_wait = true\n\
             dir = 7\nlog = [\"/a\"]\nrecord = { path = \"/r\" }\n\
             real_z3 = 1979-05-27\nsolver_salt.x = \"y\"\n\
             [z3cache]\nmode = \"ro\"\n[store]\nkind = \"all\"\n",
        );
        assert_eq!(entries, [("mode", "enforce".into(), 3)]);
        assert_eq!(
            problems,
            [
                (1, "unknown key \"colour\"; ignored".to_string()),
                (2, "unknown key \"MODE\"; ignored".into()),
                (
                    4,
                    "gc_every must be an integer, not a float; ignored".into()
                ),
                (5, "verify must be a number, not a string; ignored".into()),
                (
                    6,
                    "max_size must be a string or an integer, not a float; ignored".into()
                ),
                (
                    7,
                    "lock_wait must be a number, not a boolean; ignored".into()
                ),
                (8, "dir must be a string, not an integer; ignored".into()),
                (9, "log must be a string, not an array; ignored".into()),
                (10, "record must be a string, not a table; ignored".into()),
                (
                    11,
                    "real_z3 must be a string, not a date-time; ignored".into()
                ),
                (
                    12,
                    "solver_salt must be a string, not a table; ignored".into()
                ),
                (13, "unknown key \"z3cache\"; ignored".into()),
                (15, "store must be a string, not a table; ignored".into()),
            ]
        );
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
        assert_eq!(
            at(&[home]),
            Some("/home/u/.config/z3cache/config.toml".into())
        );
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "")]),
            Some("/home/u/.config/z3cache/config.toml".into())
        );
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "/x")]),
            Some("/x/z3cache/config.toml".into())
        );
        assert_eq!(
            at(&[home, ("XDG_CONFIG_HOME", "/x"), ("Z3CACHE_CONFIG", "/c")]),
            Some("/c".into())
        );
        assert_eq!(
            at(&[home, ("Z3CACHE_CONFIG", "")]),
            Some("/home/u/.config/z3cache/config.toml".into())
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
        let file =
            "mode = \"ro\"\nmax_size = \"500M\"\ndir = \"/from/file\"\nsolver_salt = \"s\"\n";
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
            Some("mode in /cfg/z3cache/config.toml line 1")
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
            Some("mode = \"off\"\ndir = \"/from/file\"\nmax_size = \"\"\n"),
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
            Some("max_size = \"1M\"\nmode = \"sometimes\"\nverify = 0.5\ngc_every = -1\n"),
        );
        assert_eq!(c.max_size, 1 << 20);
        assert_eq!(c.mode, Mode::Rw);
        assert_eq!(c.verify, 0.5);
        assert_eq!(c.gc_every, DEFAULT_GC_EVERY);
        assert_eq!(c.source("mode"), Source::Default);
        assert_eq!(
            c.warnings,
            [
                "unknown mode \"sometimes\" in /cfg/z3cache/config.toml line 2; using rw",
                "invalid Z3CACHE_MAX_SIZE \"lots\"; using max_size from /cfg/z3cache/config.toml line 1",
                "invalid Z3CACHE_VERIFY \"2\"; using verify from /cfg/z3cache/config.toml line 3",
                "invalid gc_every \"-1\" in /cfg/z3cache/config.toml line 4; using 100",
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
        let c = config(
            &[("Z3CACHE_GC_EVERY", "5")],
            Some("mode = \"ro\"\ncolour = \"blue\"\ngc_every = \"7\"\nverify = true\n"),
        );
        assert_eq!(c.mode, Mode::Ro);
        assert_eq!(c.gc_every, 5);
        assert_eq!(c.verify, 0.0);
        assert_eq!(c.source("verify"), Source::Default);
        assert_eq!(
            c.warnings,
            [
                "config file /cfg/z3cache/config.toml line 2: unknown key \"colour\"; ignored",
                "config file /cfg/z3cache/config.toml line 3: gc_every must be an integer, \
                 not a string; ignored",
                "config file /cfg/z3cache/config.toml line 4: verify must be a number, \
                 not a boolean; ignored",
            ]
        );
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
        let f = d.join("config.toml");
        std::fs::write(&f, "mode = \"ro\"\n").unwrap();
        assert_eq!(read_file(&f), Ok(Some(b"mode = \"ro\"\n".to_vec())));
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
