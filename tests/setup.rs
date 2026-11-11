//! Tier 2: `z3cache setup` against a temporary home directory, with a fake
//! `verus` on `PATH` whose `z3` is the stub solver. No real Z3 needed.
//!
//! Every run sets `HOME` and `XDG_CONFIG_HOME` to a temporary directory, so
//! that no test can touch the user's own startup files.

mod common;

use std::ffi::OsStr;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::*;
use z3cache::setup::{BEGIN, END, assignment, backup_path, echo_command};

/// A temporary home directory, and a Verus release whose `verus` is on
/// `PATH` through a symlink.
struct World {
    home: PathBuf,
    release: PathBuf,
    /// `PATH` for setup: the directory with the `verus` symlink.
    bin: PathBuf,
    /// A copy (or hard link) of z3cache in `~/.cargo/bin`, as
    /// `cargo install` puts it.
    installed: PathBuf,
}

const VERUS_SCRIPT: &str = "#!/bin/sh\necho Verus\necho '  Version: 0.2026.11.01'\n";

fn world(name: &str) -> World {
    let dir = std::fs::canonicalize(temp_dir(name)).unwrap();
    let home = dir.join("home");
    let release = dir.join("release");
    let bin = dir.join("bin");
    for d in [&home, &release, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(release.join("verus"), VERUS_SCRIPT).unwrap();
    std::fs::set_permissions(release.join("verus"), PermissionsExt::from_mode(0o755)).unwrap();
    symlink(stub(), release.join("z3")).unwrap();
    symlink(release.join("verus"), bin.join("verus")).unwrap();
    let installed = home.join(".cargo/bin/z3cache");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    if std::fs::hard_link(z3cache(), &installed).is_err() {
        std::fs::copy(z3cache(), &installed).unwrap();
    }
    World {
        home,
        release,
        bin,
        installed,
    }
}

/// What a setup run printed.
struct Out {
    text: String,
    code: i32,
}

impl World {
    /// Runs `bin args` with `$SHELL` = `shell`, `answers` on stdin, as if on
    /// a terminal if `tty`, and `extra` in the environment.
    fn run_with(
        &self,
        bin: &Path,
        shell: &str,
        args: &[&str],
        answers: &str,
        tty: bool,
        extra: &[(&str, &OsStr)],
    ) -> Out {
        assert!(
            self.home
                .starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        );
        let config = self.home.join(".config");
        let mut env = vec![
            ("HOME", self.home.as_os_str()),
            ("XDG_CONFIG_HOME", config.as_os_str()),
            ("SHELL", OsStr::new(shell)),
            ("PATH", self.bin.as_os_str()),
            ("Z3CACHE_REAL_Z3", OsStr::new("")),
        ];
        if tty {
            env.push(("Z3CACHE_ASSUME_TTY", OsStr::new("1")));
        }
        env.extend_from_slice(extra);
        let mut cmd = proxy_command(bin, args, &env);
        cmd.env_remove("ZDOTDIR");
        let mut child = cmd.spawn().unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(answers.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        Out {
            text: String::from_utf8(out.stdout).unwrap(),
            code: out.status.code().unwrap_or(-1),
        }
    }

    /// Runs the installed z3cache's setup.
    fn run(&self, shell: &str, args: &[&str], answers: &str, tty: bool) -> Out {
        self.setup_with(shell, args, answers, tty, &[])
    }

    fn setup_with(
        &self,
        shell: &str,
        args: &[&str],
        answers: &str,
        tty: bool,
        extra: &[(&str, &OsStr)],
    ) -> Out {
        let mut all = vec!["setup"];
        all.extend_from_slice(args);
        self.run_with(&self.installed, shell, &all, answers, tty, extra)
    }

    fn file(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.file(rel)).ok()
    }

    /// Every file under the home directory, apart from the installed binary.
    fn files(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push(p.strip_prefix(base).unwrap().display().to_string());
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.home, &self.home, &mut out);
        out.retain(|f| f != ".cargo/bin/z3cache");
        out.sort();
        out
    }
}

const ZSH_LINE: &str = "export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\"";
const FISH_LINE: &str = "set -gx VERUS_Z3_PATH \"$HOME/.cargo/bin/z3cache\"";

