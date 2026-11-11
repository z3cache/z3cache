//! Finding the real solver and Verus (DESIGN section 4).
//!
//! Nothing here may fail or hang the build (rule 2): every lookup is a few
//! system calls, and any error only moves on to the next rule.

use std::ffi::{OsStr, OsString};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::config::{Config, Source};
use crate::profile::SolverProfile;

/// The client's driver, next to which a Verus release keeps its `z3`.
pub const VERUS: &str = "verus";

/// Which rule found the real solver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The `real_z3` setting, from the environment or the config file.
    Setting,
    /// Next to the executable of the process that started z3cache: Verus's
    /// driver, in the Verus release directory.
    NextToParent(PathBuf),
    /// Next to the first `verus` on `PATH`, symlinks resolved.
    NextToVerus(PathBuf),
    /// The first solver on `PATH` that is not z3cache.
    OnPath,
}

/// The real solver and how it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealSolver {
    /// Absolute, with symlinks resolved: the one file that is identified,
    /// hashed and run (DESIGN section 4).
    pub path: PathBuf,
    pub found: Found,
}

impl RealSolver {
    /// How the solver was found, for `doctor` and the log.
    pub fn how(&self, cfg: &Config) -> String {
        match &self.found {
            Found::Setting => format!(
                "from {}",
                cfg.origin("real_z3").unwrap_or_else(|| "real_z3".into())
            ),
            Found::NextToParent(p) => {
                format!("next to {}, the program that started z3cache", p.display())
            }
            Found::NextToVerus(p) => format!("next to {}, the verus on PATH", p.display()),
            Found::OnPath => "the first z3 on PATH that is not z3cache".into(),
        }
    }
}

/// What the lookup reads from the process and its environment, so that
/// tests can supply their own.
pub struct Surroundings<'a> {
    /// The executable of the parent process, if it can be told.
    pub parent_exe: &'a dyn Fn() -> Option<PathBuf>,
    /// `PATH`.
    pub path: Option<OsString>,
    /// Device and inode of this program, so that it never runs itself.
    pub me: Option<(u64, u64)>,
}

/// The device and inode of the file `p` points at.
pub fn file_id(p: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

/// The real solver for this process: [`find_real_solver_in`] with the
/// process's own surroundings.
pub fn find_real_solver(cfg: &Config, profile: &dyn SolverProfile) -> Result<RealSolver, String> {
    let s = Surroundings {
        parent_exe: &parent_executable,
        path: std::env::var_os("PATH"),
        me: std::env::current_exe().ok().as_deref().and_then(file_id),
    };
    find_real_solver_in(cfg, profile, &s)
}

/// Finds the real solver, by the first rule that gives an executable file
/// that is not z3cache:
///
/// 1. the `real_z3` setting; if it is set but unusable, that is an error. A
///    relative path, even a bare name, is taken from the current directory;
/// 2. the solver next to the parent process's executable, symlinks
///    resolved;
/// 3. the solver next to the first `verus` on `PATH`, symlinks resolved;
/// 4. the first solver on `PATH`.
///
/// The result is canonical: absolute, with every symlink resolved, so that
/// the file whose identity is checked, memoised and hashed is the one that
/// is run, whatever the current directory or `PATH` of a later step.
pub fn find_real_solver_in(
    cfg: &Config,
    profile: &dyn SolverProfile,
    s: &Surroundings<'_>,
) -> Result<RealSolver, String> {
    let name = profile.binary_name();
    let usable = |p: &Path| -> Option<PathBuf> {
        let p = canonical_executable(p)?;
        (s.me.is_none() || file_id(&p) != s.me).then_some(p)
    };
    if let Some(p) = &cfg.real_z3 {
        let given = match cfg.source("real_z3") {
            Source::Env => format!("Z3CACHE_REAL_Z3={}", p.display()),
            _ => format!(
                "real_z3 = {} ({})",
                p.display(),
                cfg.origin("real_z3").unwrap_or_default()
            ),
        };
        let Some(path) = canonical_executable(p) else {
            return Err(format!("{given} is not an executable file"));
        };
        if s.me.is_some() && file_id(&path) == s.me {
            return Err(format!("{given} points at z3cache itself"));
        }
        return Ok(RealSolver {
            path,
            found: Found::Setting,
        });
    }
    if let Some(parent) = (s.parent_exe)().and_then(|p| std::fs::canonicalize(p).ok())
        && let Some(path) = usable(&parent.with_file_name(name))
    {
        return Ok(RealSolver {
            path,
            found: Found::NextToParent(parent),
        });
    }
    if let Some(verus) = first_on_path(s.path.as_deref(), VERUS)
        && let Some(path) = usable(&verus.with_file_name(name))
    {
        return Ok(RealSolver {
            path,
            found: Found::NextToVerus(verus),
        });
    }
    for dir in std::env::split_paths(s.path.as_deref().unwrap_or_default()) {
        if let Some(path) = usable(&dir.join(name)) {
            return Ok(RealSolver {
                path,
                found: Found::OnPath,
            });
        }
    }
    Err(format!(
        "no real {name} found: none next to the program that started z3cache, \
         next to verus on PATH, or on PATH; run `z3cache setup`, or {}",
        cfg.how_to_set("real_z3")
    ))
}

/// `p` absolute and with symlinks resolved, if it is an executable file.
fn canonical_executable(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok().filter(|c| is_executable(c))
}

/// The first executable file called `name` in a directory of `path` (a
/// `PATH` value), with symlinks resolved.
pub fn first_on_path(path: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path.unwrap_or_default())
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
        .and_then(|p| std::fs::canonicalize(p).ok())
}

