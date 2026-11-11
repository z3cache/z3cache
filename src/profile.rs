//! Solver-specific behaviour (DESIGN section 17). Only Z3 is supported.

use std::ffi::OsString;

pub trait SolverProfile: Sync {
    /// Name of the real solver binary, looked up on `PATH`.
    fn binary_name(&self) -> &'static str;

    /// True if these arguments (excluding argv[0]) select proxy mode.
    fn is_proxy_args(&self, args: &[OsString]) -> bool;

    /// True if an option, named exactly as written and without the leading
    /// `:`, is on the allowlist. Any other option bypasses (rule 3).
    fn is_cached_option(&self, name: &[u8]) -> bool;

    /// True if an option sets a wall-clock limit, under which `sat` and
    /// `unknown` depend on machine load and are never stored (section 7).
    fn is_wall_clock_option(&self, name: &[u8]) -> bool;

    /// The command that opens one frame.
    fn push_command(&self) -> &'static [u8];

    /// The command that discards one frame.
    fn pop_command(&self) -> &'static [u8];

    /// Command that makes the solver print `sentinel` on a line of its own.
    fn sentinel_command(&self, sentinel: &str) -> Vec<u8>;

    /// The lines (including the newline) the solver may print for
    /// [`SolverProfile::sentinel_command`]. The first is the expected one;
    /// the others are forms a changed output setting could produce, accepted
    /// so that such a change cannot leave the proxy waiting forever.
    fn sentinel_lines(&self, sentinel: &str) -> Vec<Vec<u8>>;

    /// True if a response line reports an error.
    fn is_error_line(&self, line: &[u8]) -> bool;

    /// What the solver prints for `(echo <tok>)`, or `None` if `tok` is not
    /// a string literal.
    fn echo_response(&self, tok: &[u8]) -> Option<Vec<u8>>;

    /// The command whose response carries the resource counter.
    fn statistics_command(&self) -> &'static [u8];

    /// The resource counter in a statistics response.
    fn parse_rlimit(&self, lines: &[Vec<u8>]) -> Option<u64>;

    /// Changes the resource counter in the statistics response among
    /// `lines` to `f` of its value. The response may follow other output,
    /// such as errors for earlier commands, which is left alone.
    fn edit_rlimit(&self, lines: &mut [Vec<u8>], f: &dyn Fn(u64) -> u64);

    /// The statistics response given when no solver is running.
    fn synthesize_statistics(&self, value: u64) -> Vec<u8>;
}

pub struct Z3;

/// Options the cache accepts, by exact spelling (DESIGN section 5). Each is
/// plain solver configuration: its value is in the key, it does not change
/// what `pop` undoes, and it writes nothing to stdout or files. The list is
/// every option Verus 0.2026.10.26 sends in full runs of a large project. Aliases and
/// other spellings are deliberately absent: they bypass.
pub const Z3_CACHED_OPTIONS: &[&str] = &[
    "auto_config",
    "pi.enabled",
    "rewriter.sort_disjunctions",
    "rlimit",
    "smt.arith.nl",
    "smt.arith.solver",
    "smt.case_split",
    "smt.delay_units",
    "smt.mbqi",
    "smt.qi.eager_threshold",
];

/// Z3 options that bound solving by wall-clock time. None is on the
/// allowlist today; `rlimit` counts resources and is deterministic. If one
/// is ever allowlisted, the `all` store policy refuses `sat` and `unknown`
/// while it is set, without further changes.
pub const Z3_WALL_CLOCK_OPTIONS: &[&str] = &["timeout", "smt.timeout", "sat.timeout"];