fn block(line: &str) -> String {
    format!("{BEGIN}\n{line}\n{END}\n")
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn prints_the_command_for_each_shell() {
    let w = world("setup-print");
    for (shell, file, command) in [
        (
            "/bin/zsh",
            "~/.zshenv",
            "echo 'export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\"' >> ~/.zshenv",
        ),
        (
            "/usr/local/bin/bash",
            "~/.bashrc",
            "echo 'export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\"' >> ~/.bashrc",
        ),
        (
            "/opt/homebrew/bin/fish",
            "~/.config/fish/config.fish",
            "echo 'set -gx VERUS_Z3_PATH \"$HOME/.cargo/bin/z3cache\"' >> ~/.config/fish/config.fish",
        ),
    ] {
        let out = w.run(shell, &[], "", false);
        let t = &out.text;
        assert_eq!(out.code, 0, "{t}");
        let name = Path::new(shell).file_name().unwrap().to_str().unwrap();
        assert!(
            t.contains(&format!(
                "verus     {}, version 0.2026.11.01, with its z3\n",
                w.release.display()
            )),
            "{t}"
        );
        assert!(
            t.contains(&format!("z3cache   {}\n", w.installed.display())),
            "{t}"
        );
        assert!(
            t.contains(&format!("shell     {name}, your login shell from $SHELL")),
            "{t}"
        );
        assert!(t.contains("--shell zsh|bash|fish"), "{t}");
        assert!(t.contains(&format!("add a line to {file}")), "{t}");
        assert!(t.contains(&format!("\n    {command}\n")), "{t}");
        assert!(
            t.contains(&format!("run `source {file}` in this one")),
            "{t}"
        );
        assert!(t.contains("z3cache doctor"), "{t}");
        assert!(t.contains("use Verus as usual"), "{t}");
        assert!(t.contains("z3cache stats"), "{t}");
        assert!(!t.contains("Add them now?"), "{t}");
        assert!(!t.contains("check     "), "printing runs no check: {t}");
        if name == "zsh" {
            assert!(t.contains("which every zsh reads"), "{t}");
            assert!(t.contains("~/.zshrc"), "{t}");
        }
        if name == "bash" {
            assert_eq!(
                t.contains("~/.bash_profile"),
                cfg!(target_os = "macos"),
                "{t}"
            );
        }
    }
    // Nothing was written.
    assert_eq!(w.files(), Vec::<String>::new());
}

#[test]
fn an_unknown_shell_gets_the_posix_line() {
    let w = world("setup-unknown");
    let out = w.run("/bin/tcsh", &[], "", true);
    let t = &out.text;
    assert_eq!(out.code, 0, "{t}");
    assert!(
        t.contains("shell     not zsh, bash or fish ($SHELL is /bin/tcsh)"),
        "{t}"
    );
    assert!(
        t.contains("Add this line to your shell's startup file:"),
        "{t}"
    );
    assert!(t.contains(&format!("\n    {ZSH_LINE}\n")), "{t}");
    assert!(!t.contains("[Y/n]"), "{t}");
    // --yes cannot do what it was asked.
    assert_eq!(w.run("/bin/tcsh", &["--yes"], "", false).code, 1);
    assert_eq!(w.files(), Vec::<String>::new());
}

#[test]
fn the_line_names_the_binary_that_ran() {
    // Outside the home directory, the path is absolute.
    let w = world("setup-absolute");
    let exe = std::fs::canonicalize(z3cache()).unwrap();
    let out = w.run_with(&z3cache(), "/bin/zsh", &["setup"], "", false, &[]);
    let line = assignment(None, &exe, Some(&w.home));
    assert!(line.starts_with("export VERUS_Z3_PATH=\"/"), "{line}");
    assert!(
        out.text.contains(&echo_command(&line, "~/.zshenv")),
        "{}",
        out.text
    );
}

#[test]
fn shell_choice_flag_then_parent_then_login_shell() {
    let w = world("setup-shell");
    let out = w.run("/bin/zsh", &["--shell", "fish"], "", false);
    assert!(
        out.text.contains("shell     fish (from --shell)"),
        "{}",
        out.text
    );
    assert!(out.text.contains(FISH_LINE), "{}", out.text);
    assert_eq!(w.run("/bin/zsh", &["--shell", "ksh"], "", false).code, 2);

    // Typed in bash although the login shell is zsh: bash.
    let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.exists())
    else {
        eprintln!("SKIPPED: no bash to run setup from");
        return;
    };
    // `; exit` keeps bash from replacing itself with the last command.
    let script = format!(
        "{} setup; exit $?",
        w.installed.display().to_string().replace(' ', "\\ ")
    );
    let out = w.run_with(bash, "/bin/zsh", &["-c", &script], "", false, &[]);
    assert!(
        out.text
            .contains("shell     bash, detected from the shell you ran this in"),
        "{}",
        out.text
    );
    assert!(out.text.contains(">> ~/.bashrc"), "{}", out.text);
}

