//! The live solver: materializing the path in it, framed exchanges, and
//! judging what it prints (DESIGN sections 7 and 8).

use std::collections::BTreeMap;

use super::channel::{Channel, Gone, Output, Reply, Section, push_line};
use super::{Session, Stop};
use crate::config::{Lifetime, Mode};
use crate::log;
use crate::state::{Replay, State};

/// A running solver and how much of the path it has received.
pub(super) struct Live<'a> {
    pub(super) channel: Channel<'a>,
    /// `sent[i]`: number of replay entries of frame `i` already sent. The
    /// solver's stack depth is `sent.len() - 1`.
    pub(super) sent: Vec<usize>,
    /// The option values the solver has received. `pop` does not undo them
    /// (assumption A1), so they can differ from the client's.
    pub(super) options: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Live<'_> {
    /// Adds to `batch` the client's command for every option whose value in
    /// the solver differs, and notes the new value as sent. Returns whether
    /// it added any.
    fn sync_options(&mut self, state: &State, batch: &mut Vec<u8>) -> bool {
        let mut added = false;
        for (name, value, command) in state.options() {
            if self.options.get(name).map(Vec::as_slice) != Some(value) {
                push_line(batch, command);
                self.options.insert(name.to_vec(), value.to_vec());
                added = true;
            }
        }
        added
    }
}

/// Adds to `batch` the part of the path not yet sent, from the cursor `sent`
/// (as in [`Live::sent`]), and advances the cursor: `push` for each frame
/// not yet opened, and each unsent replay entry through `entry`.
pub(super) fn push_unsent(
    state: &State,
    push: &[u8],
    sent: &mut Vec<usize>,
    batch: &mut Vec<u8>,
    mut entry: impl FnMut(&mut Vec<u8>, &Replay),
) {
    for i in 0..state.frame_count() {
        if i >= sent.len() {
            push_line(batch, push);
            sent.push(0);
        }
        let replay = state.replay(i);
        for r in &replay[sent[i]..] {
            entry(batch, r);
        }
        sent[i] = replay.len();
    }
}

