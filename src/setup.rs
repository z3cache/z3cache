//! `z3cache setup` (DESIGN section 12): makes Verus run z3cache by setting
//! `VERUS_Z3_PATH` in the user's shell startup file.
//!
//! The setting goes in a block between two marker lines, so that running
//! setup again changes nothing and `--undo` can remove exactly what setup
//! added. Nothing outside the markers is ever changed. The file is changed
//! only after the user agrees at the prompt, or with `--yes`.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, IsTerminal, Write};
use std::ops::Range;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::cli::{check_writable, round_trip};
use crate::config::{Config, read_file};
use crate::locate::{self, find_real_solver};
use crate::profile::{SolverProfile, Z3};

/// The first line of the block setup writes.
pub const BEGIN: &str = "# >>> z3cache >>>";
/// The last line of the block setup writes.
pub const END: &str = "# <<< z3cache <<<";

/// The variable Verus reads to find its solver.
const VARIABLE: &str = "VERUS_Z3_PATH";

/// Test-only: set to `1`, z3cache behaves as if stdin and stdout were
/// terminals, so that tests can answer setup's prompts on a pipe. Not meant
/// for users.
const ASSUME_TTY: &str = "Z3CACHE_ASSUME_TTY";

/// How long `verus --version` may take before setup gives up on it.
const VERSION_WAIT: Duration = Duration::from_secs(5);

const USAGE: &str = "\
usage: z3cache setup [--shell zsh|bash|fish] [--yes|--print] [--undo]
  --shell   the shell to set up, instead of the one setup detects
  --yes     change the startup file without asking
  --print   only print what to do; change nothing
  --undo    remove the lines setup added
";

fn assume_tty() -> bool {
    std::env::var_os(ASSUME_TTY).is_some_and(|v| v == "1")
}

/// True if stdin is a terminal: a person typed the command.
pub fn stdin_is_terminal() -> bool {
    assume_tty() || std::io::stdin().is_terminal()
}

/// True if setup may ask questions: stdin and stdout are both terminals.
fn interactive() -> bool {
    assume_tty() || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub fn name(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }

    pub fn from_name(name: &str) -> Option<Shell> {
        match name {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }

    /// The shell an executable is: its file name, without the leading `-`
    /// of a login shell.
    fn of_executable(p: &Path) -> Option<Shell> {
        let name = p.file_name()?.to_str()?;
        Shell::from_name(name.trim_start_matches('-'))
    }
}

/// How the shell was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFrom {
    /// `--shell`.
    Flag,
    /// The process that ran setup: the shell it was typed in.
    Parent,
    /// `$SHELL`, the login shell.
    LoginShell,
}

/// The shell to set up: `--shell`, else the parent process if it is a
/// shell setup knows, else `$SHELL`. `None` if none of them is zsh, bash
/// or fish.
pub fn detect_shell(
    flag: Option<Shell>,
    parent: Option<&Path>,
    shell_var: Option<&OsStr>,
) -> Option<(Shell, ShellFrom)> {
    if let Some(s) = flag {
        return Some((s, ShellFrom::Flag));
    }
    if let Some(s) = parent.and_then(Shell::of_executable) {
        return Some((s, ShellFrom::Parent));
    }
    shell_var
        .and_then(|v| Shell::of_executable(Path::new(v)))
        .map(|s| (s, ShellFrom::LoginShell))
}

/// A shell startup file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub path: PathBuf,
    /// The path as shown to the user, with `~` for the home directory.
    pub shown: String,
}

/// The startup file for `shell`: one that the shells scripts and agents
/// start read too, where the shell has one.
pub fn profile(shell: Shell, home: &Path, xdg_config: Option<&Path>) -> Profile {
    let path = match shell {
        Shell::Zsh => home.join(".zshenv"),
        Shell::Bash => home.join(".bashrc"),
        Shell::Fish => xdg_config
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".config"))
            .join("fish/config.fish"),
    };
    let shown = tilde(&path, home);
    Profile { path, shown }
}

/// `p` with `~/` for `home`, quoted for a shell if it needs to be.
fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if !rest.as_os_str().is_empty() => {
            format!("~/{}", shell_word(&rest.to_string_lossy()))
        }
        _ => shell_word(&p.to_string_lossy()),
    }
}

/// `s` as one shell word: as it is if that is safe, else single-quoted.
fn shell_word(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+@%:,=".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        s.to_string()
    } else {
        single_quoted(s)
    }
}