#[test]
fn yes_adds_the_block_once() {
    let w = world("setup-yes");
    let out = w.run("/bin/zsh", &["--yes"], "", false);
    let t = &out.text;
    assert_eq!(out.code, 0, "{t}");
    assert_eq!(w.read(".zshenv").unwrap(), block(ZSH_LINE));
    assert!(t.contains("Added to ~/.zshenv.\n"), "{t}");
    // The self-check found the z3 next to verus and ran the stub.
    assert!(t.contains("check     ok: "), "{t}");
    assert!(t.contains("the verus on PATH"), "{t}");
    assert!(t.contains("run `source ~/.zshenv` in this one"), "{t}");
    // A new file needs no backup.
    assert_eq!(w.files(), [".zshenv"]);

    // Again: nothing changes.
    let out = w.run("/bin/zsh", &[], "", true);
    assert!(
        out.text.contains("Already set up in ~/.zshenv."),
        "{}",
        out.text
    );
    assert!(!out.text.contains("[Y/n]"), "{}", out.text);
    assert!(out.text.contains("check     ok: "), "{}", out.text);
    assert_eq!(w.read(".zshenv").unwrap(), block(ZSH_LINE));
    assert_eq!(w.files(), [".zshenv"]);
}

#[test]
fn appending_keeps_the_file_and_saves_one_backup() {
    for (before, sep) in [("alias a=b\n", ""), ("alias a=b", "\n")] {
        let w = world("setup-append");
        let f = w.file(".bashrc");
        std::fs::write(&f, before).unwrap();
        std::fs::set_permissions(&f, PermissionsExt::from_mode(0o600)).unwrap();
        let out = w.run("/bin/bash", &["--yes"], "", false);
        assert_eq!(out.code, 0, "{}", out.text);
        assert_eq!(
            w.read(".bashrc").unwrap(),
            format!("{before}{sep}{}", block(ZSH_LINE))
        );
        assert_eq!(mode(&f), 0o600);
        let backup = backup_path(&f);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before);
        assert_eq!(mode(&backup), 0o600);
        assert!(
            out.text.contains(
                "Added to ~/.bashrc (the file as it was is in ~/.bashrc.z3cache-backup)."
            ),
            "{}",
            out.text
        );

        // The backup is made once: a later change keeps the first one.
        let other = format!("{BEGIN}\nexport VERUS_Z3_PATH=\"/old/z3cache\"\n{END}\n");
        std::fs::write(&f, format!("x\n{other}y\n")).unwrap();
        let out = w.run("/bin/bash", &["--yes"], "", false);
        assert_eq!(out.code, 0, "{}", out.text);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before);
    }
}

#[test]
fn a_different_block_is_replaced_only_when_agreed() {
    let w = world("setup-replace");
    let f = w.file(".zshenv");
    let old = "export VERUS_Z3_PATH=\"/old/z3cache\"";
    let before = format!("# mine\nexport A=1\n{}export B=2\n", block(old));
    std::fs::write(&f, &before).unwrap();
    std::fs::set_permissions(&f, PermissionsExt::from_mode(0o640)).unwrap();

    // Not on a terminal: nothing changes, and the way to do it is shown.
    let out = w.run("/bin/zsh", &[], "", false);
    assert!(out.text.contains(&format!("\n    {old}\n")), "{}", out.text);
    assert!(
        out.text.contains(&format!("\n    {ZSH_LINE}\n")),
        "{}",
        out.text
    );
    assert!(out.text.contains("z3cache setup --yes"), "{}", out.text);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);

    // Declined.
    let out = w.run("/bin/zsh", &[], "n\n", true);
    assert!(out.text.contains("Replace them? [Y/n] n\n"), "{}", out.text);
    assert!(out.text.contains("Nothing changed."), "{}", out.text);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
    assert!(!backup_path(&f).exists());

    // Agreed: only the block changes.
    let out = w.run("/bin/zsh", &[], "y\n", true);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(out.text.contains("Updated ~/.zshenv"), "{}", out.text);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        format!("# mine\nexport A=1\n{}export B=2\n", block(ZSH_LINE))
    );
    assert_eq!(mode(&f), 0o640);
    assert_eq!(std::fs::read_to_string(backup_path(&f)).unwrap(), before);
}

