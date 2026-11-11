//! Sample crate for the end-to-end tier: intentional verification failures
//! next to functions that verify.

use vstd::prelude::*;

verus! {

fn inc(a: u8) -> (r: u8)
    requires
        a < 10,
    ensures
        r == a + 1,
{
    a + 1
}

// Each failure has exactly one counterexample, so the model Z3 reports for
// it does not depend on what the solver did before.
fn bad_assert(x: u32)
    requires
        1 < x < 3,
{
    assert(x > 5);
}

fn bad_postcondition(x: u32) -> (r: u32)
    requires
        x == 7,
    ensures
        r > x,
{
    x
}

proof fn zero_is_identity(x: int)
    ensures
        x + 0 == x,
{
}

fn min(a: i64, b: i64) -> (r: i64)
    ensures
        r <= a && r <= b,
{
    if a < b { a } else { b }
}

fn main() {
}

} // verus!
