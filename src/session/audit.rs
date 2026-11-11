//! Audits (DESIGN section 13): a sampled hit is re-solved in a fresh solver
//! before it is answered.

use std::time::Instant;

use super::channel::{Channel, Output, Section, push_line};
use super::live::push_unsent;
use super::{Session, Stop};
use crate::log;
use crate::sexp::Command;
use crate::state::{Hashes, Verdict};
use crate::store::Entry;

/// A number in [0, 1) derived from the session nonce and `what`.
pub(super) fn sample(nonce: &[u8; 32], what: &[u8]) -> f64 {
    let mut h = blake3::Hasher::new();
    h.update(nonce);
    h.update(what);
    let n = u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().expect("8 bytes"));
    (n >> 11) as f64 / (1u64 << 53) as f64
}

impl Session<'_> {
    /// A query hit chosen for audit (section 13): re-solve it in a separate
    /// fresh solver given the whole current path, before answering. The
    /// audit never changes the outcome of the session except to correct a
    /// contradicted entry.
    pub(super) fn audited_hit(&mut self, cmd: &Command, h: Hashes, entry: Entry, start: Instant) -> Result<(), Stop> {
        self.counts.audits += 1;
        let cached = Verdict::of(&entry.response);
        let key_hex = h.key.to_hex();
        let contradiction = match self.audit(&cmd.canonical) {
            Err(e) => {
                self.counts.audit_failures += 1;
                log!("audit failed ({e}); answering from the cache");
                None
            }
            Ok((response, _)) if Verdict::of(&response) == cached => None,
            Ok((response, delta)) => {
                let now = Verdict::of(&response);
                if cached.is_definite() && now.is_definite() {
                    Some((response, delta, now))
                } else {
                    self.counts.audit_instabilities += 1;
                    log!("audit: instability for {key_hex}: cached {cached}, now {now}; keeping the entry");
                    None
                }
            }
        };
        let Some((response, delta, now)) = contradiction else {
            return self.answer_from_entry(cmd, h, entry, start, false);
        };
        self.counts.audit_contradictions += 1;
        log!("audit: CONTRADICTION for {key_hex}: cached {cached}, now {now}; entry deleted");
        self.store.delete(h.key.as_bytes());
        // Answered by a solver: a miss (rule 10).
        self.counts.misses += 1;
        self.rlimit += delta;
        self.state.append_query(cmd.canonical.as_slice(), response.as_slice(), now);
        self.write_client(&response)?;
        self.record(true, Some(h), Some(now), start, "solver");
        Ok(())
    }

    /// Solves `query` in a new solver after the whole current path. Returns
    /// the response and its resource cost. Unlike [`Session::materialize`],
    /// queries on the path are re-run without sentinels, and any error
    /// fails the audit.
    fn audit(&mut self, query: &[u8]) -> Result<(Vec<u8>, u64), String> {
        let mut path = Vec::new();
        let push = self.profile.push_command();
        push_unsent(&self.state, push, &mut vec![0], &mut path, |batch, r| push_line(batch, &r.command));
        let stats = self.profile.statistics_command();
        let sections = vec![
            Section::lines(path, Output::Own),
            Section::command(stats, Output::Own),
            Section::command(query, Output::Own),
            Section::command(stats, Output::Own),
        ];
        let mut solver =
            Channel::spawn(self.real, self.args, self.framing.clone()).map_err(|e| format!("cannot start: {e}"))?;
        let result = solver.exchange(sections);
        let _ = solver.finish();
        let Ok(mut replies) = result else {
            return Err("the audit solver exited".to_string());
        };
        if replies.iter().any(|r| r.error.is_some()) {
            return Err("the audit solver reported an error".to_string());
        }
        let before = self.profile.parse_rlimit(&replies[1].lines).unwrap_or(0);
        let after = self.profile.parse_rlimit(&replies[3].lines).unwrap_or(0);
        Ok((replies.swap_remove(2).text(), after.saturating_sub(before)))
    }
}