/// `s` in single quotes, which neither POSIX shells nor fish expand.
fn single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The line that sets `VERUS_Z3_PATH` to `exe` in `shell` (POSIX syntax for
/// an unknown shell). A path under `home` is written as `$HOME/...`, so
/// that the line keeps working if the home directory moves.
pub fn assignment(shell: Option<Shell>, exe: &Path, home: Option<&Path>) -> String {
    let fish = shell == Some(Shell::Fish);
    let escape = |s: &str| {
        let mut o = String::new();
        for c in s.chars() {
            if matches!(c, '"' | '\\' | '$') || (c == '`' && !fish) {
                o.push('\\');
            }
            o.push(c);
        }
        o
    };
    let under_home = home.and_then(|h| {
        let canonical = std::fs::canonicalize(h).ok();
        [Some(h.to_path_buf()), canonical]
            .into_iter()
            .flatten()
            .find_map(|h| exe.strip_prefix(h).ok().map(Path::to_path_buf))
            .filter(|rest| !rest.as_os_str().is_empty())
    });
    let value = match under_home {
        Some(rest) => format!("$HOME/{}", escape(&rest.to_string_lossy())),
        None => escape(&exe.to_string_lossy()),
    };
    if fish {
        format!("set -gx {VARIABLE} \"{value}\"")
    } else {
        format!("export {VARIABLE}=\"{value}\"")
    }
}

/// The block setup writes around `line`.
pub fn block(line: &str) -> String {
    format!("{BEGIN}\n{line}\n{END}\n")
}

/// A command that appends `line` to the file shown as `shown`. The line is
/// single-quoted, so that `$HOME` in it is expanded when the shell starts,
/// not when the command is pasted.
pub fn echo_command(line: &str, shown: &str) -> String {
    format!("echo {} >> {shown}", single_quoted(line))
}

/// What a startup file holds of setup's block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    None,
    /// One block: its bytes, from the start of the first marker line to the
    /// end of the last, and the lines between the markers, trimmed.
    Block {
        range: Range<usize>,
        inner: String,
    },
    /// Markers that do not make one block, at this line (from 1).
    Malformed(usize),
}

fn trim_line(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|b| !b" \t\r\n".contains(b))
        .map_or(0, |i| i + 1);
    &line[..end]
}

/// Finds setup's block in a startup file.
pub fn find_block(text: &[u8]) -> Existing {
    let mut begin: Option<(usize, usize)> = None;
    let mut found: Option<Range<usize>> = None;
    let mut inner = Vec::new();
    let mut offset = 0;
    for (i, line) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        let bare = trim_line(line);
        if bare == BEGIN.as_bytes() {
            if begin.is_some() || found.is_some() {
                return Existing::Malformed(i + 1);
            }
            begin = Some((offset, i + 1));
        } else if bare == END.as_bytes() {
            match begin.take() {
                Some((start, _)) if found.is_none() => found = Some(start..offset + line.len()),
                _ => return Existing::Malformed(i + 1),
            }
        } else if begin.is_some() {
            inner.push(String::from_utf8_lossy(bare).into_owned());
        }
        offset += line.len();
    }
    if let Some((_, n)) = begin {
        return Existing::Malformed(n);
    }
    match found {
        None => Existing::None,
        Some(range) => Existing::Block {
            range,
            inner: inner.join("\n"),
        },
    }
}

/// Lines outside `block` that set `VERUS_Z3_PATH`: (line from 1, text).
pub fn other_settings(text: &[u8], block: Option<&Range<usize>>) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0;
    for (i, line) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        let inside = block.is_some_and(|r| r.contains(&offset));
        offset += line.len();
        let bare = String::from_utf8_lossy(trim_line(line)).into_owned();
        if !inside && bare.contains(VARIABLE) && !bare.trim_start().starts_with('#') {
            found.push((i + 1, bare.trim().to_string()));
        }
    }
    found
}

/// `text` with `block` in place of the existing one, or appended.
pub fn with_block(text: &[u8], existing: &Existing, block: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + block.len() + 1);
    match existing {
        Existing::Block { range, .. } => {
            out.extend_from_slice(&text[..range.start]);
            out.extend_from_slice(block.as_bytes());
            out.extend_from_slice(&text[range.end..]);
        }
        _ => {
            out.extend_from_slice(text);
            out.extend_from_slice(&append_text(text, block));
        }
    }
    out
}

/// What to append to `text` to add `block`: a newline first if the file
/// does not end in one.
fn append_text(text: &[u8], block: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !text.is_empty() && !text.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(block.as_bytes());
    out
}

/// `text` without the bytes in `range`.
pub fn without(text: &[u8], range: &Range<usize>) -> Vec<u8> {
    [&text[..range.start], &text[range.end..]].concat()
}

