//! The client's side of a session: its input, read on a thread of its own,
//! and everything written to it (DESIGN sections 10 and 14).

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::sync::mpsc::{self, Receiver};

use crate::lock::KeyLock;
use crate::log;
use crate::record::{flush_or_log, write_or_disable};

/// Chunks of client input (at most 64 KiB each) queued ahead of the session.
const INPUT_QUEUE: usize = 4;

/// What the client-input thread hands to the session.
pub(super) enum Input {
    Data(Vec<u8>),
    End,
}

/// Reads client input on its own thread, so that a solver that dies while
/// the client is quiet ends the session (section 14). The queue is bounded
/// so that a solver that stops reading pushes back on the client through the
/// pipes instead of growing memory without limit; dropping the receiver
/// releases a blocked sender.
pub(super) fn read_client_input() -> Receiver<Input> {
    let (tx, input) = mpsc::sync_channel(INPUT_QUEUE);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match io::stdin().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Input::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    log!("reading stdin failed: {e}");
                    break;
                }
            }
        }
        let _ = tx.send(Input::End);
    });
    input
}

/// The client stopped reading.
pub(super) struct Closed;

/// Everything written to the client: stdout and the recorded output
/// transcript. While an in-flight lock is held, nothing reaches the client,
/// so a client that stops reading cannot keep other sessions waiting:
/// output is kept, and written once the lock is released (section 10).
pub(super) struct ClientOut {
    transcript: Option<BufWriter<File>>,
    lock: Option<KeyLock>,
    held: Vec<u8>,
    /// A write failed: later output is only recorded.
    closed: bool,
}

impl ClientOut {
    pub(super) fn new(transcript: Option<BufWriter<File>>) -> ClientOut {
        ClientOut { transcript, lock: None, held: Vec::new(), closed: false }
    }

    /// Writes bytes to the client and flushes them, or keeps them while a
    /// lock is held.
    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), Closed> {
        if self.lock.is_some() {
            self.held.extend_from_slice(bytes);
            return Ok(());
        }
        self.put(bytes)
    }

    /// Holds `lock` until [`ClientOut::release`].
    pub(super) fn hold(&mut self, lock: KeyLock) {
        self.lock = Some(lock);
    }

    /// Releases the lock, if one is held, then writes what was kept.
    pub(super) fn release(&mut self) -> Result<(), Closed> {
        drop(self.lock.take());
        let held = std::mem::take(&mut self.held);
        self.put(&held)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Closed> {
        if bytes.is_empty() {
            return Ok(());
        }
        write_or_disable(&mut self.transcript, "output", bytes);
        if self.closed {
            return Err(Closed);
        }
        let mut out = io::stdout().lock();
        if out.write_all(bytes).and_then(|_| out.flush()).is_err() {
            self.closed = true;
            return Err(Closed);
        }
        Ok(())
    }

    /// Copies solver output to the client, unchanged, until the solver
    /// closes it (raw bypass).
    pub(super) fn pump(&mut self, rx: Receiver<Vec<u8>>) {
        while let Ok(first) = rx.recv() {
            let mut batch = Vec::new();
            let mut next = Some(first);
            while let Some(line) = next {
                batch.extend_from_slice(&line);
                next = rx.try_recv().ok();
            }
            let _ = self.write(&batch);
        }
    }

    /// Flushes the transcript at the end of the session.
    pub(super) fn finish(&mut self) {
        flush_or_log(&mut self.transcript, "output");
    }
}
