//! Tier 5: Verus on a small crate, directly and through the proxy, cold and
//! warm. The verification results must be identical and the warm run must
//! start no solver.
//!
//! Skipped, with a message, unless `Z3CACHE_VERUS` points at a `verus`
//! binary. The Z3 next to it (as in a Verus release) is the real solver.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

const CRATE: &str = r#"
use vstd::prelude::*;
verus! {
spec fn sq(x: int) -> int { x * x }
proof fn sq_nonneg(x: int) ensures sq(x) >= 0 { assert(x * x >= 0) by (nonlinear_arith); }
fn add_small(a: u32, b: u32) -> (r: u32) requires a < 100, b < 100 ensures r == a + b { a + b }
fn max(a: u64, b: u64) -> (r: u64) ensures r >= a, r >= b, r == a || r == b { if a > b { a } else { b } }
fn sum_to(n: u32) -> (r: u64) requires n < 1000 ensures r <= 1000 * 1000 {
    let mut i: u32 = 0; let mut s: u64 = 0;
    while i < n invariant i <= n, n < 1000, s <= i * 1000 decreases n - i { s = s + i as u64; i = i + 1; }
    s
}
fn main() {}
}
"#;

/// Runs Verus on the crate; returns its "verification results" line.
fn verus(verus: &Path, dir: &Path, cache: Option<&Path>) -> String {
    let mut cmd = Command::new(verus);
    cmd.arg("smoke.rs").current_dir(dir);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("Z3CACHE_") {
            cmd.env_remove(k);
        }
    }
    let z3 = verus.with_file_name("z3");
    if let Some(cache) = cache {
        cmd.env("VERUS_Z3_PATH", z3cache())
            .env("Z3CACHE_REAL_Z3", &z3)
            .env("Z3CACHE_DIR", cache);
    } else {
        cmd.env("VERUS_Z3_PATH", &z3);
    }
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "verus failed:\n{text}");
    text.lines()
        .find(|l| l.contains("verification results"))
        .unwrap_or_else(|| panic!("no results line in:\n{text}"))
        .to_string()
}

fn solver_starts(cache: &Path) -> i64 {
    let out = run(&z3cache(), &["stats", "--json"], b"", &[("Z3CACHE_DIR", cache.as_os_str())]);
    let json = out.text();
    let all = &json[json.find("\"all_time\"").unwrap()..];
    let at = all.find("\"solver_starts\": ").unwrap() + "\"solver_starts\": ".len();
    all[at..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
}

#[test]
fn verus_results_are_identical_and_warm_starts_no_solver() {
    let Some(verus_bin) = std::env::var_os("Z3CACHE_VERUS").filter(|v| !v.is_empty()).map(PathBuf::from)
    else {
        eprintln!("SKIPPED: set Z3CACHE_VERUS to a verus binary to run the end-to-end test");
        return;
    };
    let dir = temp_dir("e2e");
    std::fs::write(dir.join("smoke.rs"), CRATE).unwrap();
    let cache = dir.join("cache");

    let direct = verus(&verus_bin, &dir, None);
    assert!(direct.contains("0 errors"), "{direct}");
    let cold = verus(&verus_bin, &dir, Some(&cache));
    assert_eq!(cold, direct);
    let after_cold = solver_starts(&cache);
    assert!(after_cold > 0);
    let warm = verus(&verus_bin, &dir, Some(&cache));
    assert_eq!(warm, direct);
    assert_eq!(solver_starts(&cache), after_cold, "the warm run started a solver");
}