/// The backup of a startup file: `<file>.z3cache-backup`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".z3cache-backup");
    PathBuf::from(p)
}

/// Copies the file at `path` to its backup, unless a backup exists already.
/// Returns the backup's path if it made one.
fn backup(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let b = backup_path(path);
    if b.symlink_metadata().is_ok() {
        return Ok(None);
    }
    let mut src = std::fs::File::open(path)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&b)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.set_permissions(src.metadata()?.permissions())?;
    Ok(Some(b))
}

/// Appends `addition` to the file at `path`, creating the file and its
/// directory if they do not exist.
fn append(path: &Path, addition: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    f.write_all(addition)
}

/// Replaces the contents of the file at `path`, keeping its permissions. A
/// symlink stays a symlink: the file it points at is replaced. The new
/// contents are written to a temporary file next to it and renamed into
/// place, so that the file is never left half-written.
fn rewrite(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let perms = std::fs::metadata(&target)?.permissions();
    let mut name = OsString::from(".");
    name.push(target.file_name().unwrap_or(OsStr::new("profile")));
    name.push(format!(".z3cache-{}", std::process::id()));
    let tmp = target.with_file_name(name);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.set_permissions(perms)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Asks a yes/no question; yes unless the answer starts with `n`. No answer
/// (end of input) is no.
fn ask(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().read_line(&mut answer);
    if !std::io::stdin().is_terminal() {
        // The answer was not echoed.
        println!("{}", answer.trim_end());
    }
    match read {
        Ok(0) | Err(_) => false,
        Ok(_) => {
            let a = answer.trim().to_ascii_lowercase();
            a.is_empty() || a == "y" || a == "yes"
        }
    }
}

/// `text` wrapped at 78 columns, each line starting with `indent`.
fn wrap(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut line = String::from(indent);
    for word in text.split_whitespace() {
        if line.len() > indent.len() && line.len() + 1 + word.len() > 78 {
            out.push_str(&line);
            out.push('\n');
            line = String::from(indent);
        }
        if line.len() > indent.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// `text` with each line indented by four spaces.
fn indented(text: &str) -> String {
    text.lines().map(|l| format!("    {l}\n")).collect()
}

/// The version `verus --version` prints: the value of its `Version:` line,
/// or else its first line.
pub fn parse_version(out: &str) -> Option<String> {
    let lines = || out.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find_map(|l| l.strip_prefix("Version:").map(|v| v.trim().to_string()))
        .or_else(|| lines().next().map(str::to_string))
}

/// Runs `verus --version`, giving up after [`VERSION_WAIT`].
fn verus_version(verus: &Path) -> Option<String> {
    let child = Command::new(verus)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let group = child.id() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(VERSION_WAIT) {
        Ok(Ok(out)) => parse_version(&String::from_utf8_lossy(&out.stdout)),
        Ok(Err(_)) => None,
        Err(_) => {
            // SAFETY: kill(2) on the process group of our own child.
            unsafe { libc::kill(-group, libc::SIGKILL) };
            None
        }
    }
}

#[derive(Debug, Default)]
struct Options {
    shell: Option<Shell>,
    yes: bool,
    print: bool,
    undo: bool,
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let shell = match a.as_str() {
            "--yes" | "-y" => {
                o.yes = true;
                continue;
            }
            "--print" => {
                o.print = true;
                continue;
            }
            "--undo" => {
                o.undo = true;
                continue;
            }
            "--shell" => it.next().ok_or("--shell needs a value")?.as_str(),
            other => match other.strip_prefix("--shell=") {
                Some(v) => v,
                None => return Err(format!("unknown argument {other:?}")),
            },
        };
        o.shell = Some(
            Shell::from_name(shell)
                .ok_or_else(|| format!("unknown shell {shell:?}: use zsh, bash or fish"))?,
        );
    }
    if o.yes && o.print {
        return Err("--yes and --print do not go together".into());
    }
    Ok(o)
}

/// What setup found out before deciding what to do.
struct Setup {
    opts: Options,
    interactive: bool,
    home: Option<PathBuf>,
    shell: Option<(Shell, ShellFrom)>,
    /// The startup file, if the shell is known and so is `HOME`.
    profile: Option<Profile>,
    /// The line that sets `VERUS_Z3_PATH`.
    line: String,
    /// `VERUS_Z3_PATH` already points at this program.
    env_is_me: bool,
}

/// Runs `z3cache setup` with the arguments after `setup`.
pub fn run(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("z3cache setup: {e}");
            eprint!("{USAGE}");
            return 2;
        }
    };
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let me = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("z3cache: cannot tell where this program is: {e}");
            return 1;
        }
    };
    let home = env("HOME").map(PathBuf::from);
    let shell = detect_shell(
        opts.shell,
        locate::parent_executable().as_deref(),
        env("SHELL").as_deref(),
    );
    let xdg = env("XDG_CONFIG_HOME").map(PathBuf::from);
    let profile = match (shell, &home) {
        (Some((s, _)), Some(h)) => Some(profile(s, h, xdg.as_deref())),
        _ => None,
    };
    let env_is_me = env(VARIABLE)
        .and_then(|p| std::fs::canonicalize(p).ok())
        .is_some_and(|p| p == me);

    print!("{}", verus_line(env("PATH").as_deref()));
    println!("z3cache   {}", me.display());
    print!("{}", shell_line(shell, env("SHELL").as_deref()));
    println!();

    let s = Setup {
        line: assignment(shell.map(|s| s.0), &me, home.as_deref()),
        interactive: interactive() && !opts.print,
        opts,
        home,
        shell,
        profile,
        env_is_me,
    };
    if s.opts.undo { s.undo() } else { s.install() }
}