#[test]
fn the_prompt_adds_on_enter_and_prints_the_command_on_no() {
    let w = world("setup-prompt");
    let out = w.run("/bin/zsh", &[], "n\n", true);
    let t = &out.text;
    assert!(
        t.contains(&format!("\n    {BEGIN}\n    {ZSH_LINE}\n    {END}\n")),
        "{t}"
    );
    assert!(t.contains("Add them now? [Y/n] n\nNothing changed."), "{t}");
    assert!(t.contains(&echo_command(ZSH_LINE, "~/.zshenv")), "{t}");
    assert_eq!(w.files(), Vec::<String>::new());

    // End of input is no as well.
    let out = w.run("/bin/zsh", &[], "", true);
    assert!(out.text.contains("Nothing changed."), "{}", out.text);
    assert_eq!(w.files(), Vec::<String>::new());

    // Enter takes the default, yes.
    let out = w.run("/bin/zsh", &[], "\n", true);
    assert!(out.text.contains("Added to ~/.zshenv."), "{}", out.text);
    assert_eq!(w.read(".zshenv").unwrap(), block(ZSH_LINE));
}

#[test]
fn fish_gets_its_own_syntax_and_directory() {
    let w = world("setup-fish");
    let out = w.run("/usr/bin/fish", &["--yes"], "", false);
    assert_eq!(out.code, 0, "{}", out.text);
    assert_eq!(
        w.read(".config/fish/config.fish").unwrap(),
        block(FISH_LINE)
    );
    let out = w.run("/usr/bin/fish", &["--undo", "--yes"], "", false);
    assert!(out.text.contains("set -e VERUS_Z3_PATH"), "{}", out.text);
    assert_eq!(w.read(".config/fish/config.fish").unwrap(), "");
}

#[test]
fn undo_removes_only_the_block() {
    let w = world("setup-undo");
    let f = w.file(".zshenv");
    let before = format!("export A=1\n{}export B=2\n", block(ZSH_LINE));
    std::fs::write(&f, &before).unwrap();
    std::fs::set_permissions(&f, PermissionsExt::from_mode(0o600)).unwrap();

    // Not on a terminal, without --yes: shown, not removed.
    let out = w.run("/bin/zsh", &["--undo"], "", false);
    assert!(
        out.text.contains("z3cache setup --undo --yes"),
        "{}",
        out.text
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
    // --print never changes anything either.
    w.run("/bin/zsh", &["--undo", "--print"], "", true);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);

    let out = w.run("/bin/zsh", &["--undo"], "y\n", true);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        out.text.contains(&format!("\n    {ZSH_LINE}\n")),
        "{}",
        out.text
    );
    assert!(out.text.contains("Removed from ~/.zshenv."), "{}", out.text);
    assert!(out.text.contains("unset VERUS_Z3_PATH"), "{}", out.text);
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "export A=1\nexport B=2\n"
    );
    assert_eq!(mode(&f), 0o600);
    assert_eq!(std::fs::read_to_string(backup_path(&f)).unwrap(), before);

    // Nothing left: not an error.
    let out = w.run("/bin/zsh", &["--undo", "--yes"], "", false);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        out.text.contains("No z3cache lines in ~/.zshenv"),
        "{}",
        out.text
    );
    let out = w.run("/bin/bash", &["--undo"], "", false);
    assert_eq!(out.code, 0, "{}", out.text);
    assert!(
        out.text.contains("~/.bashrc does not exist"),
        "{}",
        out.text
    );
}

