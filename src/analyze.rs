//! `z3cache analyze`: compare recorded runs (DESIGN section 12).
//!
//! Each argument is one recording directory, normally one Verus run. The
//! report shows what each run cost, and for every run after the first, what
//! an exact-key cache filled by all earlier runs (with the default `unsat`
//! store policy) would have answered.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::record::{RECORDS_HEADER, RECORDS_HEADER_V1};

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub kind: String,
    pub key: String,
    pub context: String,
    pub query: String,
    pub verdict: String,
    pub us: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionData {
    pub meta: BTreeMap<String, String>,
    pub rows: Vec<Row>,
}

impl SessionData {
    fn get(&self, k: &str) -> &str {
        self.meta.get(k).map(String::as_str).unwrap_or("")
    }

    fn num(&self, k: &str) -> u64 {
        self.get(k).parse().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub path: PathBuf,
    pub sessions: Vec<SessionData>,
    /// Argument lists of invocations passed straight to the real solver.
    pub execs: Vec<String>,
}

pub fn load(dir: &Path) -> io::Result<Recording> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("meta") {
            continue;
        }
        let mut s = SessionData::default();
        for line in fs::read_to_string(&path)?.lines() {
            if let Some((k, v)) = line.split_once('=') {
                s.meta.insert(k.to_string(), v.to_string());
            }
        }
        if let Ok(text) = fs::read_to_string(path.with_extension("tsv")) {
            s.rows = parse_rows(&text);
        }
        sessions.push(s);
    }
    sessions.sort_by_key(|s| (s.num("start_unix_ms"), s.num("pid")));
    let execs = match fs::read_to_string(dir.join("exec.log")) {
        Ok(text) => text
            .lines()
            .filter_map(|l| l.splitn(3, '\t').nth(2).map(str::to_string))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    Ok(Recording {
        path: dir.to_path_buf(),
        sessions,
        execs,
    })
}

fn parse_rows(text: &str) -> Vec<Row> {
    let mut lines = text.lines();
    let fields = match lines.next() {
        Some(RECORDS_HEADER) => 9,
        Some(RECORDS_HEADER_V1) => 8,
        _ => return Vec::new(),
    };
    lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() != fields {
                return None;
            }
            Some(Row {
                kind: f[1].to_string(),
                key: f[3].to_string(),
                context: f[4].to_string(),
                query: f[5].to_string(),
                verdict: f[6].to_string(),
                us: f[7].parse().ok()?,
            })
        })
        .collect()
}

/// Count and solver time of a set of queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub n: u64,
    pub us: u64,
}