/// The `verus` line: where the verus on `PATH` is, its version, and
/// whether its z3 is next to it.
fn verus_line(path: Option<&OsStr>) -> String {
    let Some(verus) = locate::first_on_path(path, locate::VERUS) else {
        return label_wrap(
            "verus     ",
            "not on PATH. That is fine: when Verus runs z3cache, z3cache uses the z3 \
             in Verus's directory, as long as Verus runs from there.",
        );
    };
    let dir = verus.parent().unwrap_or(Path::new("/"));
    let version = verus_version(&verus)
        .map(|v| format!(", version {v}"))
        .unwrap_or_default();
    let z3 = if locate::is_executable(&dir.join(Z3.binary_name())) {
        ", with its z3".to_string()
    } else {
        "; no z3 next to it, so z3cache looks next to the program that runs it".to_string()
    };
    format!("verus     {}{version}{z3}\n", dir.display())
}

/// The `shell` line: which shell and how it was chosen.
fn shell_line(shell: Option<(Shell, ShellFrom)>, shell_var: Option<&OsStr>) -> String {
    const OTHER: &str = "--shell zsh|bash|fish to choose another";
    match shell {
        Some((s, ShellFrom::Flag)) => format!("shell     {} (from --shell)\n", s.name()),
        Some((s, ShellFrom::Parent)) => format!(
            "shell     {}, detected from the shell you ran this in ({OTHER})\n",
            s.name()
        ),
        Some((s, ShellFrom::LoginShell)) => format!(
            "shell     {}, your login shell from $SHELL ({OTHER})\n",
            s.name()
        ),
        None => {
            let named = match shell_var {
                Some(v) => format!("$SHELL is {}", v.to_string_lossy()),
                None => "$SHELL is not set".into(),
            };
            format!(
                "shell     not zsh, bash or fish ({named}); --shell zsh|bash|fish if you use one\n"
            )
        }
    }
}

impl Setup {
    /// Where the line goes and why, for the explanation before it.
    fn about_file(&self, p: &Profile) -> String {
        match self.shell.map(|s| s.0) {
            Some(Shell::Zsh) => format!(
                "{}, which every zsh reads, so scripts and agents get it too (~/.zshrc \
                 would cover interactive shells only)",
                p.shown
            ),
            Some(Shell::Bash) if cfg!(target_os = "macos") => format!(
                "{}. On macOS, Terminal starts bash as a login shell, which reads \
                 ~/.bash_profile instead: if yours does not source ~/.bashrc, add the \
                 line there too",
                p.shown
            ),
            _ => p.shown.clone(),
        }
    }

