//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use z3cache::store::{SqliteStore, Store};

pub fn z3cache() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_z3cache"))
}

/// The scripted stub solver, built from `examples/stub_solver.rs`.
pub fn stub() -> PathBuf {
    let p = z3cache().parent().unwrap().join("examples/stub_solver");
    assert!(
        p.exists(),
        "stub solver not built at {}; run plain `cargo test` or `cargo build --examples`",
        p.display()
    );
    p
}

pub fn corpus(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/corpus")
            .join(name),
    )
    .unwrap()
}

pub fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "z3cache-test-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        name
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Run {
    pub stdout: Vec<u8>,
    pub code: i32,
}

impl Run {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// A config file path that never exists. Tests use it as `Z3CACHE_CONFIG`
/// so that they never read the user's config file.
pub fn no_config() -> PathBuf {
    std::env::temp_dir()
        .join(format!("z3cache-test-{}-no-config", std::process::id()))
        .join("config")
}

/// A command running `bin` with `args`, stdin and stdout piped, all
/// `Z3CACHE_*` and `Z3STUB_*` variables cleared and `env` set. Unless `env`
/// sets `Z3CACHE_DIR`, it gets a fresh, empty cache; unless it sets
/// `Z3CACHE_CONFIG`, no config file.
pub fn proxy_command(bin: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("Z3CACHE_") || k.starts_with("Z3STUB_") {
            cmd.env_remove(&*k);
        }
    }
    // Never touch the user's cache: a fresh one per run unless given.
    if !env.iter().any(|(k, _)| *k == "Z3CACHE_DIR") {
        cmd.env("Z3CACHE_DIR", temp_dir("cache"));
    }
    // Never read the user's config file.
    if !env.iter().any(|(k, _)| *k == "Z3CACHE_CONFIG") {
        cmd.env("Z3CACHE_CONFIG", no_config());
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

/// Runs `bin` as [`proxy_command`] sets it up, feeding `input` in chunks of
/// `chunk` bytes.
pub fn run_chunked(
    bin: &Path,
    args: &[&str],
    input: &[u8],
    chunk: usize,
    env: &[(&str, &OsStr)],
) -> Run {
    let mut child = proxy_command(bin, args, env).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        for piece in input.chunks(chunk.max(1)) {
            if stdin.write_all(piece).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let status = child.wait().unwrap();
    writer.join().unwrap();
    Run {
        stdout,
        code: status.code().unwrap_or(-1),
    }
}

pub fn run(bin: &Path, args: &[&str], input: &[u8], env: &[(&str, &OsStr)]) -> Run {
    run_chunked(bin, args, input, usize::MAX, env)
}

/// Number of times the stub solver started, from its `Z3STUB_LOG`.
pub fn launches(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .map(|t| t.lines().filter(|l| l.starts_with("launch ")).count())
        .unwrap_or(0)
}

/// The `rlimit-count` values in a session's output, in order.
pub fn rlimit_counts(out: &str) -> Vec<u64> {
    out.split(":rlimit-count ")
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap()
        })
        .collect()
}

/// Like [`run`], but fails the test instead of hanging if `bin` has not
/// exited within `limit`.
pub fn run_bounded(
    bin: &Path,
    args: &[&str],
    input: &[u8],
    env: &[(&str, &OsStr)],
    limit: std::time::Duration,
) -> Run {
    let mut child = proxy_command(bin, args, env).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{} did not exit within {limit:?}", bin.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Run {
        stdout: reader.join().unwrap(),
        code: status.code().unwrap_or(-1),
    }
}

/// Polls `cond` until it holds. The deadline is generous: it only turns a
/// broken handshake into a failure instead of a hang.
pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting until {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// True if the file exists and contains `needle`.
pub fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains(needle))
}

/// The line z3cache logs (Z3CACHE_LOG) when it first finds an in-flight
/// lock held by another session.
pub const LOCK_BUSY: &str = "in-flight lock busy";

/// An all-time counter of the store in `cache`.
pub fn counter(cache: &Path, name: &str) -> i64 {
    let rows = SqliteStore::open_read_only(cache)
        .unwrap()
        .counters()
        .unwrap();
    rows.iter().filter(|r| r.1 == name).map(|r| r.2).sum()
}

/// `(declare-const x Int)`, then `n` distinct unsat queries, each in its own
/// frame, numbered from `offset`.
pub fn many_queries(offset: usize, n: usize) -> Vec<u8> {
    let mut s = String::from("(declare-const x Int)\n");
    for i in offset..offset + n {
        s.push_str(&format!("(push)(assert (> x {i}))(check-sat)(pop)\n"));
    }
    s.into_bytes()
}

/// The real Z3: `Z3CACHE_REAL_Z3`, or else the first `z3` on `PATH`.
pub fn real_z3() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("Z3CACHE_REAL_Z3").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("z3"))
        .find(|p| p.is_file())
}

/// Makes outputs comparable across the proxy's replay:
/// - error positions refer to the input as the solver received it, which the
///   proxy re-sends in canonical form;
/// - statistics vary from run to run;
/// - Z3 4.16.0 explains an exceeded resource limit as `canceled` if the
///   solver has been through a push/pop and as `max. resource limit
///   exceeded` otherwise, and the replay leaves out popped frames
///   (DESIGN section 13, solver history). The verdict is the same.
pub fn normalize(out: &str) -> String {
    let mut o = String::new();
    for line in out.split_inclusive('\n') {
        if line == "(:reason-unknown \"canceled\")\n"
            || line == "(:reason-unknown \"max. resource limit exceeded\")\n"
        {
            o.push_str("(:reason-unknown <resource limit>)\n");
            continue;
        }
        let is_stats = line.starts_with(" :")
            || (line.starts_with("(:")
                && !line.starts_with("(:version")
                && !line.starts_with("(:reason-unknown"));
        if is_stats {
            continue;
        }
        match line.strip_prefix("(error \"line ") {
            Some(rest) => match rest.find(": ") {
                Some(at) => {
                    o.push_str("(error \"");
                    o.push_str(&rest[at + 2..]);
                }
                None => o.push_str(line),
            },
            None => o.push_str(line),
        }
    }
    o
}

/// Like [`run`], but returns `None` instead of hanging if `bin` has not
/// exited within `limit` (it is then killed). Waits on the exit itself, not
/// by polling, so it adds no latency to fast runs.
pub fn run_within(
    bin: &Path,
    args: &[&str],
    input: &[u8],
    env: &[(&str, &OsStr)],
    limit: std::time::Duration,
) -> Option<Run> {
    let mut child = proxy_command(bin, args, env).spawn().unwrap();
    let pid = child.id() as i32;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let status = child.wait().unwrap();
        let _ = tx.send(Run {
            stdout: out,
            code: status.code().unwrap_or(-1),
        });
    });
    match rx.recv_timeout(limit) {
        Ok(run) => Some(run),
        Err(_) => {
            // SAFETY: kill(2) on our own child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            None
        }
    }
}
