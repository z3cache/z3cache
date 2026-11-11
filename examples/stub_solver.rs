//! Scripted stand-in for Z3, used by the proxy tests (DESIGN section 18,
//! tier 2). Not a solver: it answers from a script.
//!
//! Environment:
//! - `Z3STUB_ANSWERS`: comma-separated responses to successive `check-sat`
//!   commands (default: always `unsat`). The answer `crash:N` makes the stub
//!   exit with status N without printing anything; `sleep:MS:ANSWER` waits
//!   MS milliseconds, then answers ANSWER; `gate:PATH:ANSWER` waits until
//!   PATH exists, then answers ANSWER.
//! - `Z3STUB_EXIT_ZERO`: if set, exit 0 even after reporting an error.
//! - `Z3STUB_LOG`: file to which the stub appends `launch <args>` when it
//!   starts and the canonical form of every command it receives.
//!
//! Run as a file named `rust_verify`, the stub stands in for Verus's driver
//! instead: it runs its arguments as a command, with the same stdin, stdout
//! and stderr, and exits with that command's status. Tests use it as the
//! parent process of z3cache (DESIGN section 4).
//!
//! Behaviour that matters to the proxy matches Z3: `echo` prints the string
//! unquoted, errors are `(error "...")` lines, processing continues after an
//! error, and the exit status is 1 if any error was reported.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};

use z3cache::sexp::{Item, Splitter, string_contents};

fn main() {
    // Like a real solver, die quietly if the proxy goes away.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let mut argv = std::env::args_os();
    let name = argv.next().unwrap_or_default();
    if std::path::Path::new(&name).file_name() == Some("rust_verify".as_ref()) {
        let program = argv.next().expect("rust_verify: no command");
        let status = std::process::Command::new(program)
            .args(argv)
            .status()
            .expect("rust_verify: cannot run the command");
        std::process::exit(status.code().unwrap_or(1));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log = std::env::var_os("Z3STUB_LOG").map(|p| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
    });
    if let Some(l) = &mut log {
        writeln!(l, "launch {}", args.join(" ")).unwrap();
    }
    if args.iter().any(|a| a == "--version") {
        println!("Z3 version 4.99.0 - 64 bit (stub)");
        return;
    }
    if !(args.iter().any(|a| a == "-smt2") && args.iter().any(|a| a == "-in")) {
        println!("stub: unsupported arguments {}", args.join(" "));
        std::process::exit(3);
    }

    let answers: Vec<String> = std::env::var("Z3STUB_ANSWERS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let mut next_answer = 0usize;
    let mut rlimit: u64 = 0;
    let mut depth: u64 = 0;
    let mut last_answer = String::new();
    let mut errors = false;

    let mut stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut splitter = Splitter::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = stdin.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        splitter.feed(&buf[..n]);
        while let Some(cmd) = splitter.next_command() {
            let Ok(cmd) = cmd else {
                writeln!(stdout, "(error \"stub: parse error\")").unwrap();
                std::process::exit(1);
            };
            if let Some(l) = &mut log {
                l.write_all(&cmd.canonical).unwrap();
                l.write_all(b"\n").unwrap();
            }
            let head = String::from_utf8_lossy(cmd.head().unwrap_or(b"")).into_owned();
            let arg0 = cmd
                .args()
                .first()
                .map(|i| i.bytes().to_vec())
                .unwrap_or_default();
            let mut error = |stdout: &mut io::StdoutLock, msg: &str| {
                errors = true;
                writeln!(stdout, "(error \"stub: {msg}\")").unwrap();
            };
            match head.as_str() {
                "echo" => {
                    let s = string_contents(&arg0).unwrap();
                    stdout.write_all(&s).unwrap();
                    stdout.write_all(b"\n").unwrap();
                }
                "check-sat" | "check-sat-assuming" => {
                    let mut answer = answers
                        .get(next_answer)
                        .map(String::as_str)
                        .unwrap_or("unsat");
                    next_answer += 1;
                    if let Some(rest) = answer.strip_prefix("gate:") {
                        // Wait until the file exists: a test holds the
                        // answer back for as long as it needs. Give up if
                        // the proxy that started us is gone (we would be
                        // orphaned, holding the test run's stderr pipe open
                        // forever) or after 60 s, so a broken test fails
                        // instead of hanging the suite.
                        let (path, then) = rest.rsplit_once(':').expect("gate:PATH:ANSWER");
                        let parent = std::os::unix::process::parent_id();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(60);
                        while !std::path::Path::new(path).exists() {
                            if std::os::unix::process::parent_id() != parent
                                || std::time::Instant::now() > deadline
                            {
                                std::process::exit(9);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        answer = then;
                    } else if let Some(rest) = answer.strip_prefix("sleep:") {
                        let (ms, then) = rest.split_once(':').expect("sleep:MS:ANSWER");
                        std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap()));
                        answer = then;
                    }
                    if let Some(code) = answer.strip_prefix("crash:") {
                        std::process::exit(code.parse().unwrap());
                    }
                    rlimit += 100;
                    last_answer = answer.to_string();
                    writeln!(stdout, "{answer}").unwrap();
                }
                "get-info" => match &arg0[..] {
                    b":version" => writeln!(stdout, "(:version \"4.99.0\")").unwrap(),
                    b":all-statistics" => {
                        writeln!(stdout, "(:max-memory   0.19\n :rlimit-count {rlimit})").unwrap()
                    }
                    b":reason-unknown" => writeln!(stdout, "(:reason-unknown \"stub\")").unwrap(),
                    _ => error(&mut stdout, "unsupported info"),
                },
                // Like Z3: a model only after sat.
                "get-model" if last_answer == "unsat" => {
                    error(&mut stdout, "model is not available")
                }
                "get-model" => writeln!(stdout, "(\n  (define-fun x () Int\n    0)\n)").unwrap(),
                "get-value" => writeln!(stdout, "((x 0))").unwrap(),
                "push" => depth += 1,
                "pop" => {
                    let n = match cmd.args().first() {
                        Some(Item::Atom(a)) => String::from_utf8_lossy(a).parse().unwrap_or(1),
                        _ => 1,
                    };
                    if n > depth {
                        error(&mut stdout, "pop below the base frame");
                    } else {
                        depth -= n;
                    }
                }
                "assert"
                    if cmd
                        .canonical
                        .windows(20)
                        .any(|w| w == b"stub-multiline-error") =>
                {
                    // Like Z3's unknown-parameter error: one response over
                    // several lines.
                    error(&mut stdout, "multiline\n  detail one\n  detail two")
                }
                "assert" if cmd.canonical.windows(10).any(|w| w == b"stub-error") => {
                    error(&mut stdout, "asserted stub-error")
                }
                "assert" | "declare-const" | "declare-fun" | "declare-sort" | "define-fun"
                | "set-option" | "set-info" | "set-logic" | "reset" => {}
                "exit" => {
                    stdout.flush().unwrap();
                    std::process::exit(i32::from(
                        errors && std::env::var_os("Z3STUB_EXIT_ZERO").is_none(),
                    ));
                }
                _ => error(&mut stdout, &format!("unsupported command {head}")),
            }
        }
        stdout.flush().unwrap();
    }
    std::process::exit(i32::from(
        errors && std::env::var_os("Z3STUB_EXIT_ZERO").is_none(),
    ));
}