impl Tally {
    fn add(&mut self, us: u64) {
        self.n += 1;
        self.us += us;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Simulation {
    pub hits: Tally,
    /// Same key seen before, but its verdict was not `unsat`, so not stored.
    pub not_stored: Tally,
    /// Same query (options, top frame, command) answered `unsat` before
    /// under a different context.
    pub context_changed: Tally,
    /// Query not seen before.
    pub query_changed: Tally,
    /// Recorded without a key (the solver could not be hashed).
    pub no_key: Tally,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub path: PathBuf,
    pub sessions: u64,
    pub incomplete: u64,
    pub errored: u64,
    pub bypassed: BTreeMap<String, u64>,
    pub argv: BTreeMap<String, u64>,
    pub session_us: u64,
    pub span_ms: u64,
    pub queries: Tally,
    pub verdicts: BTreeMap<String, u64>,
    pub reads: u64,
    /// `None` for the first recording, which has nothing to hit.
    pub sim: Option<Simulation>,
}

#[derive(Default)]
struct Cache {
    seen_keys: HashSet<String>,
    unsat_keys: HashSet<String>,
    unsat_queries: HashSet<String>,
}

impl Cache {
    fn add(&mut self, r: &Recording) {
        for row in r.sessions.iter().flat_map(|s| &s.rows) {
            if row.kind != "query" || row.key == "-" {
                continue;
            }
            self.seen_keys.insert(row.key.clone());
            if row.verdict == "unsat" {
                self.unsat_keys.insert(row.key.clone());
                self.unsat_queries.insert(row.query.clone());
            }
        }
    }

    fn simulate(&self, r: &Recording) -> Simulation {
        let mut sim = Simulation::default();
        for row in r.sessions.iter().flat_map(|s| &s.rows) {
            if row.kind != "query" {
                continue;
            }
            let bucket = if row.key == "-" {
                &mut sim.no_key
            } else if self.unsat_keys.contains(&row.key) {
                &mut sim.hits
            } else if self.seen_keys.contains(&row.key) {
                &mut sim.not_stored
            } else if self.unsat_queries.contains(&row.query) {
                &mut sim.context_changed
            } else {
                &mut sim.query_changed
            };
            bucket.add(row.us);
        }
        sim
    }
}

pub fn summarize(recordings: &[Recording]) -> Vec<Summary> {
    let mut cache = Cache::default();
    let mut out = Vec::new();
    for (i, r) in recordings.iter().enumerate() {
        let mut s = Summary {
            path: r.path.clone(),
            ..Summary::default()
        };
        let mut first_start = u64::MAX;
        let mut last_end = 0u64;
        for sess in &r.sessions {
            s.sessions += 1;
            if sess.get("end_unix_ms").is_empty() {
                s.incomplete += 1;
            }
            if sess.get("errored") == "1" {
                s.errored += 1;
            }
            if !sess.get("bypass").is_empty() {
                *s.bypassed.entry(sess.get("bypass").to_string()).or_default() += 1;
            }
            *s.argv.entry(sess.get("argv").to_string()).or_default() += 1;
            s.session_us += sess.num("wall_us");
            first_start = first_start.min(sess.num("start_unix_ms"));
            last_end = last_end.max(sess.num("end_unix_ms"));
            for row in &sess.rows {
                match row.kind.as_str() {
                    "query" => {
                        s.queries.add(row.us);
                        *s.verdicts.entry(row.verdict.clone()).or_default() += 1;
                    }
                    "read" => s.reads += 1,
                    _ => {}
                }
            }
        }
        for e in &r.execs {
            *s.argv.entry(format!("{e} (exec)")).or_default() += 1;
        }
        s.span_ms = last_end.saturating_sub(first_start);
        if i > 0 {
            s.sim = Some(cache.simulate(r));
        }
        cache.add(r);
        out.push(s);
    }
    out
}

fn secs(us: u64) -> String {
    format!("{:.2} s", us as f64 / 1e6)
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "-".into()
    } else {
        format!("{:.1}%", 100.0 * part as f64 / whole as f64)
    }
}

pub fn render(summaries: &[Summary]) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "z3cache analyze (key format {})", crate::FORMAT_VERSION);
    for (i, s) in summaries.iter().enumerate() {
        let _ = writeln!(o, "\nrecording {}: {}", i + 1, s.path.display());
        let _ = writeln!(
            o,
            "  sessions          {} ({} errored, {} bypassed, {} incomplete)",
            s.sessions,
            s.errored,
            s.bypassed.values().sum::<u64>(),
            s.incomplete
        );
        let _ = writeln!(
            o,
            "  solver time       {} summed over sessions, {} first start to last exit",
            secs(s.session_us),
            secs(s.span_ms * 1000)
        );
        let verdicts: Vec<String> = s.verdicts.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(
            o,
            "  queries           {} taking {} ({}); {} reads",
            s.queries.n,
            secs(s.queries.us),
            if verdicts.is_empty() { "none".into() } else { verdicts.join(", ") },
            s.reads
        );
        for (reason, n) in &s.bypassed {
            let _ = writeln!(o, "  bypass            {n} x {reason}");
        }
        for (argv, n) in &s.argv {
            let _ = writeln!(o, "  arguments         {n} x {argv}");
        }
        let Some(sim) = &s.sim else {
            let _ = writeln!(o, "  projection        none (first recording; nothing to hit)");
            continue;
        };
        let total = s.queries;
        let line = |o: &mut String, label: &str, t: Tally| {
            let _ = writeln!(
                o,
                "    {label:<30} {:>7} {:>7}  {:>10} {:>7}",
                t.n,
                pct(t.n, total.n),
                secs(t.us),
                pct(t.us, total.us)
            );
        };
        let _ = writeln!(o, "  projection (nothing was cached): a cache filled by recordings 1-{}, storing unsat only, would give:", i);
        let _ = writeln!(o, "    {:<30} {:>7} {:>7}  {:>10} {:>7}", "", "queries", "", "time", "");
        line(&mut o, "would hit", sim.hits);
        line(&mut o, "would miss: earlier not unsat", sim.not_stored);
        line(&mut o, "would miss: new context only", sim.context_changed);
        line(&mut o, "would miss: new query", sim.query_changed);
        if sim.no_key.n > 0 {
            line(&mut o, "not keyed", sim.no_key);
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, query: &str, verdict: &str, us: u64) -> Row {
        Row {
            kind: "query".into(),
            key: key.into(),
            context: String::new(),
            query: query.into(),
            verdict: verdict.into(),
            us,
        }
    }

    fn recording(rows: Vec<Row>) -> Recording {
        Recording {
            sessions: vec![SessionData {
                meta: BTreeMap::new(),
                rows,
            }],
            ..Recording::default()
        }
    }

    #[test]
    fn simulation_classifies_misses() {
        let first = recording(vec![
            row("k1", "q1", "unsat", 10),
            row("k2", "q2", "unknown", 20),
            row("k3", "q3", "unsat", 30),
        ]);
        let second = recording(vec![
            row("k1", "q1", "unsat", 11),   // hit
            row("k2", "q2", "unsat", 21),   // seen, but stored only if unsat
            row("k3b", "q3", "unsat", 31),  // same query, other context
            row("k4", "q4", "unsat", 41),   // new
        ]);
        let third = recording(vec![row("k4", "q4", "unsat", 5), row("k2", "q2", "sat", 5)]);
        let s = summarize(&[first, second, third]);
        assert!(s[0].sim.is_none());
        let sim = s[1].sim.as_ref().unwrap();
        assert_eq!(sim.hits, Tally { n: 1, us: 11 });
        assert_eq!(sim.not_stored, Tally { n: 1, us: 21 });
        assert_eq!(sim.context_changed, Tally { n: 1, us: 31 });
        assert_eq!(sim.query_changed, Tally { n: 1, us: 41 });
        // The third run sees what the second stored.
        let sim = s[2].sim.as_ref().unwrap();
        assert_eq!(sim.hits.n, 2);
        assert_eq!(s[1].queries, Tally { n: 4, us: 104 });
    }

    #[test]
    fn rows_parse() {
        let text = format!("{RECORDS_HEADER}\n1\tquery\t1\tk\tc\tq\tunsat\t42\tcache\nbad line\n");
        let rows = parse_rows(&text);
        assert_eq!(rows.len(), 1);
        let v1 = format!("{RECORDS_HEADER_V1}\n1\tquery\t1\tk\tc\tq\tunsat\t42\n");
        assert_eq!(parse_rows(&v1).len(), 1);
        assert_eq!(rows[0].us, 42);
        assert!(parse_rows("# other header\n1\tquery\t1\tk\tc\tq\tunsat\t42\n").is_empty());
    }
}
