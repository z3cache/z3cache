//! The client's resource counter (DESIGN section 8), and a proof that the
//! `rlimit-count` values given to the client never decrease (AGENTS rule 7).
//!
//! The session changes its counter and computes every `rlimit-count` it
//! gives the client only through the three functions here. Their
//! specifications, and the model of a session at the end of this file, are
//! checked by Verus (`tests/proofs.rs`):
//!
//! ```text
//! verus --crate-type=lib src/rlimit/counter.rs
//! ```
//!
//! Ordinary builds ignore the specifications: they are attributes under
//! `cfg(verus_keep_ghost)`, which only Verus sets.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The counter after a query that cost `cost`, answered by a solver or from
/// the cache.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == spec_add(counter, cost),
))]
pub fn advance(counter: u64, cost: u64) -> u64 {
    counter.saturating_add(cost)
}

/// When bypass begins: how far the client's counter is ahead of `solver`, the
/// live solver's own count at that moment. Zero means raw bypass is safe.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == spec_offset(counter, solver),
))]
pub fn bypass_offset(counter: u64, solver: u64) -> u64 {
    counter.saturating_sub(solver)
}

/// In framed bypass: the value given to the client for the solver's own
/// count `solver`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == spec_add(solver, offset),
))]
pub fn shifted(solver: u64, offset: u64) -> u64 {
    solver.saturating_add(offset)
}

#[cfg(verus_keep_ghost)]
verus! {

/// `a + b`, saturating at `u64::MAX`.
pub open spec fn spec_add(a: u64, b: u64) -> u64 {
    if a + b > u64::MAX { u64::MAX } else { (a + b) as u64 }
}

/// `a - b`, saturating at 0.
pub open spec fn spec_offset(a: u64, b: u64) -> u64 {
    if a > b { (a - b) as u64 } else { 0 }
}

/// What the session is doing with the client's statistics requests.
pub enum Phase {
    /// Answering with the counter, synthesized or written into the solver's
    /// response.
    Caching,
    /// Bypass: forwarding the solver's own count shifted by `offset`. Raw
    /// bypass, chosen when the offset is 0, forwards it unchanged, which is
    /// the same value.
    Bypass { offset: u64 },
}

/// A session as far as `rlimit-count` is concerned.
pub struct Model {
    pub phase: Phase,
    /// The client's counter.
    pub counter: u64,
    /// The live solver's own count, as it would report it now.
    pub solver: u64,
    /// The last value given to the client, 0 before the first.
    pub last: u64,
}

pub enum Event {
    /// A query was answered, by a solver or from the cache, at this cost.
    Query { cost: u64 },
    /// The live solver's own count is now `count`: it did work, or a new
    /// solver was started, whose count begins again at 0.
    Solver { count: u64 },
    /// The client asked for statistics.
    Statistics,
    /// The session left caching for bypass.
    StartBypass,
}

/// What the session can do. In bypass the solver is never replaced, and a
/// solver's own count never decreases (DESIGN section 16, A10), so its count
/// only grows. Raw bypass forced by a command outside the class tables, the
/// one exception to rule 7, is not part of the model.
pub open spec fn possible(m: Model, e: Event) -> bool {
    match (m.phase, e) {
        (Phase::Bypass { .. }, Event::Solver { count }) => count >= m.solver,
        _ => true,
    }
}

/// The value given to the client for a statistics request.
pub open spec fn reported(m: Model) -> u64 {
    match m.phase {
        Phase::Caching => m.counter,
        Phase::Bypass { offset } => spec_add(m.solver, offset),
    }
}

/// The session after an event.
pub open spec fn step(m: Model, e: Event) -> Model {
    match e {
        Event::Query { cost } => Model { counter: spec_add(m.counter, cost), ..m },
        Event::Solver { count } => Model { solver: count, ..m },
        Event::Statistics => Model { last: reported(m), ..m },
        Event::StartBypass => match m.phase {
            Phase::Caching => Model {
                phase: Phase::Bypass { offset: spec_offset(m.counter, m.solver) },
                ..m
            },
            Phase::Bypass { .. } => m,
        },
    }
}

/// The values given to the client over a run of events.
pub open spec fn reports(m: Model, events: Seq<Event>) -> Seq<u64>
    decreases events.len(),
{
    if events.len() == 0 {
        seq![]
    } else {
        let rest = reports(step(m, events[0]), events.drop_first());
        match events[0] {
            Event::Statistics => seq![reported(m)] + rest,
            _ => rest,
        }
    }
}

/// Every event of the run is possible when it happens.
pub open spec fn possible_run(m: Model, events: Seq<Event>) -> bool
    decreases events.len(),
{
    events.len() == 0 || (possible(m, events[0]) && possible_run(
        step(m, events[0]),
        events.drop_first(),
    ))
}

/// Holds at every point of a session: the next value given to the client is
/// at least the last.
pub open spec fn next_at_least_last(m: Model) -> bool {
    m.last <= reported(m)
}

pub open spec fn non_decreasing(s: Seq<u64>) -> bool {
    forall|i: int, j: int| 0 <= i <= j < s.len() ==> s[i] <= s[j]
}

/// One possible event keeps `next_at_least_last`.
pub proof fn step_keeps_next_at_least_last(m: Model, e: Event)
    requires
        next_at_least_last(m),
        possible(m, e),
    ensures
        next_at_least_last(step(m, e)),
{
}

/// Every value given from `m` on is at least `m.last`, and they never
/// decrease.
pub proof fn run_is_non_decreasing(m: Model, events: Seq<Event>)
    requires
        next_at_least_last(m),
        possible_run(m, events),
    ensures
        non_decreasing(reports(m, events)),
        forall|i: int| 0 <= i < reports(m, events).len() ==> m.last <= #[trigger] reports(m, events)[i],
    decreases events.len(),
{
    if events.len() > 0 {
        let next = step(m, events[0]);
        step_keeps_next_at_least_last(m, events[0]);
        run_is_non_decreasing(next, events.drop_first());
    }
}

/// Rule 7: from the start of a session, whatever the client asks and
/// whatever the cache and the solvers answer, the `rlimit-count` values
/// given to the client never decrease.
pub proof fn rule_7(solver: u64, events: Seq<Event>)
    requires
        possible_run(Model { phase: Phase::Caching, counter: 0, solver, last: 0 }, events),
    ensures
        non_decreasing(reports(Model { phase: Phase::Caching, counter: 0, solver, last: 0 }, events)),
{
    run_is_non_decreasing(Model { phase: Phase::Caching, counter: 0, solver, last: 0 }, events);
}

} // verus!
