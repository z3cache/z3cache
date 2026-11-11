//! Statistics synthesis (DESIGN section 8).
//!
//! Verus reads `rlimit-count` from `(get-info :all-statistics)` and
//! subtracts successive values, so the value given to the client must never
//! decrease (AGENTS rule 7). The proxy owns a virtual counter: it advances by
//! the recorded cost of each query, whether the answer came from the solver
//! or the cache.

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

/// Replaces the `rlimit-count` value in a statistics response.
pub fn rewrite(lines: &mut [Vec<u8>], value: u64) {
    for l in lines.iter_mut() {
        if let Some((start, end)) = value_span(l) {
            l.splice(start..end, value.to_string().into_bytes());
        }
    }
}

/// Adds `offset` to every `rlimit-count` value in one output line (bypass).
pub fn shift_line(line: &mut Vec<u8>, offset: u64) {
    if offset == 0 {
        return;
    }
    if let Some((start, end)) = value_span(line)
        && let Some(v) = std::str::from_utf8(&line[start..end])
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            line.splice(start..end, (v + offset).to_string().into_bytes());
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
        s.split_inclusive('\n').map(|l| l.as_bytes().to_vec()).collect()
    }

    const REAL: &str = "(:max-memory   0.19\n :memory       0.19\n :num-allocs   451\n :rlimit-count 1234)\n";

    #[test]
    fn parse_and_rewrite() {
        let mut l = lines(REAL);
        assert_eq!(parse(&l), Some(1234));
        rewrite(&mut l, 99);
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
    fn shift() {
        let mut l = b" :rlimit-count 10)\n".to_vec();
        shift_line(&mut l, 5);
        assert_eq!(l, b" :rlimit-count 15)\n");
        let mut other = b"unsat\n".to_vec();
        shift_line(&mut other, 5);
        assert_eq!(other, b"unsat\n");
    }
}
