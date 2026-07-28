//! Shared helpers for the integration tests.
//!
// Each integration test binary compiles this module separately and uses only
// the helpers it needs, so anything used by `interop.rs` but not `e2e.rs` (or
// the reverse) looks dead to that binary. The allow is about how Cargo builds
// `tests/`, not about unused code.
#![allow(dead_code)]
//!
//! These tests drive the compiled `mgit` binary rather than calling the library
//! directly. That is deliberate: argument parsing, output formatting, exit
//! codes, and repository discovery from the process's current directory are all
//! part of the behaviour, and a test that bypasses `main` cannot catch a
//! regression in any of them.

use std::path::Path;
use std::process::{Command, Output};

/// A fixed identity and timestamp for every command in the test suite.
///
/// Both `mgit` and real `git` read these variables, which is the mechanism that
/// makes the byte-identical-commit interop test possible: with the same author,
/// committer, and instant, two correct implementations must produce the same
/// commit object and therefore the same SHA-1. Anything that differs is a real
/// difference in the format, not noise.
pub const AUTHOR_NAME: &str = "Test Author";
pub const AUTHOR_EMAIL: &str = "test@example.com";
pub const FIXED_DATE: &str = "@1700000000 +0000";

fn with_env(cmd: &mut Command, dir: &Path) {
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_AUTHOR_DATE", FIXED_DATE)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_DATE", FIXED_DATE);
}

/// Run `mgit` in `dir`. `CARGO_BIN_EXE_mgit` is set by Cargo to the path of the
/// binary built for this test run, so there is no chance of testing a stale
/// build or something else on `$PATH`.
pub fn mgit_raw(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mgit"));
    with_env(&mut cmd, dir);
    cmd.args(args).output().expect("running mgit")
}

/// Run `mgit` and require success, returning stdout.
pub fn mgit(dir: &Path, args: &[&str]) -> String {
    let out = mgit_raw(dir, args);
    assert!(
        out.status.success(),
        "mgit {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Run `mgit` and require failure, returning stderr.
pub fn mgit_err(dir: &Path, args: &[&str]) -> String {
    let out = mgit_raw(dir, args);
    assert!(
        !out.status.success(),
        "mgit {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Run real `git` in `dir`.
///
/// Config is forced on the command line rather than trusting the machine's
/// global config: a developer with `commit.gpgsign = true` or a non-default
/// `core.autocrlf` would otherwise see these tests fail for reasons unrelated
/// to the code.
pub fn git_raw(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    with_env(&mut cmd, dir);
    cmd.args([
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "gc.auto=0",
    ]);
    cmd.args(args).output().expect("running git")
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_raw(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Raw stdout bytes from `git`, for comparing binary content exactly.
pub fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = git_raw(dir, args);
    assert!(out.status.success(), "git {args:?} failed");
    out.stdout
}

pub fn mgit_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = mgit_raw(dir, args);
    assert!(
        out.status.success(),
        "mgit {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Fail the test run if real `git` is unavailable.
///
/// The interoperability claim is the point of this project, so a missing `git`
/// must not silently turn those tests into no-ops. `MINI_GIT_ALLOW_NO_GIT=1`
/// exists as an explicit escape hatch for a machine without git, and the test
/// says loudly when it takes it.
pub fn require_git() -> bool {
    let available = Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if available {
        return true;
    }
    if std::env::var("MINI_GIT_ALLOW_NO_GIT").as_deref() == Ok("1") {
        eprintln!("SKIPPING interop test: git not found and MINI_GIT_ALLOW_NO_GIT=1 is set");
        return false;
    }
    panic!(
        "real `git` is required for the interoperability tests, which are the point of this \
         project. Install git, or set MINI_GIT_ALLOW_NO_GIT=1 to skip them explicitly."
    );
}
