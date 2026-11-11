//! Sample crate for the end-to-end tier: several modules that use each
//! other, so Verus runs several solver sessions, in parallel when it has
//! several threads.

use vstd::prelude::*;

verus! {

mod bits {
    use vstd::prelude::*;

    pub open spec fn is_even(x: nat) -> bool {
        x % 2 == 0
    }

    pub fn even(x: u32) -> (r: bool)
        ensures
            r == is_even(x as nat),
    {
        x % 2 == 0
    }

    pub proof fn double_is_even(x: nat)
        ensures
            is_even(2 * x),
    {
    }
}

mod ranges {
    use vstd::prelude::*;

    pub struct Range {
        pub lo: u32,
        pub hi: u32,
    }

    impl Range {
        pub open spec fn wf(&self) -> bool {
            self.lo <= self.hi
        }

        pub open spec fn len(&self) -> nat {
            (self.hi - self.lo) as nat
        }

        pub open spec fn contains(&self, x: int) -> bool {
            self.lo <= x < self.hi
        }
    }

    pub fn width(r: &Range) -> (n: u32)
        requires
            r.wf(),
        ensures
            n == r.len(),
    {
        r.hi - r.lo
    }

    pub fn clamp(r: &Range, x: u32) -> (y: u32)
        requires
            r.wf(),
            r.lo < r.hi,
        ensures
            r.contains(y as int),
            r.contains(x as int) ==> y == x,
    {
        if x < r.lo {
            r.lo
        } else if x >= r.hi {
            r.hi - 1
        } else {
            x
        }
    }
}

mod search {
    use vstd::prelude::*;

    pub fn find(v: &Vec<u32>, x: u32) -> (r: Option<usize>)
        ensures
            r matches Some(i) ==> i < v.len() && v[i as int] == x,
            r is None ==> forall|j: int| 0 <= j < v.len() ==> v[j] != x,
    {
        let mut i: usize = 0;
        while i < v.len()
            invariant
                i <= v.len(),
                forall|j: int| 0 <= j < i ==> v[j] != x,
            decreases v.len() - i,
        {
            if v[i] == x {
                return Some(i);
            }
            i = i + 1;
        }
        None
    }
}

mod counting {
    use vstd::prelude::*;
    use crate::bits::is_even;

    pub fn count_even(v: &Vec<u32>) -> (n: usize)
        ensures
            n <= v.len(),
    {
        let mut n: usize = 0;
        let mut i: usize = 0;
        while i < v.len()
            invariant
                i <= v.len(),
                n <= i,
            decreases v.len() - i,
        {
            if crate::bits::even(v[i]) {
                n = n + 1;
            }
            i = i + 1;
        }
        n
    }

    pub proof fn evens(x: nat)
        ensures
            is_even(2 * x + 4),
    {
        crate::bits::double_is_even(x + 2);
        assert(2 * x + 4 == 2 * (x + 2));
    }
}

mod sets {
    use vstd::prelude::*;

    pub proof fn insert_contains(s: Set<int>, x: int)
        ensures
            s.insert(x).contains(x),
            s.insert(x).remove(x) =~= s.remove(x),
    {
    }

    pub proof fn union_sub(a: Set<int>, b: Set<int>)
        ensures
            a.subset_of(a.union(b)),
            b.subset_of(a.union(b)),
    {
    }
}

mod top {
    use vstd::prelude::*;
    use crate::ranges::Range;

    pub fn first_width(rs: &Vec<Range>) -> (n: u32)
        requires
            rs.len() > 0,
            rs[0].wf(),
        ensures
            n == rs[0].len(),
    {
        crate::ranges::width(&rs[0])
    }
}

fn main() {
}

} // verus!