    fn install(&self) -> i32 {
        let Some(p) = &self.profile else {
            return self.no_profile();
        };
        let text = match read_file(&p.path) {
            Ok(t) => t,
            Err(e) => {
                println!("Cannot read {}: {e}. Nothing changed.", p.shown);
                return 1;
            }
        };
        let exists = text.is_some();
        let text = text.unwrap_or_default();
        let existing = find_block(&text);
        let range = match &existing {
            Existing::Block { range, .. } => Some(range),
            _ => None,
        };
        let others = other_settings(&text, range);
        match &existing {
            Existing::Malformed(n) => {
                print!(
                    "{}",
                    wrap(
                        &format!(
                            "{} has an incomplete z3cache block (line {n}). Remove it by \
                             hand, then run `z3cache setup` again. Nothing changed.",
                            p.shown
                        ),
                        ""
                    )
                );
                return 1;
            }
            Existing::Block { inner, .. } if *inner == self.line => {
                println!("Already set up in {}.", p.shown);
                return self.done(p, false);
            }
            Existing::None if others.iter().any(|(_, l)| *l == self.line) => {
                let n = others.iter().find(|(_, l)| *l == self.line).unwrap().0;
                println!("Already set up in {} (line {n}).", p.shown);
                return self.done(p, false);
            }
            Existing::None if self.env_is_me && !self.opts.yes => {
                println!("Already set up: {VARIABLE} points at this z3cache.");
                return self.done(p, false);
            }
            _ => {}
        }

        let replacing = matches!(existing, Existing::Block { .. });
        if let Existing::Block { inner, .. } = &existing {
            println!(
                "{} already has z3cache lines, for another setting:\n\n{}\nThey would become:\n\n{}",
                p.shown,
                indented(inner),
                indented(&self.line)
            );
        } else {
            print!(
                "{}",
                wrap(
                    &format!(
                        "To make Verus use z3cache, {} {}{}",
                        if self.interactive || self.opts.yes {
                            "setup adds these lines to"
                        } else {
                            "add a line to"
                        },
                        self.about_file(p),
                        if self.interactive || self.opts.yes {
                            ":"
                        } else {
                            "."
                        }
                    ),
                    ""
                )
            );
            println!();
            if self.interactive || self.opts.yes {
                print!("{}", indented(&block(&self.line)));
                println!();
            }
            for (n, l) in &others {
                print!(
                    "{}",
                    wrap(
                        &format!(
                            "Line {n} of {} also sets {VARIABLE} ({l}); the new lines come \
                             after it, so they win.",
                            p.shown
                        ),
                        ""
                    )
                );
                println!();
            }
        }

        let go = if self.opts.yes {
            true
        } else if self.interactive {
            let yes = ask(if replacing {
                "Replace them?"
            } else {
                "Add them now?"
            });
            if !yes {
                println!("Nothing changed.\n");
            }
            yes
        } else {
            false
        };
        if !go {
            return self.print_instructions(p, replacing);
        }

        let result = (|| -> std::io::Result<Option<PathBuf>> {
            let saved = if exists { backup(&p.path)? } else { None };
            match &existing {
                Existing::Block { .. } => {
                    rewrite(&p.path, &with_block(&text, &existing, &block(&self.line)))?
                }
                _ => append(&p.path, &append_text(&text, &block(&self.line)))?,
            }
            Ok(saved)
        })();
        match result {
            Ok(saved) => {
                let verb = if replacing { "Updated" } else { "Added to" };
                match saved {
                    Some(b) => println!(
                        "{verb} {} (the file as it was is in {}).",
                        p.shown,
                        self.shown(&b)
                    ),
                    None => println!("{verb} {}.", p.shown),
                }
                self.done(p, true)
            }
            Err(e) => {
                println!("Cannot change {}: {e}.\n", p.shown);
                self.print_instructions(p, replacing);
                1
            }
        }
    }

    /// `p` as shown to the user.
    fn shown(&self, p: &Path) -> String {
        match &self.home {
            Some(h) => tilde(p, h),
            None => p.display().to_string(),
        }
    }

    /// What to do by hand: the command to paste, then the next steps.
    fn print_instructions(&self, p: &Profile, replacing: bool) -> i32 {
        if replacing {
            print!(
                "{}",
                wrap(
                    &format!(
                        "To use this z3cache, run `z3cache setup --yes`, or change the line \
                         in {} by hand.",
                        p.shown
                    ),
                    ""
                )
            );
        } else {
            println!("Copy and paste this command to add it:\n");
            print!("{}", indented(&echo_command(&self.line, &p.shown)));
            println!("\nor run `z3cache setup --yes` to have setup add it.");
        }
        println!();
        self.next_steps(Some(p), true);
        0
    }

    /// Unknown shell or no `HOME`: the POSIX line to add by hand.
    fn no_profile(&self) -> i32 {
        let why = if self.home.is_none() {
            "HOME is not set, so setup cannot tell where your shell's startup file is"
        } else {
            "setup knows the startup files of zsh, bash and fish (--shell zsh|bash|fish)"
        };
        println!("Add this line to your shell's startup file:\n");
        print!("{}", indented(&self.line));
        println!();
        print!("{}", wrap(&format!("({why}.)"), ""));
        println!();
        self.next_steps(None, true);
        if self.opts.yes { 1 } else { 0 }
    }

    /// After setup is in place: the self-check, then the next steps.
    fn done(&self, p: &Profile, changed: bool) -> i32 {
        if !self.opts.print {
            print!("{}", self_check());
        }
        println!();
        self.next_steps((changed || !self.env_is_me).then_some(p), false);
        0
    }

