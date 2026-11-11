//! Sample crate for the end-to-end tier: varied proofs that all verify.
//! Each module is a separate solver session.

use vstd::prelude::*;

verus! {

mod arith {
    use vstd::prelude::*;

    pub open spec fn sq(x: int) -> int {
        x * x
    }

    pub proof fn sq_nonneg(x: int)
        ensures
            sq(x) >= 0,
    {
        assert(x * x >= 0) by (nonlinear_arith);
    }

    pub proof fn mul_le(a: int, b: int, c: int)
        requires
            0 <= a <= b,
            0 <= c,
        ensures
            a * c <= b * c,
    {
        assert(a * c <= b * c) by (nonlinear_arith)
            requires
                0 <= a <= b,
                0 <= c,
        ;
    }

    pub fn add_small(a: u32, b: u32) -> (r: u32)
        requires
            a < 1000,
            b < 1000,
        ensures
            r == a + b,
    {
        a + b
    }

    pub fn max3(a: u64, b: u64, c: u64) -> (r: u64)
        ensures
            r >= a && r >= b && r >= c,
            r == a || r == b || r == c,
    {
        let m = if a > b { a } else { b };
        if m > c { m } else { c }
    }
}

mod loops {
    use vstd::prelude::*;

    pub fn sum_to(n: u32) -> (r: u64)
        requires
            n < 1000,
        ensures
            r <= 1000 * 1000,
    {
        let mut i: u32 = 0;
        let mut s: u64 = 0;
        while i < n
            invariant
                i <= n,
                n < 1000,
                s <= i * 1000,
            decreases n - i,
        {
            s = s + i as u64;
            i = i + 1;
        }
        s
    }

    pub fn count_down(n: u32) -> (r: u32)
        ensures
            r == 0,
    {
        let mut k = n;
        while k > 0
            invariant
                k <= n,
            decreases k,
        {
            k = k - 1;
        }
        k
    }
}

mod recursion {
    use vstd::prelude::*;

    pub open spec fn triangle(n: nat) -> nat
        decreases n,
    {
        if n == 0 {
            0
        } else {
            n + triangle((n - 1) as nat)
        }
    }

    pub proof fn triangle_monotone(i: nat, j: nat)
        requires
            i <= j,
        ensures
            triangle(i) <= triangle(j),
        decreases j,
    {
        if i < j {
            triangle_monotone(i, (j - 1) as nat);
        }
    }

    pub proof fn triangle_three()
        ensures
            triangle(3) == 6,
    {
        reveal_with_fuel(triangle, 4);
    }
}

mod quant {
    use vstd::prelude::*;

    pub open spec fn succ(x: int) -> int {
        x + 1
    }

    pub open spec fn all_pos(s: Seq<int>) -> bool {
        forall|i: int| 0 <= i < s.len() ==> #[trigger] s[i] > 0
    }

    pub proof fn all_pos_prefix(s: Seq<int>, j: int)
        requires
            all_pos(s),
            0 <= j <= s.len(),
        ensures
            all_pos(s.subrange(0, j)),
    {
        assert forall|i: int| 0 <= i < j implies #[trigger] s.subrange(0, j)[i] > 0 by {
            assert(s[i] > 0);
        }
    }

    pub proof fn has_bigger(x: int)
        ensures
            exists|y: int| #[trigger] succ(y) > x + 1,
    {
        assert(succ(x + 1) > x + 1);
    }
}

mod shapes {
    use vstd::prelude::*;

    // Tuple variants: Verus emits the accessors of named fields that differ
    // between variants in an order that varies from run to run, which
    // would change the cache keys of this module between identical runs.
    pub enum Shape {
        Square(u32),
        Rect(u32, u32),
    }

    impl Shape {
        pub open spec fn area(&self) -> int {
            match self {
                Shape::Square(side) => *side as int * *side as int,
                Shape::Rect(w, h) => *w as int * *h as int,
            }
        }
    }

    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    impl Point {
        pub open spec fn norm1(&self) -> int {
            (if self.x >= 0 { self.x as int } else { -self.x }) + (if self.y >= 0 {
                self.y as int
            } else {
                -self.y
            })
        }
    }

    pub fn is_square(s: &Shape) -> (r: bool)
        ensures
            r == (s is Square),
    {
        match s {
            Shape::Square(..) => true,
            Shape::Rect(..) => false,
        }
    }

    pub fn shift(p: Point) -> (q: Point)
        requires
            0 <= p.x < 1000,
        ensures
            q.x == p.x + 1,
            q.y == p.y,
            q.norm1() == p.norm1() + 1,
    {
        Point { x: p.x + 1, y: p.y }
    }

    pub proof fn square_area(side: u32)
        ensures
            Shape::Square(side).area() == Shape::Rect(side, side).area(),
    {
    }
}

mod collections {
    use vstd::prelude::*;

    pub proof fn push_then_index(s: Seq<int>, x: int)
        ensures
            s.push(x).len() == s.len() + 1,
            s.push(x)[s.len() as int] == x,
            s.push(x).subrange(0, s.len() as int) =~= s,
    {
    }

    pub proof fn insert_then_get(m: Map<int, int>, k: int, v: int)
        ensures
            m.insert(k, v).contains_key(k),
            m.insert(k, v)[k] == v,
            m.insert(k, v).dom() =~= m.dom().insert(k),
    {
    }

    pub fn zeros(n: usize) -> (v: Vec<u64>)
        requires
            n < 100,
        ensures
            v.len() == n,
            forall|i: int| 0 <= i < n ==> v[i] == 0,
    {
        let mut v = Vec::new();
        let mut i: usize = 0;
        while i < n
            invariant
                i <= n,
                v.len() == i,
                forall|j: int| 0 <= j < i ==> v[j] == 0,
            decreases n - i,
        {
            v.push(0);
            i = i + 1;
        }
        v
    }
}

fn main() {
}

} // verus!