pub fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The executable of this process's parent, as the kernel reports it, or
/// `None` if it cannot be told.
pub fn parent_executable() -> Option<PathBuf> {
    executable_of(std::os::unix::process::parent_id())
}

#[cfg(target_os = "linux")]
fn executable_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn executable_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath(3) writes at most `buf.len()` bytes into `buf`.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n)
        .ok()
        .filter(|&n| n > 0 && n <= buf.len())?;
    buf.truncate(n);
    Some(PathBuf::from(OsString::from_vec(buf)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn executable_of(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Z3;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn config(real_z3: Option<&str>) -> Config {
        let lookup = |n: &str| match (n, real_z3) {
            ("Z3CACHE_REAL_Z3", Some(v)) => Some(OsString::from(v)),
            ("Z3CACHE_CONFIG", _) => Some(OsString::from("/nonexistent/config.toml")),
            _ => None,
        };
        Config::from_sources(&lookup, &|_| Ok(None))
    }

    fn executable(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A Verus release (`release/{verus,z3,rust_verify}`), a `bin` with a
    /// symlink to its `verus`, a plain `z3` in `other`, and z3cache in `me`.
    struct Layout {
        root: PathBuf,
        release: PathBuf,
        bin: PathBuf,
        other: PathBuf,
        me: PathBuf,
    }

    fn layout(name: &str) -> Layout {
        let root = crate::test_dir("locate", name);
        let l = Layout {
            release: root.join("release"),
            bin: root.join("bin"),
            other: root.join("other"),
            me: root.join("me/z3cache"),
            root,
        };
        for f in ["verus", "z3", "rust_verify"] {
            executable(&l.release.join(f));
        }
        executable(&l.other.join("z3"));
        executable(&l.me);
        std::fs::create_dir_all(&l.bin).unwrap();
        symlink(l.release.join("verus"), l.bin.join("verus")).unwrap();
        l
    }

    fn real(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    fn find(
        cfg: &Config,
        parent: Option<&Path>,
        path: &[&Path],
        me: &Path,
    ) -> Result<RealSolver, String> {
        let parent = parent.map(Path::to_path_buf);
        let parent_exe = move || parent.clone();
        let s = Surroundings {
            parent_exe: &parent_exe,
            path: Some(std::env::join_paths(path).unwrap()),
            me: file_id(me),
        };
        find_real_solver_in(cfg, &Z3, &s)
    }

    #[test]
    fn the_setting_comes_first_and_an_invalid_one_is_an_error() {
        let l = layout("setting");
        let other = l.other.join("z3");
        let cfg = config(Some(other.to_str().unwrap()));
        let found = find(&cfg, Some(&l.release.join("rust_verify")), &[&l.bin], &l.me).unwrap();
        assert_eq!((found.path, found.found), (real(&other), Found::Setting));

        let missing = config(Some("/nonexistent/z3"));
        let e = find(
            &missing,
            Some(&l.release.join("rust_verify")),
            &[&l.bin],
            &l.me,
        )
        .unwrap_err();
        assert_eq!(
            e,
            "Z3CACHE_REAL_Z3=/nonexistent/z3 is not an executable file"
        );
        let me = config(Some(l.me.to_str().unwrap()));
        let e = find(&me, None, &[], &l.me).unwrap_err();
        assert!(e.ends_with("points at z3cache itself"), "{e}");
        let _ = std::fs::remove_dir_all(&l.root);
    }

    #[test]
    fn next_to_the_parent_then_next_to_verus_then_on_path() {
        let l = layout("order");
        let cfg = config(None);
        let parent = l.release.join("rust_verify");

        let found = find(&cfg, Some(&parent), &[&l.other, &l.bin], &l.me).unwrap();
        assert_eq!(found.path, real(&l.release).join("z3"));
        assert_eq!(found.found, Found::NextToParent(real(&parent)));

        // A parent with no solver next to it: the verus on PATH, through
        // its symlink.
        let found = find(
            &cfg,
            Some(&l.other.join("none")),
            &[&l.other, &l.bin],
            &l.me,
        );
        let found = found.unwrap();
        assert_eq!(found.path, real(&l.release).join("z3"));
        assert_eq!(
            found.found,
            Found::NextToVerus(real(&l.release).join("verus"))
        );

        // No parent and no verus: the first z3 on PATH.
        let found = find(&cfg, None, &[&l.root.join("empty"), &l.other], &l.me).unwrap();
        assert_eq!(
            (found.path, found.found),
            (real(&l.other.join("z3")), Found::OnPath)
        );

        // Nothing at all.
        let e = find(&cfg, None, &[&l.root.join("empty")], &l.me).unwrap_err();
        assert!(e.starts_with("no real z3 found"), "{e}");
        assert!(e.contains("run `z3cache setup`"), "{e}");
        assert!(e.contains("Z3CACHE_REAL_Z3"), "{e}");
        let _ = std::fs::remove_dir_all(&l.root);
    }

    #[test]
    fn the_parent_is_resolved_through_symlinks() {
        let l = layout("parent-link");
        let link = l.root.join("links/rust_verify");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(l.release.join("rust_verify"), &link).unwrap();
        let found = find(&config(None), Some(&link), &[], &l.me).unwrap();
        assert_eq!(found.path, real(&l.release).join("z3"));
        let _ = std::fs::remove_dir_all(&l.root);
    }

    #[test]
    fn unusable_candidates_fall_through() {
        let l = layout("unusable");
        let cfg = config(None);
        let parent = l.release.join("rust_verify");

        // A z3 next to the parent that is z3cache itself is skipped, as is
        // one that is not executable, a directory, or a dangling symlink.
        let ours = l.root.join("ours");
        std::fs::create_dir_all(&ours).unwrap();
        executable(&ours.join("rust_verify"));
        symlink(&l.me, ours.join("z3")).unwrap();
        let plain = l.root.join("plain");
        std::fs::create_dir_all(plain.join("z3")).unwrap();
        executable(&plain.join("rust_verify"));
        let text = l.root.join("text");
        executable(&text.join("rust_verify"));
        std::fs::write(text.join("z3"), "").unwrap();
        let dangling = l.root.join("dangling");
        executable(&dangling.join("rust_verify"));
        symlink(l.root.join("missing"), dangling.join("z3")).unwrap();
        for dir in [&ours, &plain, &text, &dangling] {
            let found = find(&cfg, Some(&dir.join("rust_verify")), &[&l.other], &l.me).unwrap();
            assert_eq!(found.found, Found::OnPath, "{}", dir.display());
        }

        // A parent that cannot be resolved falls through too.
        let found = find(&cfg, Some(&l.root.join("gone")), &[&l.bin], &l.me).unwrap();
        assert!(matches!(found.found, Found::NextToVerus(_)));

        // A verus on PATH without a solver next to it, and z3cache on PATH
        // as `z3`: the next z3 on PATH.
        std::fs::remove_file(l.release.join("z3")).unwrap();
        let shim = l.root.join("shim");
        std::fs::create_dir_all(&shim).unwrap();
        symlink(&l.me, shim.join("z3")).unwrap();
        let found = find(&cfg, Some(&parent), &[&l.bin, &shim, &l.other], &l.me).unwrap();
        assert_eq!(
            (found.path, found.found),
            (real(&l.other.join("z3")), Found::OnPath)
        );
        let _ = std::fs::remove_dir_all(&l.root);
    }

    /// Review finding 2: the setting is resolved once, to the one file that
    /// is then identified and run: a relative path (a bare name too) from
    /// the current directory, made absolute, with symlinks resolved.
    #[test]
    fn the_setting_is_resolved_to_one_canonical_file() {
        let l = layout("canonical");
        let link = l.root.join("link-to-z3");
        symlink(l.other.join("z3"), &link).unwrap();
        let found = find(&config(Some(link.to_str().unwrap())), None, &[], &l.me).unwrap();
        assert_eq!(found.path, real(&l.other.join("z3")));
        let cwd = std::env::current_dir().unwrap();
        let rel = pathdiff(&l.other.join("z3"), &cwd);
        let found = find(&config(Some(rel.to_str().unwrap())), None, &[], &l.me).unwrap();
        assert!(found.path.is_absolute(), "{}", found.path.display());
        assert_eq!(found.path, real(&l.other.join("z3")));
        // Every other rule gives a canonical path too: here a z3 on PATH
        // that is a symlink.
        let links = l.root.join("links");
        std::fs::create_dir_all(&links).unwrap();
        symlink(l.other.join("z3"), links.join("z3")).unwrap();
        let found = find(&config(None), None, &[&links], &l.me).unwrap();
        assert_eq!(
            (found.path, found.found),
            (real(&l.other.join("z3")), Found::OnPath)
        );
        let _ = std::fs::remove_dir_all(&l.root);
    }

    /// `to` relative to `from`, through `..` components.
    fn pathdiff(to: &Path, from: &Path) -> PathBuf {
        let (to, from) = (real(to), real(from));
        let common = to
            .components()
            .zip(from.components())
            .take_while(|(a, b)| a == b)
            .count();
        let mut rel = PathBuf::new();
        for _ in from.components().skip(common) {
            rel.push("..");
        }
        for c in to.components().skip(common) {
            rel.push(c);
        }
        rel
    }

    #[test]
    fn only_the_first_verus_on_path_counts() {
        let l = layout("first-verus");
        // A verus earlier on PATH without a z3: the release's z3 is not
        // used, because that is not the verus the user runs.
        let wrapper = l.root.join("wrapper");
        executable(&wrapper.join("verus"));
        let found = find(&config(None), None, &[&wrapper, &l.bin, &l.other], &l.me).unwrap();
        assert_eq!(found.found, Found::OnPath);
        assert_eq!(
            first_on_path(Some(&std::env::join_paths([&l.bin]).unwrap()), VERUS),
            Some(real(&l.release).join("verus"))
        );
        assert_eq!(first_on_path(None, VERUS), None);
        let _ = std::fs::remove_dir_all(&l.root);
    }

    #[test]
    fn the_parent_executable_is_known() {
        // This test process was started by cargo or a shell, whose
        // executable exists.
        let parent = parent_executable();
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let p = parent.expect("parent executable");
            assert!(p.is_absolute(), "{}", p.display());
            assert!(std::fs::metadata(&p).is_ok(), "{}", p.display());
        }
        // A process that does not exist has no executable.
        assert_eq!(executable_of(u32::MAX), None);
    }
}