impl Session<'_> {
    /// Discards solver frames the client has popped. `pop` does not undo
    /// options (assumption A1), and option commands the solver had not yet
    /// received are discarded with their frame, so the solver is then sent
    /// the client's command for every option whose value differs. This puts
    /// the solver's options where a real solver's would be at this point,
    /// before any later push, assertion or replayed query.
    pub(super) fn pop_live(&mut self) -> Result<(), Stop> {
        let frames = self.state.frame_count();
        let Some(live) = &mut self.live else {
            return Ok(());
        };
        if live.sent.len() <= frames {
            return Ok(());
        }
        if self.cfg.lifetime == Lifetime::Fresh {
            // Section 7: the next miss gets a solver that has seen only the
            // path at that point.
            return self.end_solver().1;
        }
        let mut batch = Vec::new();
        while live.sent.len() > frames {
            live.sent.pop();
            push_line(&mut batch, self.profile.pop_command());
        }
        live.sync_options(&self.state, &mut batch);
        self.write_solver(&batch)
    }

    /// Starts the solver if needed and sends it the part of the path it has
    /// not seen, then any option whose value in the solver differs from the
    /// client's.
    pub(super) fn materialize(&mut self) -> Result<(), Stop> {
        if self.cfg.mode == Mode::Enforce {
            let msg = self
                .profile
                .error_line("z3cache: cache miss in enforce mode");
            let _ = self.write_client(&msg);
            log!("enforce: cache miss");
            return Err(Stop(1));
        }
        if self.live.is_none() {
            self.start_solver()?;
        }
        let sections = self.unsent_sections();
        if sections.is_empty() {
            return Ok(());
        }
        for reply in self.exchange(sections)? {
            if reply.output.passed_on() {
                self.write_client(&reply.text())?;
            }
        }
        Ok(())
    }

    /// After [`Session::materialize`], before the client's next read or
    /// bypassed command: if a `pop` came after the latest query, Z3 has
    /// discarded that query's result, but the live solver may not have had
    /// the popped frame, or may be a new one that has just re-run the query.
    /// It is then made to discard the result too, so that it answers as Z3
    /// would have ("model is not available") rather than from a result the
    /// client can no longer see (section 7).
    pub(super) fn discard_popped_result(&mut self) -> Result<(), Stop> {
        if !self.state.popped_since_query() || self.live.is_none() {
            return Ok(());
        }
        let discard = self.profile.discard_result_commands();
        self.write_solver(discard)
    }

    fn start_solver(&mut self) -> Result<(), Stop> {
        match Channel::spawn(self.real, self.args, self.framing.clone()) {
            Ok(channel) => {
                self.counts.solver_starts += 1;
                self.live = Some(Live {
                    channel,
                    sent: vec![0],
                    options: BTreeMap::new(),
                });
                Ok(())
            }
            Err(e) => {
                eprintln!("z3cache: cannot start {}: {e}", self.real.display());
                log!("cannot start {}: {e}", self.real.display());
                Err(Stop(127))
            }
        }
    }

    /// What [`Session::materialize`] sends, as sections. Queries answered
    /// from the cache are re-run, each in a section of its own so that its
    /// answer can be compared with the one served (a difference is a replay
    /// mismatch); all other output, such as errors, is passed on complete.
    fn unsent_sections(&mut self) -> Vec<Section> {
        let live = self.live.as_mut().expect("materialize starts a solver");
        let mut sections = Vec::new();
        let mut batch = Vec::new();
        let push = self.profile.push_command();
        push_unsent(&self.state, push, &mut live.sent, &mut batch, |batch, r| {
            if let Some(served) = &r.served {
                sections.push(Section::lines(std::mem::take(batch), Output::Response));
                sections.push(Section::command(&r.command, Output::Replay(served.clone())));
            } else {
                push_line(batch, &r.command);
            }
            if let Some((name, value)) = &r.option {
                live.options.insert(name.clone(), value.clone());
            }
        });
        // Replaying the path in order, with options reconciled at each pop
        // (pop_live), leaves the solver with the client's options. A
        // difference here would be a bug; log it and correct it rather than
        // answer under the wrong settings.
        let replayed = batch.len();
        if live.sync_options(&self.state, &mut batch) {
            log!(
                "option drift: {}",
                String::from_utf8_lossy(&batch[replayed..]).trim_end()
            );
        }
        if !batch.is_empty() || !sections.is_empty() {
            sections.push(Section::lines(batch, Output::Response));
        }
        sections
    }

    /// [`Session::exchange`] for one command.
    pub(super) fn ask(&mut self, cmd: &[u8], output: Output) -> Result<Reply, Stop> {
        let mut replies = self.exchange(vec![Section::command(cmd, output)])?;
        Ok(replies.swap_remove(0))
    }

    /// Sends sections to the live solver and returns their replies, each
    /// judged by [`Session::absorb`]. Ends the session if the solver goes
    /// away.
    pub(super) fn exchange(&mut self, sections: Vec<Section>) -> Result<Vec<Reply>, Stop> {
        let live = self.live.as_mut().expect("exchange needs a live solver");
        match live.channel.exchange(sections) {
            Ok(mut replies) => {
                for r in &mut replies {
                    self.absorb(r);
                }
                Ok(replies)
            }
            Err(gone) => Err(self.gone(gone)),
        }
    }

    /// Buffers bytes for the live solver, which reads them with the next
    /// exchange. Ends the session if the solver has gone away.
    pub(super) fn write_solver(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        let live = self
            .live
            .as_mut()
            .expect("write_solver needs a live solver");
        live.channel.write(bytes).map_err(|gone| self.gone(gone))
    }

    /// Judges a reply from a solver: the one place where an error stops
    /// storing (rule 6) and, in reply to the client, fails the session
    /// (section 7); where statistics get the client's counter (section 8);
    /// and where re-run answers are compared with those served.
    fn absorb(&mut self, reply: &mut Reply) {
        match &reply.output {
            Output::Statistics(edit) => self.profile.edit_rlimit(&mut reply.lines, &**edit),
            Output::Replay(served) => {
                let got = reply.lines.concat();
                if got != *served {
                    self.counts.replay_mismatches += 1;
                    self.no_store = true;
                    log!(
                        "replay mismatch: served {:?}, solver now says {:?}",
                        String::from_utf8_lossy(served),
                        String::from_utf8_lossy(&got)
                    );
                }
            }
            _ => {}
        }
        let Some(line) = reply.error_line() else {
            return;
        };
        if reply.output.passed_on() {
            if !self.errored {
                log!(
                    "solver reported an error: {}",
                    String::from_utf8_lossy(line)
                );
            }
            self.errored = true;
        }
        self.no_store = true;
    }

    /// The solver went away (section 14): passes on what it said in reply
    /// to the client's commands, then ends the session.
    pub(super) fn gone(&mut self, Gone(mut replies): Gone) -> Stop {
        for r in &mut replies {
            self.absorb(r);
        }
        for r in replies.iter().filter(|r| r.output.passed_on()) {
            if let Err(stop) = self.write_client(&r.text()) {
                return stop;
            }
        }
        log!("solver exited unexpectedly");
        Stop(self.shutdown())
    }

    /// Closes the solver's input, passes on its remaining output, and waits.
    /// Returns 0 if no solver was started.
    pub(super) fn shutdown(&mut self) -> i32 {
        self.end_solver().0
    }

    /// Ends the live solver, if any, passing on anything it still had to
    /// say (such as an error for a command it received). Returns its exit
    /// code (0 if there was none) and the result of passing that on.
    pub(super) fn end_solver(&mut self) -> (i32, Result<(), Stop>) {
        let Some(live) = self.live.take() else {
            return (0, Ok(()));
        };
        let (mut rest, code) = live.channel.finish();
        self.absorb(&mut rest);
        (code, self.write_client(&rest.text()))
    }
}
