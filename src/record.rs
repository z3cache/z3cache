//! Session recording (`Z3CACHE_RECORD`, DESIGN section 12).
//!
//! Each proxied session writes four files named `<start_ms>-<pid>.*` in the
//! record directory:
//!
//! - `.in`: every byte the client sent;
//! - `.out`: every byte written to the client;
//! - `.tsv`: one line per query or read (see [`RECORDS_HEADER`]);
//! - `.meta`: `key=value` lines describing the session. Written at start and
//!   rewritten at exit; a file without `end_unix_ms` is from a session that
//!   was killed.
//!
//! Invocations that are passed straight to the real solver append a line to
//! `exec.log`.
//!
//! Recording never affects the session: a write error disables the file
//! concerned and is logged.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::log;
use crate::state::{Hashes, Verdict};

/// Version 2 added `served`: `cache`, `audited` or `solver`. Version 1
/// lacked it.
pub const RECORDS_HEADER: &str =
    "# z3cache records v2\tseq\tkind\tdepth\tkey\tcontext\tquery\tverdict\tus\tserved";
pub const RECORDS_HEADER_V1: &str = "# z3cache records v1\tseq\tkind\tdepth\tkey\tcontext\tquery\tverdict\tus";

pub fn unix_ms() -> u128 {
    crate::since_epoch().as_millis()
}

pub fn join_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Appends an `exec.log` line for an invocation that bypasses the proxy.
pub fn record_exec(dir: &Path, args: &[OsString]) {
    let line = format!("{}\t{}\t{}\n", unix_ms(), std::process::id(), join_args(args));
    let result = fs::create_dir_all(dir).and_then(|_| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("exec.log"))?
            .write_all(line.as_bytes())
    });
    if let Err(e) = result {
        log!("cannot write exec.log in {}: {e}", dir.display());
    }
}

pub struct Record<'a> {
    pub seq: u64,
    pub kind: &'a str,
    pub depth: usize,
    pub hashes: Option<Hashes>,
    /// The query's verdict; `None` for a read (written as `-`).
    pub verdict: Option<Verdict>,
    pub us: u128,
    /// `cache` only if the response was written from a stored entry without
    /// consulting a solver; `audited` if it was written from an entry after
    /// an audit agreed with it or could not decide; otherwise `solver`.
    pub served: &'a str,
}

pub struct Recorder {
    dir: PathBuf,
    id: String,
    input: Option<BufWriter<File>>,
    output: Option<BufWriter<File>>,
    records: Option<BufWriter<File>>,
    meta: Vec<(String, String)>,
}

fn open(path: &Path) -> Option<BufWriter<File>> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(f) => Some(BufWriter::with_capacity(1 << 16, f)),
        Err(e) => {
            log!("cannot create {}: {e}", path.display());
            None
        }
    }
}

fn write_or_disable(w: &mut Option<BufWriter<File>>, what: &str, bytes: &[u8]) {
    if let Some(f) = w
        && let Err(e) = f.write_all(bytes) {
            log!("recording {what} failed: {e}");
            *w = None;
        }
}

impl Recorder {
    pub fn create(dir: &Path) -> Option<Recorder> {
        if let Err(e) = fs::create_dir_all(dir) {
            log!("cannot create record directory {}: {e}", dir.display());
            return None;
        }
        let start = unix_ms();
        let id = format!("{start}-{}", std::process::id());
        let path = |ext: &str| dir.join(format!("{id}.{ext}"));
        let mut records = open(&path("tsv"));
        write_or_disable(&mut records, "records", format!("{RECORDS_HEADER}\n").as_bytes());
        let mut r = Recorder {
            dir: dir.to_path_buf(),
            input: open(&path("in")),
            output: open(&path("out")),
            records,
            meta: Vec::new(),
            id,
        };
        r.set("format", crate::FORMAT_VERSION);
        r.set("z3cache_version", env!("CARGO_PKG_VERSION"));
        r.set("pid", std::process::id());
        r.set("start_unix_ms", start);
        if let Ok(cwd) = std::env::current_dir() {
            r.set("cwd", cwd.display());
        }
        r.write_meta();
        Some(r)
    }

    pub fn set(&mut self, key: &str, value: impl std::fmt::Display) {
        let value = value.to_string().replace('\\', "\\\\").replace('\n', "\\n");
        match self.meta.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.meta.push((key.to_string(), value)),
        }
    }

    pub fn input(&mut self, bytes: &[u8]) {
        write_or_disable(&mut self.input, "input", bytes);
    }

    pub fn output(&mut self, bytes: &[u8]) {
        write_or_disable(&mut self.output, "output", bytes);
    }

    /// Takes the input transcript, for a thread that reads from the client.
    pub fn take_input(&mut self) -> Option<BufWriter<File>> {
        self.input.take()
    }

    /// Takes the output transcript, for a thread that writes to the client.
    pub fn take_output(&mut self) -> Option<BufWriter<File>> {
        self.output.take()
    }

    pub fn record(&mut self, r: &Record<'_>) {
        let hex = |f: fn(&Hashes) -> blake3::Hash| {
            r.hashes.map(|h| f(&h).to_hex().to_string()).unwrap_or_else(|| "-".into())
        };
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.seq,
            r.kind,
            r.depth,
            hex(|h| h.key),
            hex(|h| h.context),
            hex(|h| h.query),
            r.verdict.map_or("-", Verdict::as_str),
            r.us,
            r.served
        );
        write_or_disable(&mut self.records, "records", line.as_bytes());
        // Flush per record so a killed session still leaves its records.
        if let Some(f) = &mut self.records {
            let _ = f.flush();
        }
    }

    fn write_meta(&self) {
        let mut text = String::new();
        for (k, v) in &self.meta {
            text.push_str(k);
            text.push('=');
            text.push_str(v);
            text.push('\n');
        }
        let path = self.dir.join(format!("{}.meta", self.id));
        let tmp = self.dir.join(format!("{}.meta.tmp", self.id));
        if let Err(e) = fs::write(&tmp, text).and_then(|_| fs::rename(&tmp, &path)) {
            log!("cannot write {}: {e}", path.display());
        }
    }

    pub fn finish(mut self) {
        self.set("end_unix_ms", unix_ms());
        for (w, what) in [
            (&mut self.input, "input"),
            (&mut self.output, "output"),
            (&mut self.records, "records"),
        ] {
            if let Some(f) = w
                && let Err(e) = f.flush() {
                    log!("recording {what} failed: {e}");
                }
        }
        self.write_meta();
    }
}
