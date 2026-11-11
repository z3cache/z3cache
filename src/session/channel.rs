//! Framed exchanges with a solver (DESIGN section 7).
//!
//! Each section of commands sent to a solver is followed by a private
//! sentinel `echo`; the lines the solver prints before the sentinel are that
//! section's reply. The section says what its output is, which decides
//! whether the reply is checked for errors.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use crate::profile::SolverProfile;
use crate::solver::Solver;

/// What a section's output is, and so what becomes of it (section 7).
pub(super) enum Output {
    /// The solver's response to the client's command: checked for errors,
    /// and passed on.
    Response,
    /// A statistics response for the client: checked, its `rlimit-count`
    /// changed by the function (section 8), and passed on.
    Statistics(Box<dyn Fn(u64) -> u64>),
    /// Text the client asked for with a local command in framed bypass:
    /// data, never checked, and passed on.
    Data,
    /// The proxy's own command: checked, never passed on.
    Own,
    /// A cached query re-run during replay, with the response served for
    /// it: compared, not checked, never passed on.
    Replay(Vec<u8>),
}

impl Output {
    fn checked(&self) -> bool {
        matches!(self, Output::Response | Output::Statistics(_) | Output::Own)
    }

    /// True if the reply goes to the client.
    pub(super) fn passed_on(&self) -> bool {
        matches!(
            self,
            Output::Response | Output::Statistics(_) | Output::Data
        )
    }
}

/// Commands for a solver, and what their output is.
pub(super) struct Section {
    /// The commands exactly as the solver receives them.
    bytes: Vec<u8>,
    /// The sentinel follows `bytes` on the same line. Used for the client's
    /// bytes in framed bypass, so that framing adds no line to what the
    /// solver reads and line numbers in its messages stay as they were.
    inline: bool,
    output: Output,
}

impl Section {
    /// Commands, each on a line of its own.
    pub(super) fn lines(bytes: Vec<u8>, output: Output) -> Section {
        Section {
            bytes,
            inline: false,
            output,
        }
    }

    /// One command on a line of its own.
    pub(super) fn command(cmd: &[u8], output: Output) -> Section {
        let mut bytes = Vec::with_capacity(cmd.len() + 1);
        push_line(&mut bytes, cmd);
        Section::lines(bytes, output)
    }

    /// The client's bytes, as sent.
    pub(super) fn client(raw: &[u8], output: Output) -> Section {
        Section {
            bytes: raw.to_vec(),
            inline: true,
            output,
        }
    }
}

/// What the solver printed for a section.
pub(super) struct Reply {
    /// Each line including its newline; the last may lack one at end of
    /// output.
    pub(super) lines: Vec<Vec<u8>>,
    /// Index in `lines` of the first error, if the output is checked.
    pub(super) error: Option<usize>,
    pub(super) output: Output,
}

impl Reply {
    pub(super) fn text(&self) -> Vec<u8> {
        self.lines.concat()
    }

    /// The first error line, without its newline.
    pub(super) fn error_line(&self) -> Option<&[u8]> {
        let line = &self.lines[self.error?];
        Some(line.strip_suffix(b"\n").unwrap_or(line))
    }
}

/// The solver went away before every section was answered: the replies
/// read until then, the last of them partial. Empty if the solver could not
/// be written to.
pub(super) struct Gone(pub(super) Vec<Reply>);

/// The session's sentinel and how to read solver output.
#[derive(Clone)]
pub(super) struct Framing<'a> {
    profile: &'a dyn SolverProfile,
    command: Vec<u8>,
    lines: Vec<Vec<u8>>,
}

impl<'a> Framing<'a> {
    pub(super) fn new(profile: &'a dyn SolverProfile, sentinel: &str) -> Framing<'a> {
        Framing {
            profile,
            command: profile.sentinel_command(sentinel),
            lines: profile.sentinel_lines(sentinel),
        }
    }

    /// Classifies solver output: the one place it is checked for errors.
    pub(super) fn reply(&self, lines: Vec<Vec<u8>>, output: Output) -> Reply {
        let error = if output.checked() {
            lines.iter().position(|l| self.profile.is_error_line(l))
        } else {
            None
        };
        Reply {
            lines,
            error,
            output,
        }
    }
}

/// A running solver whose output is read in framed sections.
pub(super) struct Channel<'a> {
    solver: Solver,
    framing: Framing<'a>,
}

impl<'a> Channel<'a> {
    pub(super) fn spawn(
        real: &Path,
        args: &[OsString],
        framing: Framing<'a>,
    ) -> io::Result<Channel<'a>> {
        Ok(Channel {
            solver: Solver::spawn(real, args)?,
            framing,
        })
    }

    /// Buffers bytes for the solver without a sentinel. Whatever they make
    /// it print is read with the next section.
    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), Gone> {
        self.solver.write(bytes).map_err(|_| Gone(Vec::new()))
    }

    /// Sends the sections in one batch, each followed by the sentinel, and
    /// returns one reply per section.
    pub(super) fn exchange(&mut self, sections: Vec<Section>) -> Result<Vec<Reply>, Gone> {
        let mut batch = Vec::new();
        for s in &sections {
            batch.extend_from_slice(&s.bytes);
            batch.extend_from_slice(&self.framing.command);
            if !s.inline {
                batch.push(b'\n');
            }
        }
        if self.solver.send(&batch).is_err() {
            return Err(Gone(Vec::new()));
        }
        let mut replies = Vec::with_capacity(sections.len());
        for s in sections {
            let r = self.solver.read_until(&self.framing.lines);
            replies.push(self.framing.reply(r.lines, s.output));
            if r.eof {
                return Err(Gone(replies));
            }
        }
        Ok(replies)
    }

    pub(super) fn has_exited(&mut self) -> bool {
        self.solver.has_exited()
    }

    /// Closes the solver's input and waits for it. Returns what it still
    /// printed, as the response to the client's commands, and its exit
    /// code.
    pub(super) fn finish(mut self) -> (Reply, i32) {
        let (rest, code) = self.solver.finish();
        (self.framing.reply(rest, Output::Response), code)
    }

    /// The solver itself, for raw bypass.
    pub(super) fn into_solver(self) -> Solver {
        self.solver
    }
}

/// Adds a command and its newline to a batch for a solver.
pub(super) fn push_line(batch: &mut Vec<u8>, cmd: &[u8]) {
    batch.extend_from_slice(cmd);
    batch.push(b'\n');
}