    fn next_steps(&self, p: Option<&Profile>, doctor: bool) {
        let mut steps = Vec::new();
        match p {
            Some(p) => {
                steps.push(format!(
                    "open a new terminal (or run `source {}` in this one)",
                    p.shown
                ));
            }
            None if !self.env_is_me => steps.push("open a new terminal".into()),
            None => {}
        }
        if doctor {
            steps.push("run `z3cache doctor` to check the setup".into());
        }
        steps.push("use Verus as usual".into());
        steps.push("run `z3cache stats` to see what the cache answered".into());
        println!("Next:");
        for (i, s) in steps.iter().enumerate() {
            println!("  {}. {s}", i + 1);
        }
    }

    fn undo(&self) -> i32 {
        let Some(p) = &self.profile else {
            println!(
                "No startup file to change: {}. Use --shell zsh|bash|fish.",
                if self.home.is_none() {
                    "HOME is not set"
                } else {
                    "setup knows only zsh, bash and fish"
                }
            );
            return 0;
        };
        let text = match read_file(&p.path) {
            Ok(Some(t)) => t,
            Ok(None) => {
                println!("{} does not exist: nothing to remove.", p.shown);
                return 0;
            }
            Err(e) => {
                println!("Cannot read {}: {e}. Nothing changed.", p.shown);
                return 1;
            }
        };
        let range = match find_block(&text) {
            Existing::Block { range, .. } => range,
            Existing::None => {
                println!("No z3cache lines in {}: nothing to remove.", p.shown);
                for (n, l) in other_settings(&text, None) {
                    println!(
                        "Line {n} sets {VARIABLE} by other means ({l}); setup leaves it alone."
                    );
                }
                return 0;
            }
            Existing::Malformed(n) => {
                println!(
                    "{} has an incomplete z3cache block (line {n}); remove it by hand. \
                     Nothing changed.",
                    p.shown
                );
                return 1;
            }
        };
        println!(
            "z3cache's lines in {}:\n\n{}",
            p.shown,
            indented(&String::from_utf8_lossy(&text[range.clone()]))
        );
        let go = if self.opts.print {
            false
        } else if self.opts.yes {
            true
        } else if self.interactive {
            ask("Remove them?")
        } else {
            println!("Run `z3cache setup --undo --yes` to remove them.");
            return 0;
        };
        if !go {
            println!("Nothing changed.");
            return 0;
        }
        let result = backup(&p.path).and_then(|_| rewrite(&p.path, &without(&text, &range)));
        match result {
            Ok(()) => {
                let unset = match self.shell.map(|s| s.0) {
                    Some(Shell::Fish) => "set -e VERUS_Z3_PATH",
                    _ => "unset VERUS_Z3_PATH",
                };
                print!(
                    "{}",
                    wrap(
                        &format!(
                            "Removed from {}. Terminals already open keep {VARIABLE} until \
                             you run `{unset}` in them.",
                            p.shown
                        ),
                        ""
                    )
                );
                0
            }
            Err(e) => {
                println!("Cannot change {}: {e}. Nothing changed.", p.shown);
                1
            }
        }
    }
}

/// The same cold and warm query as `doctor` runs, against a temporary
/// cache, with the real solver found as a session would find it from here.
fn self_check() -> String {
    let cfg = Config::load();
    let real = match find_real_solver(&cfg, &Z3) {
        Ok(r) => r,
        Err(e) if cfg.real_z3.is_some() => {
            return check_text(&format!("z3 not usable: {e}."));
        }
        Err(_) => {
            return check_text(&format!(
                "no z3 found from this shell to test with. When Verus runs z3cache, \
                 z3cache uses the z3 next to Verus; if Verus reports that z3 is missing, {}.",
                cfg.how_to_set("real_z3")
            ));
        }
    };
    match &cfg.dir {
        None => return check_text(&format!("no cache directory: {}.", cfg.how_to_set("dir"))),
        Some(d) => {
            if let Err(e) = check_writable(d) {
                return check_text(&format!(
                    "the cache directory {} is not writable ({e}): {}.",
                    d.display(),
                    cfg.how_to_set("dir")
                ));
            }
        }
    }
    match round_trip(&real.path) {
        Ok(_) => check_text(&format!(
            "ok: z3 answered a test query, then the cache answered it again (z3: {}, {}).",
            real.path.display(),
            real.how(&cfg)
        )),
        Err(e) => check_text(&format!("FAILED: {e}. Run `z3cache doctor` for details.")),
    }
}

/// A `check` line, wrapped and indented under its label.
fn check_text(text: &str) -> String {
    label_wrap("check     ", text)
}

