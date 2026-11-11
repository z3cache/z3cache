//! Argument dispatch (DESIGN section 4): admin CLI, exec, or proxy.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, exit};

use z3cache::config::{Config, Mode};
use z3cache::locate::find_real_solver;
use z3cache::profile::{SolverProfile, Z3};
use z3cache::{cli, log, record, session, setup, solver};

fn main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let invoked_as = argv
        .first()
        .and_then(|a| Path::new(a).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let args = argv.get(1..).unwrap_or(&[]);

    if invoked_as == "z3cache" && args.first().is_some_and(cli::is_subcommand) {
        exit(cli::run(args));
    }
    // Typed at a prompt with no arguments, z3 would wait for input on the
    // terminal; point at the setup instead.
    if invoked_as == "z3cache" && args.is_empty() && setup::stdin_is_terminal() {
        print!("{}", cli::GREETING);
        exit(0);
    }

    let cfg = Config::load();
    log::init(cfg.log.as_deref());
    for w in &cfg.warnings {
        log!("{w}");
    }
    let profile: &dyn SolverProfile = &Z3;
    let real = match find_real_solver(&cfg, profile) {
        Ok(r) => {
            log!("real solver {} ({})", r.path.display(), r.how(&cfg));
            r.path
        }
        Err(e) => {
            eprintln!("z3cache: {e}");
            log!("{e}");
            exit(127);
        }
    };

    if cfg.mode == Mode::Off || !profile.is_proxy_args(args) {
        if let Some(dir) = &cfg.record {
            record::record_exec(dir, args);
        }
        let err = Command::new(&real).args(args).exec();
        eprintln!("z3cache: cannot run {}: {err}", real.display());
        log!("cannot run {}: {err}", real.display());
        exit(127);
    }

    solver::install_signal_handlers();
    exit(session::run(&cfg, profile, &real, args));
}
