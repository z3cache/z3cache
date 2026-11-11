//! Tier 6: the Verus proofs in the source (`src/rlimit/counter.rs`).
//!
//! Skipped, with a message, unless `Z3CACHE_VERUS` points at a `verus`
//! binary. Verus uses the Z3 next to it, never a proxy that may be set in
//! the environment. The run is bounded: one that does not finish in time is
//! killed with its children and fails the test.

mod common;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Longest the proof check may take. It takes a few seconds.
const LIMIT: Duration = Duration::from_secs(120);

/// Files checked by Verus, each as its own library crate.
const PROOFS: &[&str] = &["src/rlimit/counter.rs"];

#[test]
fn verus_proofs_hold() {
    let Some(verus) = std::env::var_os("Z3CACHE_VERUS").filter(|v| !v.is_empty()) else {
        eprintln!("SKIPPED: set Z3CACHE_VERUS to a verus binary to check the proofs");
        return;
    };
    let verus = PathBuf::from(verus);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in PROOFS {
        let out_dir = common::temp_dir("proofs");
        let mut cmd = Command::new(&verus);
        cmd.arg("--crate-type=lib")
            .arg(root.join(file))
            // Anything Verus writes lands outside the source tree.
            .current_dir(&out_dir)
            .env("VERUS_Z3_PATH", verus.with_file_name("z3"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = cmd.spawn().expect("start verus");
        let pid = child.id() as libc::pid_t;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        let out = match rx.recv_timeout(LIMIT) {
            Ok(out) => out.expect("run verus"),
            Err(_) => {
                // SAFETY: signals the process group of the child started above.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                panic!("verus did not finish {file} within {LIMIT:?}");
            }
        };
        let _ = std::fs::remove_dir_all(&out_dir);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success() && text.contains(", 0 errors"),
            "verus rejected {file}:\n{text}"
        );
    }
}