/// `text` wrapped with `label` before the first line and as much space
/// before the others.
fn label_wrap(label: &str, text: &str) -> String {
    let pad = " ".repeat(label.len());
    wrap(text, &pad).replacen(&pad, label, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_are_detected_in_order() {
        let zsh = Some(Path::new("/bin/zsh"));
        let fish = Some(OsStr::new("/opt/homebrew/bin/fish"));
        assert_eq!(
            detect_shell(Some(Shell::Bash), zsh, fish),
            Some((Shell::Bash, ShellFrom::Flag))
        );
        assert_eq!(
            detect_shell(None, zsh, fish),
            Some((Shell::Zsh, ShellFrom::Parent))
        );
        // A login shell's name starts with `-`.
        assert_eq!(
            detect_shell(None, Some(Path::new("/bin/-bash")), None),
            Some((Shell::Bash, ShellFrom::Parent))
        );
        // A parent that is not a shell setup knows: $SHELL.
        for parent in [
            Some(Path::new("/usr/bin/python3")),
            Some(Path::new("/bin/sh")),
            None,
        ] {
            assert_eq!(
                detect_shell(None, parent, fish),
                Some((Shell::Fish, ShellFrom::LoginShell))
            );
        }
        assert_eq!(
            detect_shell(None, None, Some(OsStr::new("-zsh"))),
            Some((Shell::Zsh, ShellFrom::LoginShell))
        );
        assert_eq!(
            detect_shell(None, None, Some(OsStr::new("/bin/tcsh"))),
            None
        );
        assert_eq!(detect_shell(None, Some(Path::new("/bin/zsh5")), None), None);
        assert_eq!(detect_shell(None, None, None), None);
    }

    #[test]
    fn startup_files() {
        let home = Path::new("/home/u");
        let p = profile(Shell::Zsh, home, None);
        assert_eq!(
            (p.path.to_str().unwrap(), &*p.shown),
            ("/home/u/.zshenv", "~/.zshenv")
        );
        let p = profile(Shell::Bash, home, Some(Path::new("/x")));
        assert_eq!(
            (p.path.to_str().unwrap(), &*p.shown),
            ("/home/u/.bashrc", "~/.bashrc")
        );
        let p = profile(Shell::Fish, home, None);
        assert_eq!(p.shown, "~/.config/fish/config.fish");
        let p = profile(Shell::Fish, home, Some(Path::new("/home/u/cfg")));
        assert_eq!(p.shown, "~/cfg/fish/config.fish");
        let p = profile(Shell::Fish, home, Some(Path::new("/etc/my config")));
        assert_eq!(p.shown, "'/etc/my config/fish/config.fish'");
        let p = profile(Shell::Zsh, Path::new("/home/it's me"), None);
        assert_eq!(p.shown, "~/.zshenv");
    }

    #[test]
    fn assignments_use_home_and_quote() {
        let home = Some(Path::new("/nonexistent-home/u"));
        let exe = Path::new("/nonexistent-home/u/.cargo/bin/z3cache");
        assert_eq!(
            assignment(Some(Shell::Zsh), exe, home),
            "export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\""
        );
        assert_eq!(
            assignment(None, exe, home),
            "export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\""
        );
        assert_eq!(
            assignment(Some(Shell::Fish), exe, home),
            "set -gx VERUS_Z3_PATH \"$HOME/.cargo/bin/z3cache\""
        );
        let odd = Path::new("/opt/a \"b\" $c `d` \\e/z3cache");
        assert_eq!(
            assignment(Some(Shell::Bash), odd, home),
            "export VERUS_Z3_PATH=\"/opt/a \\\"b\\\" \\$c \\`d\\` \\\\e/z3cache\""
        );
        assert_eq!(
            assignment(Some(Shell::Fish), odd, home),
            "set -gx VERUS_Z3_PATH \"/opt/a \\\"b\\\" \\$c `d` \\\\e/z3cache\""
        );
        assert_eq!(
            assignment(Some(Shell::Zsh), exe, None),
            "export VERUS_Z3_PATH=\"/nonexistent-home/u/.cargo/bin/z3cache\""
        );
    }

    #[test]
    fn the_echo_command_quotes_the_line() {
        let line = "export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\"";
        assert_eq!(
            echo_command(line, "~/.zshenv"),
            "echo 'export VERUS_Z3_PATH=\"$HOME/.cargo/bin/z3cache\"' >> ~/.zshenv"
        );
        assert_eq!(
            echo_command("export VERUS_Z3_PATH=\"/it's\"", "~/.bashrc"),
            "echo 'export VERUS_Z3_PATH=\"/it'\\''s\"' >> ~/.bashrc"
        );
    }

    #[test]
    fn blocks_are_found_replaced_and_removed() {
        let line = "export VERUS_Z3_PATH=\"$HOME/z\"";
        let b = block(line);
        assert_eq!(find_block(b""), Existing::None);
        assert_eq!(find_block(b"export PATH=x\n"), Existing::None);

        // Appending keeps everything, and adds a newline if missing.
        for before in ["", "a\n", "a", "a\n\n"] {
            let after = with_block(before.as_bytes(), &find_block(before.as_bytes()), &b);
            let sep = if before.is_empty() || before.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            assert_eq!(
                String::from_utf8(after.clone()).unwrap(),
                format!("{before}{sep}{b}")
            );
            let Existing::Block { range, inner } = find_block(&after) else {
                panic!("no block in {after:?}")
            };
            assert_eq!(inner, line);
            assert_eq!(
                String::from_utf8(without(&after, &range)).unwrap(),
                format!("{before}{sep}")
            );
        }

        // Replacing touches nothing outside the markers, CRLF and trailing
        // spaces included.
        let text = format!("a\r\n{BEGIN}  \r\nexport VERUS_Z3_PATH=/old\r\n{END}\r\nb");
        let existing = find_block(text.as_bytes());
        let Existing::Block { inner, .. } = &existing else {
            panic!("{existing:?}")
        };
        assert_eq!(inner, "export VERUS_Z3_PATH=/old");
        let replaced = with_block(text.as_bytes(), &existing, &b);
        assert_eq!(String::from_utf8(replaced).unwrap(), format!("a\r\n{b}b"));

        // A block at the end without a final newline.
        let text = format!("a\n{BEGIN}\nx\n{END}");
        let Existing::Block { range, .. } = find_block(text.as_bytes()) else {
            panic!()
        };
        assert_eq!(without(text.as_bytes(), &range), b"a\n");
    }

    #[test]
    fn broken_markers_are_malformed() {
        for (text, line) in [
            (format!("{BEGIN}\nx\n"), 1),
            (format!("x\n{END}\n"), 2),
            (format!("{BEGIN}\n{BEGIN}\n{END}\n"), 2),
            (format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n"), 3),
            (format!("{END}\n{BEGIN}\n"), 1),
        ] {
            assert_eq!(
                find_block(text.as_bytes()),
                Existing::Malformed(line),
                "{text:?}"
            );
        }
    }

    #[test]
    fn other_settings_outside_the_block() {
        let text = format!(
            "# VERUS_Z3_PATH in a comment\nexport VERUS_Z3_PATH=/a\n{BEGIN}\nexport VERUS_Z3_PATH=/b\n{END}\n  set -gx VERUS_Z3_PATH /c\n"
        );
        let Existing::Block { range, .. } = find_block(text.as_bytes()) else {
            panic!()
        };
        assert_eq!(
            other_settings(text.as_bytes(), Some(&range)),
            [
                (2, "export VERUS_Z3_PATH=/a".to_string()),
                (6, "set -gx VERUS_Z3_PATH /c".to_string())
            ]
        );
        assert_eq!(other_settings(text.as_bytes(), None).len(), 3);
    }

    #[test]
    fn arguments() {
        let parse = |a: &[&str]| parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let o = parse(&["--shell", "fish", "--yes"]).unwrap();
        assert_eq!(
            (o.shell, o.yes, o.print, o.undo),
            (Some(Shell::Fish), true, false, false)
        );
        let o = parse(&["--shell=bash", "--undo", "--print"]).unwrap();
        assert_eq!(
            (o.shell, o.yes, o.print, o.undo),
            (Some(Shell::Bash), false, true, true)
        );
        assert!(parse(&["--shell"]).is_err());
        assert!(parse(&["--shell", "tcsh"]).is_err());
        assert!(parse(&["--yes", "--print"]).is_err());
        assert!(parse(&["--force"]).is_err());
    }

    #[test]
    fn versions() {
        let verus = "Verus\n  Version: 0.2026.10.26.d17fbd0\n  Profile: release\n";
        assert_eq!(
            parse_version(verus).as_deref(),
            Some("0.2026.10.26.d17fbd0")
        );
        assert_eq!(
            parse_version("\nverus 1.2\nmore\n").as_deref(),
            Some("verus 1.2")
        );
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn wrapping() {
        let long = "word ".repeat(40);
        let w = label_wrap("check     ", &long);
        assert!(w.starts_with("check     word"), "{w}");
        assert!(w.lines().all(|l| l.len() <= 78), "{w}");
        assert!(
            w.lines().skip(1).all(|l| l.starts_with("          word")),
            "{w}"
        );
    }
}
