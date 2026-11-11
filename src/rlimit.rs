//! Statistics synthesis (DESIGN section 8).
//!
//! Verus reads `rlimit-count` from `(get-info :all-statistics)` and
//! subtracts successive values, so the value given to the client must never
//! decrease (AGENTS rule 7). The proxy owns a virtual counter: it advances by
//! the recorded cost of each query, whether the answer came from the solver
//! or the cache.

pub mod counter;

const KEY: &[u8] = b":rlimit-count";

/// The `rlimit-count` value in a statistics response, if present.
pub fn parse(lines: &[Vec<u8>]) -> Option<u64> {
    lines.iter().find_map(|l| {
        let (start, end) = value_span(l)?;
        std::str::from_utf8(&l[start..end]).ok()?.parse().ok()
    })
}

/// Byte range of the number after `:rlimit-count` in one line.
fn value_span(line: &[u8]) -> Option<(usize, usize)> {
    let at = line.windows(KEY.len()).position(|w| w == KEY)?;
    let mut start = at + KEY.len();
    if !line.get(start).is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    while line.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    let mut end = start;
    while line.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    (end > start).then_some((start, end))
}

/// Changes the `rlimit-count` value in the statistics response among
/// `lines` to `f` of it. The response starts at the first line that opens a
/// keyword list; lines before it, such as errors reported for earlier
/// commands, are left alone.
pub fn edit(lines: &mut [Vec<u8>], f: &dyn Fn(u64) -> u64) {
    let Some(start) = lines.iter().position(|l| l.starts_with(b"(:")) else {
        return;
    };
    for l in &mut lines[start..] {
        if let Some((start, end)) = value_span(l)
            && let Some(v) = std::str::from_utf8(&l[start..end])
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        {
            l.splice(start..end, f(v).to_string().into_bytes());
        }
    }
}

/// The statistics response given when no solver is running.
pub fn synthesize(value: u64) -> Vec<u8> {
    format!("(:rlimit-count {value})\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<Vec<u8>> {
        s.split_inclusive('\n')
            .map(|l| l.as_bytes().to_vec())
            .collect()
    }

    const REAL: &str =
        "(:max-memory   0.19\n :memory       0.19\n :num-allocs   451\n :rlimit-count 1234)\n";

    #[test]
    fn parse_and_edit() {
        let mut l = lines(REAL);
        assert_eq!(parse(&l), Some(1234));
        edit(&mut l, &|_| 99);
        assert_eq!(l.concat(), REAL.replace("1234", "99").into_bytes());
        assert_eq!(parse(&lines("(:rlimit-count 7)\n")), Some(7));
        assert_eq!(parse(&lines("(:rlimit-counts 7)\n")), None);
        assert_eq!(parse(&lines("sat\n")), None);
    }

    #[test]
    fn synthesized_response_parses() {
        assert_eq!(synthesize(42), b"(:rlimit-count 42)\n");
        assert_eq!(parse(&lines("(:rlimit-count 42)\n")), Some(42));
    }

    #[test]
    fn edit_changes_only_the_statistics_response() {
        let mut l = lines(":rlimit-count 10\n(:max-memory 0.19\n :rlimit-count 10)\n");
        edit(&mut l, &|v| v + 5);
        assert_eq!(
            l.concat(),
            b":rlimit-count 10\n(:max-memory 0.19\n :rlimit-count 15)\n"
        );
        let mut other = lines("unsat\n");
        edit(&mut other, &|v| v + 5);
        assert_eq!(other.concat(), b"unsat\n");
    }
}
