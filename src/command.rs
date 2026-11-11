//! Command classification (DESIGN section 5).
//!
//! The tables are explicit. A command that is in none of them is classified
//! as [`Class::Bypass`]; add commands deliberately, with a test.

use crate::profile::SolverProfile;
use crate::sexp::{Command, Item};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Class {
    /// Appended to the top frame; produces no output.
    Silent,
    Push(u64),
    Pop(u64),
    /// `set-option` of an allowlisted option: the name as written, without
    /// the leading `:`, and the value in canonical form.
    SetOption { name: Vec<u8>, value: Vec<u8> },
    /// `echo` and `get-info :version`: answerable without a solver.
    Local,
    /// `get-info :all-statistics`.
    Statistics,
    /// `check-sat` and `check-sat-assuming`.
    Query,
    /// Commands that read solver state without changing it.
    Read,
    Exit,
    /// Not in the tables, or an option off the allowlist (section 7).
    Bypass(String),
}

pub const SILENT: &[&str] = &[
    "assert",
    "declare-const",
    "declare-datatype",
    "declare-datatypes",
    "declare-fun",
    "declare-sort",
    "define-const",
    "define-fun",
    "define-fun-rec",
    "define-funs-rec",
    "define-sort",
    "set-info",
    "set-logic",
];

pub const READ: &[&str] = &[
    "eval",
    "get-model",
    "get-option",
    "get-unsat-core",
    "get-value",
];

pub fn classify(cmd: &Command, profile: &dyn SolverProfile) -> Class {
    let Some(head) = cmd.head() else {
        return Class::Bypass("command without a name".into());
    };
    let args = cmd.args();
    let name = String::from_utf8_lossy(head);
    let bad_args = || Class::Bypass(format!("unexpected arguments to {name}"));
    match &*name {
        n if SILENT.contains(&n) => Class::Silent,
        n if READ.contains(&n) => Class::Read,
        "push" | "pop" => {
            let count = match args {
                [] => 1,
                [Item::Atom(n)] => match numeral(n) {
                    Some(n) => n,
                    None => return bad_args(),
                },
                _ => return bad_args(),
            };
            if head == b"push" {
                Class::Push(count)
            } else {
                Class::Pop(count)
            }
        }
        "set-option" => match args {
            [Item::Atom(key), value] if key.starts_with(b":") => {
                let name = &key[1..];
                if profile.is_cached_option(name) {
                    Class::SetOption {
                        name: name.to_vec(),
                        value: value.bytes().to_vec(),
                    }
                } else {
                    Class::Bypass(format!("option {}", String::from_utf8_lossy(key)))
                }
            }
            _ => bad_args(),
        },
        "echo" => match args {
            [Item::Atom(s)] if s.starts_with(b"\"") => Class::Local,
            _ => bad_args(),
        },
        "get-info" => match args {
            [Item::Atom(key)] if key.as_slice() == b":version" => Class::Local,
            [Item::Atom(key)] if key.as_slice() == b":all-statistics" => Class::Statistics,
            [Item::Atom(key)] if key.starts_with(b":") => Class::Read,
            _ => bad_args(),
        },
        "check-sat" => match args {
            [] => Class::Query,
            _ => bad_args(),
        },
        "check-sat-assuming" => match args {
            [Item::List(_)] => Class::Query,
            _ => bad_args(),
        },
        "exit" => match args {
            [] => Class::Exit,
            _ => bad_args(),
        },
        _ => Class::Bypass(format!("command {name}")),
    }
}

fn numeral(b: &[u8]) -> Option<u64> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Z3;
    use crate::sexp::Splitter;

    fn class(src: &str) -> Class {
        let mut s = Splitter::new();
        s.feed(src.as_bytes());
        let cmd = s.next_command().unwrap().unwrap();
        classify(&cmd, &Z3)
    }

    fn is_bypass(c: Class) -> bool {
        matches!(c, Class::Bypass(_))
    }

    #[test]
    fn silent_table() {
        for name in SILENT {
            assert_eq!(class(&format!("({name} x)")), Class::Silent, "{name}");
        }
        assert!(is_bypass(class("(declare-something x)")));
    }

    #[test]
    fn read_table() {
        for name in READ {
            assert_eq!(class(&format!("({name})")), Class::Read, "{name}");
        }
        assert_eq!(class("(get-info :reason-unknown)"), Class::Read);
    }

    #[test]
    fn scope() {
        assert_eq!(class("(push)"), Class::Push(1));
        assert_eq!(class("(push 3)"), Class::Push(3));
        assert_eq!(class("(pop)"), Class::Pop(1));
        assert_eq!(class("(pop 2)"), Class::Pop(2));
        assert!(is_bypass(class("(pop x)")));
        assert!(is_bypass(class("(pop 1 2)")));
        assert!(is_bypass(class("(push -1)")));
    }

    #[test]
    fn options() {
        assert_eq!(
            class("(set-option :rlimit 100)"),
            Class::SetOption {
                name: b"rlimit".to_vec(),
                value: b"100".to_vec()
            }
        );
        // Not on the allowlist, including other spellings of allowed ones.
        assert_eq!(class("(set-option :RLimit 100)"), Class::Bypass("option :RLimit".into()));
        assert_eq!(
            class("(set-option :global-decls true)"),
            Class::Bypass("option :global-decls".into())
        );
        assert!(is_bypass(class("(set-option :trace true)")));
        assert!(is_bypass(class("(set-option rlimit 1)")));
        assert_eq!(
            class("(set-option :smtlib2_compliant true)"),
            Class::Bypass("option :smtlib2_compliant".into())
        );
    }

    #[test]
    fn local_statistics_query_exit() {
        assert_eq!(class("(echo \"<<DONE>>\")"), Class::Local);
        assert!(is_bypass(class("(echo x)")));
        assert_eq!(class("(get-info :version)"), Class::Local);
        assert_eq!(class("(get-info :all-statistics)"), Class::Statistics);
        assert_eq!(class("(check-sat)"), Class::Query);
        assert_eq!(class("(check-sat-assuming (a b))"), Class::Query);
        assert!(is_bypass(class("(check-sat x)")));
        assert_eq!(class("(exit)"), Class::Exit);
    }

    #[test]
    fn unknown_commands_bypass() {
        for src in ["(reset)", "(get-proof)", "(simplify x)", "(apply tactic)", "()", "((a))"] {
            assert!(is_bypass(class(src)), "{src}");
        }
    }
}