impl SolverProfile for Z3 {
    fn binary_name(&self) -> &'static str {
        "z3"
    }

    fn is_proxy_args(&self, args: &[OsString]) -> bool {
        let mut smt2 = false;
        let mut stdin = false;
        for a in args {
            let a = a.as_encoded_bytes();
            match a {
                b"-smt2" if !smt2 => smt2 = true,
                b"-in" if !stdin => stdin = true,
                _ => {
                    // A parameter is cached only if it is on the allowlist;
                    // anything else runs the real solver directly.
                    let Some(eq) = a.iter().position(|&c| c == b'=') else {
                        return false;
                    };
                    if !self.is_cached_option(&a[..eq]) {
                        return false;
                    }
                }
            }
        }
        smt2 && stdin
    }

    fn is_cached_option(&self, name: &[u8]) -> bool {
        Z3_CACHED_OPTIONS.iter().any(|o| o.as_bytes() == name)
    }

    fn is_wall_clock_option(&self, name: &[u8]) -> bool {
        Z3_WALL_CLOCK_OPTIONS.iter().any(|o| o.as_bytes() == name)
    }

    fn push_command(&self) -> &'static [u8] {
        b"(push)"
    }

    fn pop_command(&self) -> &'static [u8] {
        b"(pop)"
    }

    fn sentinel_command(&self, sentinel: &str) -> Vec<u8> {
        format!("(echo \"{sentinel}\")").into_bytes()
    }

    fn sentinel_lines(&self, sentinel: &str) -> Vec<Vec<u8>> {
        // Z3 quotes echo output under `smtlib2_compliant`.
        vec![
            format!("{sentinel}\n").into_bytes(),
            format!("\"{sentinel}\"\n").into_bytes(),
        ]
    }

    fn is_error_line(&self, line: &[u8]) -> bool {
        line.starts_with(b"(error")
    }

    fn echo_response(&self, tok: &[u8]) -> Option<Vec<u8>> {
        // Assumption A2: unquoted, `""` unescaped, on a line of its own.
        let mut s = crate::sexp::string_contents(tok)?;
        s.push(b'\n');
        Some(s)
    }

    fn statistics_command(&self) -> &'static [u8] {
        b"(get-info :all-statistics)"
    }

    fn parse_rlimit(&self, lines: &[Vec<u8>]) -> Option<u64> {
        crate::rlimit::parse(lines)
    }

    fn edit_rlimit(&self, lines: &mut [Vec<u8>], f: &dyn Fn(u64) -> u64) {
        crate::rlimit::edit(lines, f)
    }

    fn synthesize_statistics(&self, value: u64) -> Vec<u8> {
        crate::rlimit::synthesize(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn proxy_args() {
        assert!(Z3.is_proxy_args(&args(&["-smt2", "-in"])));
        assert!(Z3.is_proxy_args(&args(&["-in", "-smt2"])));
        assert!(Z3.is_proxy_args(&args(&["-smt2", "smt.mbqi=false", "-in"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "smt.random_seed=1"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "global_decls=true"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2"])));
        assert!(!Z3.is_proxy_args(&args(&[])));
        assert!(!Z3.is_proxy_args(&args(&["--version"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "-st"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "-t:100"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "-in"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "file.smt2"])));
        assert!(!Z3.is_proxy_args(&args(&["-smt2", "-in", "=x"])));
    }

    #[test]
    fn wall_clock_options() {
        assert!(Z3.is_wall_clock_option(b"timeout"));
        assert!(!Z3.is_wall_clock_option(b"rlimit"));
        // Today no allowlisted option is a wall-clock limit. Allowlisting one
        // is allowed (the store policy checks this list), but must be a
        // deliberate change to this test.
        let allowlisted: Vec<&str> = Z3_CACHED_OPTIONS
            .iter()
            .copied()
            .filter(|o| Z3.is_wall_clock_option(o.as_bytes()))
            .collect();
        assert_eq!(allowlisted, Vec::<&str>::new());
    }

    #[test]
    fn option_allowlist() {
        for o in Z3_CACHED_OPTIONS {
            assert!(Z3.is_cached_option(o.as_bytes()), "{o}");
        }
        // Scope-changing, output-changing and unknown options bypass,
        // whatever the spelling.
        for o in [
            "global-decls",
            "global_decls",
            "RLIMIT",
            "smt.random_seed",
            "trace",
            "produce-proofs",
            "print-success",
            "smtlib2_compliant",
            "smtlib2-compliant",
            "nonexistent_option",
        ] {
            assert!(!Z3.is_cached_option(o.as_bytes()), "{o}");
        }
    }
}
