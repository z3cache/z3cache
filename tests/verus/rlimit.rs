//! Sample crate for the end-to-end tier: one function exceeds a small
//! resource limit, the others verify.

use vstd::prelude::*;

verus! {

pub open spec fn above(s: Seq<int>, a: int) -> bool {
    forall|i: int| 0 <= i < s.len() ==> #[trigger] s[i] > a
}

fn double(a: u32) -> (r: u32)
    requires
        a < 100,
    ensures
        r == a * 2,
{
    a + a
}

// Far more work than the limit allows: the limit is exceeded on every run.
#[verifier::rlimit(0.0001)]
proof fn too_expensive(s: Seq<int>, a: int, b: int)
    requires
        s.len() > 10,
        above(s, a),
        a > b,
    ensures
        s.subrange(2, 8).push(a + 1).len() == 7,
        above(s.subrange(2, 8), b),
{
}

proof fn cheap(s: Seq<int>, a: int)
    requires
        above(s, a),
        s.len() > 0,
    ensures
        s[0] > a,
{
}

fn main() {
}

} // verus!
