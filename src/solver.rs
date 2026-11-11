//! The real solver process (DESIGN sections 7 and 14).
//!
//! A dedicated thread drains the solver's stdout into a channel line by
//! line, so writing a large batch to the solver can never deadlock on a full
//! output pipe.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

pub struct Solver {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    lines: Option<Receiver<Vec<u8>>>,
    drain: Option<JoinHandle<()>>,
}

/// Lines read up to a sentinel.
pub struct Response {
    /// Each line including its newline; the last may lack one at end of
    /// output.
    pub lines: Vec<Vec<u8>>,
    /// The solver's output ended before the sentinel arrived.
    pub eof: bool,
}

impl Solver {
    pub fn spawn(path: &Path, args: &[OsString]) -> io::Result<Solver> {
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        CHILD_PID.store(child.id() as i32, Ordering::SeqCst);
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let (tx, rx) = mpsc::channel();
        let drain = std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, stdout);
            loop {
                let mut line = Vec::new();
                match r.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Solver {
            child,
            stdin: Some(BufWriter::with_capacity(1 << 16, stdin)),
            lines: Some(rx),
            drain: Some(drain),
        })
    }

    /// Buffers bytes for the solver. Fails if the solver has gone away.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.stdin {
            Some(w) => w.write_all(bytes),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.stdin {
            Some(w) => w.flush(),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Writes bytes to the solver and flushes them.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write(bytes)?;
        self.flush()
    }

    /// Reads lines until one equals one of `sentinels`, which is not
    /// returned.
    pub fn read_until(&mut self, sentinels: &[Vec<u8>]) -> Response {
        let mut lines = Vec::new();
        let Some(rx) = &self.lines else {
            return Response { lines, eof: true };
        };
        for line in rx.iter() {
            if sentinels.contains(&line) {
                return Response { lines, eof: false };
            }
            lines.push(line);
        }
        Response { lines, eof: true }
    }

    /// Makes this solver the process killed on SIGINT, SIGTERM or SIGHUP.
    pub fn track(&self) {
        CHILD_PID.store(self.child.id() as i32, Ordering::SeqCst);
    }

    /// True if the solver process has exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Hands the solver's stdin to the caller (for bypass).
    pub fn take_stdin(&mut self) -> Option<BufWriter<ChildStdin>> {
        self.stdin.take()
    }

    /// Hands the output channel to the caller (for bypass).
    pub fn take_lines(&mut self) -> Option<Receiver<Vec<u8>>> {
        self.lines.take()
    }

    /// Closes the solver's stdin (flushing first), which tells it to exit.
    fn close_stdin(&mut self) {
        if let Some(mut w) = self.stdin.take() {
            let _ = w.flush();
        }
    }

    /// Closes the solver's stdin, collects the rest of its output and waits
    /// for it to exit. Returns that output and the exit code to pass on.
    pub fn finish(&mut self) -> (Vec<u8>, i32) {
        self.close_stdin();
        let rest = match self.lines.take() {
            Some(rx) => rx.iter().collect::<Vec<_>>().concat(),
            None => Vec::new(),
        };
        (rest, self.wait())
    }

    /// Waits for the solver to exit and returns the exit code to pass on.
    pub fn wait(&mut self) -> i32 {
        self.close_stdin();
        if let Some(d) = self.drain.take() {
            let _ = d.join();
        }
        let code = match self.child.wait() {
            Ok(status) => status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
            Err(_) => 1,
        };
        CHILD_PID.store(0, Ordering::SeqCst);
        code
    }
}

/// BLAKE3 of the executable's contents. Memoised in the store by path, size
/// and modification time.
pub fn hash_executable(path: &Path) -> io::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    h.update(b"z3cache/solver");
    h.update_reader(File::open(path)?)?;
    Ok(*h.finalize().as_bytes())
}

/// The `solver_id` that goes into keys: the executable's hash mixed with
/// `Z3CACHE_SOLVER_SALT`.
pub fn solver_id(executable: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"z3cache/solver-id");
    h.update(executable);
    h.update(&(salt.len() as u64).to_le_bytes());
    h.update(salt);
    *h.finalize().as_bytes()
}

static CHILD_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    // Only async-signal-safe calls here.
    let pid = CHILD_PID.load(Ordering::SeqCst);
    unsafe {
        if pid > 0 {
            libc::kill(pid, libc::SIGKILL);
        }
        libc::_exit(128 + sig);
    }
}

/// On SIGINT, SIGTERM or SIGHUP, kill the solver and exit.
pub fn install_signal_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
}