#[test]
fn a_symlinked_startup_file_stays_a_symlink() {
    let w = world("setup-symlink");
    let dotfiles = w.home.join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("zshenv");
    std::fs::write(&real, "export A=1\n").unwrap();
    symlink(&real, w.file(".zshenv")).unwrap();
    assert_eq!(w.run("/bin/zsh", &["--yes"], "", false).code, 0);
    assert_eq!(w.run("/bin/zsh", &["--undo", "--yes"], "", false).code, 0);
    assert!(
        std::fs::symlink_metadata(w.file(".zshenv"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "export A=1\n");
}

#[test]
fn broken_markers_are_left_alone() {
    let w = world("setup-broken");
    let before = format!("export A=1\n{BEGIN}\nexport VERUS_Z3_PATH=/x\n");
    std::fs::write(w.file(".zshenv"), &before).unwrap();
    for args in [&["--yes"][..], &["--undo", "--yes"]] {
        let out = w.run("/bin/zsh", args, "", false);
        assert_eq!(out.code, 1, "{}", out.text);
        assert!(
            out.text.contains("incomplete z3cache block (line 2)"),
            "{}",
            out.text
        );
        assert_eq!(w.read(".zshenv").unwrap(), before);
    }
    assert!(!backup_path(&w.file(".zshenv")).exists());
}

#[test]
fn an_existing_setting_counts_as_set_up() {
    let w = world("setup-existing");
    // The line added by hand, as the echo command adds it.
    std::fs::write(w.file(".zshenv"), format!("export A=1\n{ZSH_LINE}\n")).unwrap();
    let out = w.run("/bin/zsh", &[], "", true);
    assert!(
        out.text.contains("Already set up in ~/.zshenv (line 2)."),
        "{}",
        out.text
    );
    assert_eq!(w.files(), [".zshenv"]);

    // VERUS_Z3_PATH already points here, set some other way.
    let w = world("setup-env");
    let out = w.setup_with(
        "/bin/zsh",
        &[],
        "",
        true,
        &[("VERUS_Z3_PATH", w.installed.as_os_str())],
    );
    assert!(
        out.text
            .contains("Already set up: VERUS_Z3_PATH points at this z3cache."),
        "{}",
        out.text
    );
    assert!(!out.text.contains("open a new terminal"), "{}", out.text);
    assert_eq!(w.files(), Vec::<String>::new());
}

#[test]
fn without_verus_on_path_setup_still_works() {
    let w = world("setup-no-verus");
    std::fs::remove_file(w.bin.join("verus")).unwrap();
    let out = w.run("/bin/zsh", &["--yes"], "", false);
    let t = &out.text;
    assert_eq!(out.code, 0, "{t}");
    assert!(t.contains("verus     not on PATH. That is fine"), "{t}");
    assert!(t.contains("check     no z3 found from this shell"), "{t}");
    assert!(t.contains("set real_z3 in"), "{t}");
    assert_eq!(w.read(".zshenv").unwrap(), block(ZSH_LINE));
}

#[test]
fn a_verus_that_hangs_is_given_up_on() {
    let w = world("setup-hang");
    std::fs::write(w.release.join("verus"), "#!/bin/sh\nexec sleep 600\n").unwrap();
    let start = std::time::Instant::now();
    let out = w.run("/bin/zsh", &["--print"], "", false);
    assert!(start.elapsed() < std::time::Duration::from_secs(60));
    assert!(
        out.text
            .contains(&format!("verus     {}, with its z3\n", w.release.display())),
        "{}",
        out.text
    );
}

#[test]
fn bare_z3cache_on_a_terminal_points_at_setup() {
    let w = world("setup-bare");
    let out = w.run_with(&w.installed, "/bin/zsh", &[], "", true, &[]);
    assert_eq!(out.code, 0);
    assert!(out.text.contains("z3cache setup"), "{}", out.text);
    assert!(out.text.contains("z3cache help"), "{}", out.text);
    // Not on a terminal, or invoked as z3: the solver, as before.
    let out = run(&w.installed, &[], b"", &[]);
    assert_eq!(out.text(), "stub: unsupported arguments \n");
    let link = w.home.join("z3");
    symlink(&w.installed, &link).unwrap();
    let out = run(&link, &[], b"", &[("Z3CACHE_ASSUME_TTY", OsStr::new("1"))]);
    assert_eq!(out.text(), "stub: unsupported arguments \n");
}
