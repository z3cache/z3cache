//! Bypass (DESIGN section 7): forwarding the client's commands, framed or
//! raw, once caching has stopped.

use std::io::Write;
use std::sync::mpsc::Receiver;

use super::channel::{Output, Section};
use super::client::Input;
use super::{Next, Phase, Session, Stop};
use crate::command::Class;
use crate::log;
use crate::record::Recorder;
use crate::sexp::Command;

impl Session<'_> {
    /// Leaves caching for the rest of the session: notes why, and
    /// materializes.
    fn start_bypass(&mut self, reason: String) -> Result<(), Stop> {
        log!("bypass: {reason}");
        self.bypass_reason = Some(reason);
        self.materialize()
    }

    /// Switches to raw bypass. The bytes for the solver start with the
    /// trigger, the command that caused it, if there is one.
    pub(super) fn raw_bypass(
        &mut self,
        reason: String,
        trigger: Option<Command>,
    ) -> Result<Next, Stop> {
        if self.phase == Phase::Caching {
            self.start_bypass(reason)?;
        }
        Ok(self.go_raw(trigger))
    }

    fn go_raw(&mut self, trigger: Option<Command>) -> Next {
        self.phase = Phase::Raw;
        let mut pending = trigger.map(|c| c.raw).unwrap_or_default();
        pending.extend(self.splitter.take_remaining());
        Next::Raw(pending)
    }

    /// Leaves caching for bypass, framed if the client has been given an
    /// `rlimit-count` ahead of the solver's, which raw bypass could let go
    /// back (rule 7). The trigger and its class are forwarded first.
    pub(super) fn framed_bypass(
        &mut self,
        reason: &str,
        trigger: Option<(Command, Class)>,
    ) -> Result<Next, Stop> {
        self.start_bypass(reason.to_string())?;
        let offset = self.rlimit_offset()?;
        if offset == 0 {
            return Ok(self.go_raw(trigger.map(|(cmd, _)| cmd)));
        }
        self.phase = Phase::Framed { offset };
        match trigger {
            Some((cmd, class)) => self.forward(&cmd, &class, offset),
            None => Ok(Next::Continue),
        }
    }

    /// How far the client's `rlimit-count` is ahead of the live solver's.
    fn rlimit_offset(&mut self) -> Result<u64, Stop> {
        if self.rlimit == 0 {
            return Ok(0);
        }
        let stats = self.ask(self.profile.statistics_command(), Output::Own)?;
        let real = self.profile.parse_rlimit(&stats.lines).unwrap_or(0);
        Ok(self.rlimit.saturating_sub(real))
    }

    /// Framed bypass, command by command: the client's bytes go to the
    /// solver unchanged, and each response is framed by a sentinel so that a
    /// statistics response can be recognised by the command that asked for
    /// it and have `offset` added to its `rlimit-count`.
    pub(super) fn forward(
        &mut self,
        cmd: &Command,
        class: &Class,
        offset: u64,
    ) -> Result<Next, Stop> {
        let output = match class {
            Class::Silent | Class::Push(_) | Class::Pop(_) | Class::SetOption { .. } => {
                self.write_solver(&cmd.raw)?;
                return Ok(Next::Continue);
            }
            // An echo's text is data, and here it shares a response with any
            // earlier errors, which the solver's exit status still reflects.
            Class::Local => Output::Data,
            Class::Statistics => Output::Statistics(Box::new(move |v| v.saturating_add(offset))),
            _ => Output::Response,
        };
        let mut replies = self.exchange(vec![Section::client(&cmd.raw, output)])?;
        self.write_client(&replies.swap_remove(0).text())?;
        Ok(Next::Continue)
    }

    /// Raw bypass: copy bytes verbatim in both directions for the rest of
    /// the session, starting with `pending`. The proxy cannot tell a
    /// statistics response from other output here, so it changes nothing,
    /// and rule 7 is not guaranteed (DESIGN section 8). The session ends when
    /// the solver's output does, even if the client keeps stdin open.
    pub(super) fn raw_pipe(&mut self, pending: Vec<u8>, input: Receiver<Input>) -> i32 {
        log!("raw bypass");
        let live = self.live.take().expect("bypass materializes");
        let mut solver = live.channel.into_solver();
        let mut to_solver = solver.take_stdin();
        let mut in_rec = self.recorder.as_mut().and_then(Recorder::take_input);
        if let Some(w) = &mut to_solver
            && w.write_all(&pending).and_then(|_| w.flush()).is_err()
        {
            to_solver = None;
        }
        if let Some(mut w) = to_solver {
            std::thread::spawn(move || {
                while let Ok(Input::Data(data)) = input.recv() {
                    if let Some(r) = &mut in_rec {
                        let _ = r.write_all(&data).and_then(|_| r.flush());
                    }
                    if w.write_all(&data).and_then(|_| w.flush()).is_err() {
                        break;
                    }
                }
                // Dropping `w` closes the solver's stdin.
            });
        }
        // Raw output is not checked for errors: it cannot be told apart from
        // echo text. The solver's own exit status reports them.
        if let Some(rx) = solver.take_lines() {
            self.out.pump(rx);
        }
        solver.wait()
    }
}
