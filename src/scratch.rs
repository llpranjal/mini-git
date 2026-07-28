//! A throwaway directory that deletes itself when dropped.
//!
//! This exists so the project has **zero dependencies used only for testing**.
//! `tempfile` would be the obvious crate, but the whole tool needs only four
//! dependencies, and "a unique directory that cleans itself up" is about twenty
//! lines. Shipping those twenty lines is a better trade than a dependency whose
//! transitive tree (getrandom, cfg-if, rustix, ...) is larger than this program.
//!
//! It is compiled into the library rather than a `tests/common` module because
//! the integration tests, the unit tests, and the benchmark all need it, and a
//! `tests/` helper is not visible to the other two.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter so two scratch dirs created in the same process, in the
/// same millisecond, cannot collide. Process id separates concurrent test
/// binaries; the counter separates tests within one binary, which matters
/// because `cargo test` runs tests on multiple threads by default.
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn new(label: &str) -> ScratchDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "mini-git-{label}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("creating scratch directory");
        // Canonicalize immediately: on macOS `std::env::temp_dir()` is
        // `/var/folders/...`, and `/var` is a symlink to `/private/var`. Paths
        // that a subprocess reports (real `git`, or our own repository
        // discovery walking up from a canonical cwd) come back with the
        // `/private` prefix. Without this, string comparisons between "the
        // directory we made" and "the directory git says it is in" fail for
        // reasons that have nothing to do with version control. See JOURNAL.md.
        let path = std::fs::canonicalize(&path).expect("canonicalizing scratch directory");
        ScratchDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write a file, creating parent directories. Convenience for tests, which
    /// otherwise spend most of their lines on `create_dir_all`.
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creating parent directory");
        }
        std::fs::write(&full, contents).expect("writing scratch file");
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).expect("reading scratch file")
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best effort. A leaked directory in the OS temp dir is a nuisance; a
        // panic inside `drop` during an already-failing test would mask the
        // real assertion failure, which is much worse.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
