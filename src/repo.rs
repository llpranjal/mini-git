//! Repository layout, reference storage, and working-tree walking.
//!
//! # What a repository actually is
//!
//! Almost nothing. A `.git` directory containing:
//!
//! ```text
//! .git/
//!   HEAD                  "ref: refs/heads/main\n"   <- where am I
//!   refs/heads/main       "<40 hex>\n"               <- what is the branch tip
//!   objects/ab/cdef...                               <- the immutable content
//!   mini-index                                       <- the staging area
//! ```
//!
//! The objects are the database. Refs are 41-byte text files naming a commit.
//! HEAD is a 1-line text file naming a ref. That is the whole model, and it is
//! why "create a branch" is instantaneous: it writes one small file.
//!
//! # Why the index file is called `mini-index`, not `index`
//!
//! Real Git's index is a binary format with a specific header, per-entry stat
//! data, extensions, and a trailing checksum. This project stores a simpler
//! text index (see `index.rs`). If that were written to `.git/index`, real Git
//! would find a file it cannot parse and every `git` command in the repository
//! would fail with "index file corrupt".
//!
//! Using a different filename means the two indexes coexist: `git log`,
//! `git cat-file`, and `git fsck` all work on a repository this tool created,
//! because the parts Git reads -- objects, refs, HEAD -- are the real format,
//! and the one part that is not real is somewhere Git never looks. That is what
//! makes the interoperability claim honest rather than partial.

use crate::index::Index;
use crate::object::{Commit, Object};
use crate::oid::Oid;
use crate::store::ObjectStore;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// The default branch name for a new repository.
pub const DEFAULT_BRANCH: &str = "main";

/// What HEAD currently points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// The normal case: HEAD contains `ref: refs/heads/<name>`. Committing
    /// moves that branch. The branch name may not exist as a file yet -- that
    /// is exactly the state of a fresh repository before its first commit,
    /// which is why this variant carries a name and not an Oid.
    Branch(String),
    /// HEAD contains a raw object id. Commits made here would be unreachable
    /// from any branch, so `commit` refuses; see `commands/commit.rs`.
    Detached(Oid),
}

#[derive(Debug)]
pub struct Repository {
    workdir: PathBuf,
    gitdir: PathBuf,
}

impl Repository {
    /// Create `.git` and its skeleton. Idempotent in the same sense as real
    /// `git init`: re-running on an existing repository leaves it alone.
    pub fn init(workdir: impl AsRef<Path>) -> Result<Repository> {
        let workdir = workdir.as_ref();
        fs::create_dir_all(workdir).with_context(|| format!("creating {}", workdir.display()))?;
        // Canonicalize so that later path arithmetic (making working-tree paths
        // relative to the root) compares like with like. On macOS this also
        // resolves /var -> /private/var.
        let workdir = fs::canonicalize(workdir)?;
        let gitdir = workdir.join(".git");

        fs::create_dir_all(gitdir.join("objects"))?;
        fs::create_dir_all(gitdir.join("refs").join("heads"))?;

        let head_path = gitdir.join("HEAD");
        if !head_path.exists() {
            // Note: no ref file is created for `main` yet. An unborn branch is
            // represented by HEAD naming a ref that does not exist, which is
            // how real Git does it too.
            fs::write(&head_path, format!("ref: refs/heads/{DEFAULT_BRANCH}\n"))?;
        }

        Ok(Repository { workdir, gitdir })
    }

    /// Walk up from `start` looking for a `.git` directory.
    ///
    /// This is why Git commands work from a subdirectory. The walk stops at the
    /// filesystem root rather than crossing into a parent repository's `.git`
    /// by accident, and the error names where it started so the failure is
    /// actionable.
    pub fn discover(start: impl AsRef<Path>) -> Result<Repository> {
        let start = fs::canonicalize(start.as_ref())
            .with_context(|| format!("resolving {}", start.as_ref().display()))?;
        let mut dir = start.as_path();
        loop {
            if dir.join(".git").is_dir() {
                return Ok(Repository {
                    workdir: dir.to_path_buf(),
                    gitdir: dir.join(".git"),
                });
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => bail!(
                    "not a mini-git repository (no .git directory found in {} or any parent)",
                    start.display()
                ),
            }
        }
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn gitdir(&self) -> &Path {
        &self.gitdir
    }

    pub fn store(&self) -> ObjectStore {
        ObjectStore::new(self.gitdir.join("objects"))
    }

    pub fn index_path(&self) -> PathBuf {
        self.gitdir.join("mini-index")
    }

    pub fn read_index(&self) -> Result<Index> {
        Index::load(&self.index_path())
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    pub fn head_path(&self) -> PathBuf {
        self.gitdir.join("HEAD")
    }

    pub fn read_head(&self) -> Result<Head> {
        let text = fs::read_to_string(self.head_path())
            .with_context(|| format!("reading {}", self.head_path().display()))?;
        let text = text.trim();
        match text.strip_prefix("ref: ") {
            Some(refname) => {
                let name = refname.strip_prefix("refs/heads/").with_context(|| {
                    format!("HEAD points at {refname:?}, which is not a branch")
                })?;
                validate_branch_name(name)?;
                Ok(Head::Branch(name.to_string()))
            }
            None => Ok(Head::Detached(Oid::from_hex(text)?)),
        }
    }

    pub fn set_head_to_branch(&self, branch: &str) -> Result<()> {
        validate_branch_name(branch)?;
        fs::write(self.head_path(), format!("ref: refs/heads/{branch}\n"))?;
        Ok(())
    }

    fn branch_path(&self, name: &str) -> Result<PathBuf> {
        validate_branch_name(name)?;
        Ok(self.gitdir.join("refs").join("heads").join(name))
    }

    /// The commit a branch points at, or `None` if the branch does not exist.
    pub fn read_branch(&self, name: &str) -> Result<Option<Oid>> {
        let path = self.branch_path(name)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Oid::from_hex(text.trim())?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn write_branch(&self, name: &str, oid: Oid) -> Result<()> {
        let path = self.branch_path(name)?;
        fs::create_dir_all(path.parent().unwrap())?;
        // Trailing newline matches real Git's ref files exactly. `git` tolerates
        // its absence, but matching means a diff of the two files is empty.
        fs::write(&path, format!("{oid}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn delete_branch(&self, name: &str) -> Result<()> {
        let path = self.branch_path(name)?;
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))
    }

    pub fn list_branches(&self) -> Result<Vec<(String, Oid)>> {
        let dir = self.gitdir.join("refs").join("heads");
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if !entry.path().is_file() {
                    // Nested branch names like `feature/x` create directories.
                    // This implementation does not support them; see README.
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(oid) = self.read_branch(&name)? {
                    out.push((name, oid));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// The commit HEAD resolves to, or `None` on an unborn branch.
    pub fn head_commit(&self) -> Result<Option<Oid>> {
        match self.read_head()? {
            Head::Branch(name) => self.read_branch(&name),
            Head::Detached(oid) => Ok(Some(oid)),
        }
    }

    /// Resolve a user-supplied revision: `HEAD`, a branch name, or a hash
    /// prefix. Deliberately does *not* implement `HEAD~3`, `@{upstream}`, or
    /// the rest of gitrevisions -- see README, "Not implemented".
    pub fn resolve_rev(&self, rev: &str) -> Result<Oid> {
        if rev == "HEAD" {
            return self
                .head_commit()?
                .context("HEAD does not point at a commit yet (no commits in this repository)");
        }
        if let Some(oid) = self.read_branch(rev)? {
            return Ok(oid);
        }
        self.store()
            .resolve_prefix(rev)
            .with_context(|| format!("cannot resolve {rev:?} as a branch or object id"))
    }

    pub fn read_commit(&self, oid: Oid) -> Result<Commit> {
        match self.store().read(oid)? {
            Object::Commit(c) => Ok(c),
            other => bail!("{oid} is a {}, not a commit", other.object_type().as_str()),
        }
    }

    /// Walk first-parent history from `start`, newest first, stopping after
    /// `limit` commits if one is given.
    ///
    /// First-parent only, which is exact here because this implementation never
    /// creates a merge. On a repository containing merges written by real Git
    /// this would show one side of the history; the README says so rather than
    /// implying a full traversal.
    ///
    /// The `limit` is applied *during* the walk rather than by the caller
    /// truncating the result afterwards. That distinction is the difference
    /// between `log -n 10` costing ten object reads and costing one per commit
    /// in the entire repository -- and the benchmark measures it, so the two are
    /// easy to tell apart.
    pub fn history(&self, start: Oid, limit: Option<usize>) -> Result<Vec<(Oid, Commit)>> {
        let mut out = Vec::new();
        let mut current = Some(start);
        while let Some(oid) = current {
            if limit.is_some_and(|n| out.len() >= n) {
                break;
            }
            let commit = self.read_commit(oid)?;
            current = commit.parents.first().copied();
            out.push((oid, commit));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Working tree
    // -----------------------------------------------------------------------

    /// Every file in the working tree, as paths relative to the root, sorted.
    ///
    /// `.git` is skipped. `.gitignore` is *not* honoured -- this implementation
    /// has no ignore rules at all, which is a real limitation and is documented
    /// as one. Symlinks are skipped rather than followed, because following
    /// them can loop and storing them needs mode 120000, which the object layer
    /// deliberately does not support.
    pub fn walk_workdir(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        collect_files(&self.workdir, &self.workdir, &mut out)?;
        out.sort();
        Ok(out)
    }

    /// Absolute path for a repository-relative path.
    pub fn full_path(&self, rel: &str) -> PathBuf {
        self.workdir.join(rel)
    }

    /// Convert a user-supplied path (possibly absolute, possibly relative to
    /// the current directory) into a repository-relative path with forward
    /// slashes.
    pub fn relative_path(&self, input: &Path) -> Result<String> {
        let absolute = if input.is_absolute() {
            input.to_path_buf()
        } else {
            std::env::current_dir()?.join(input)
        };
        // `canonicalize` requires existence, which is wrong for a path being
        // removed, so normalize `.` and `..` manually instead.
        let normalized = normalize(&absolute);
        let workdir = normalize(&self.workdir);
        let rel = normalized.strip_prefix(&workdir).with_context(|| {
            format!(
                "{} is outside the repository at {}",
                input.display(),
                self.workdir.display()
            )
        })?;
        Ok(rel.to_string_lossy().replace('\\', "/"))
    }
}

/// Resolve `.` and `..` lexically, without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Keep every branch operation inside `.git/refs/heads`.
///
/// Hierarchical names are deliberately unsupported, so a valid name is one
/// ordinary path component plus Git's most important filename restrictions.
fn validate_branch_name(name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        bail!("branch name {name:?} must be a single path component");
    }
    if name.starts_with('.') || name.starts_with('-') || name == "HEAD" {
        bail!("branch name {name:?} is not allowed");
    }
    if name.contains("..")
        || name.contains(|c: char| c.is_whitespace() || c.is_control() || c == '\\')
    {
        bail!("branch name {name:?} contains invalid characters");
    }
    Ok(())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        // `file_type` does not follow symlinks, so a symlink to a directory is
        // reported as a symlink and skipped rather than recursed into.
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path.strip_prefix(root).unwrap();
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// True if the file has any execute bit set. Git records exactly one bit of
/// permission information, so this collapses the three POSIX execute bits into
/// the single question the tree format can answer.
#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    #[test]
    fn init_creates_the_skeleton_with_an_unborn_branch() {
        let dir = ScratchDir::new("repo-init");
        let repo = Repository::init(dir.path()).unwrap();

        assert!(repo.gitdir().join("objects").is_dir());
        assert!(repo.gitdir().join("refs").join("heads").is_dir());
        assert_eq!(
            fs::read_to_string(repo.head_path()).unwrap(),
            "ref: refs/heads/main\n"
        );
        // The branch ref must NOT exist yet -- an unborn branch is the absence
        // of the file, not a file containing zeros.
        assert_eq!(repo.read_branch("main").unwrap(), None);
        assert_eq!(repo.head_commit().unwrap(), None);
    }

    #[test]
    fn init_is_idempotent_and_preserves_head() {
        let dir = ScratchDir::new("repo-reinit");
        let repo = Repository::init(dir.path()).unwrap();
        repo.set_head_to_branch("other").unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        assert_eq!(repo.read_head().unwrap(), Head::Branch("other".into()));
    }

    #[test]
    fn discover_walks_up_from_a_subdirectory() {
        let dir = ScratchDir::new("repo-discover");
        Repository::init(dir.path()).unwrap();
        fs::create_dir_all(dir.path().join("a/b/c")).unwrap();

        let found = Repository::discover(dir.path().join("a/b/c")).unwrap();
        assert_eq!(found.workdir(), dir.path());
    }

    #[test]
    fn discover_fails_outside_a_repository() {
        let dir = ScratchDir::new("repo-none");
        let err = Repository::discover(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("not a mini-git repository"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn refs_are_plain_text_files_with_a_trailing_newline() {
        let dir = ScratchDir::new("repo-refs");
        let repo = Repository::init(dir.path()).unwrap();
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        repo.write_branch("main", oid).unwrap();

        let raw = fs::read_to_string(repo.gitdir().join("refs/heads/main")).unwrap();
        assert_eq!(raw, format!("{oid}\n"));
        assert_eq!(repo.read_branch("main").unwrap(), Some(oid));
        assert_eq!(repo.head_commit().unwrap(), Some(oid));
    }

    #[test]
    fn detached_head_is_a_bare_object_id() {
        let dir = ScratchDir::new("repo-detached");
        let repo = Repository::init(dir.path()).unwrap();
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        fs::write(repo.head_path(), format!("{oid}\n")).unwrap();
        assert_eq!(repo.read_head().unwrap(), Head::Detached(oid));
    }

    #[test]
    fn walk_skips_the_git_directory_and_returns_sorted_relative_paths() {
        let dir = ScratchDir::new("repo-walk");
        let repo = Repository::init(dir.path()).unwrap();
        dir.write("z.txt", "z");
        dir.write("a.txt", "a");
        dir.write("src/main.rs", "fn main() {}");

        let files = repo.walk_workdir().unwrap();
        assert_eq!(files, vec!["a.txt", "src/main.rs", "z.txt"]);
    }

    #[test]
    fn relative_path_normalizes_and_rejects_paths_outside_the_repo() {
        let dir = ScratchDir::new("repo-relpath");
        let repo = Repository::init(dir.path()).unwrap();
        dir.write("src/main.rs", "x");

        assert_eq!(
            repo.relative_path(&dir.path().join("src/./main.rs"))
                .unwrap(),
            "src/main.rs"
        );
        assert_eq!(
            repo.relative_path(&dir.path().join("src/../src/main.rs"))
                .unwrap(),
            "src/main.rs"
        );
        assert!(repo.relative_path(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn list_branches_is_sorted() {
        let dir = ScratchDir::new("repo-branches");
        let repo = Repository::init(dir.path()).unwrap();
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        for name in ["zeta", "alpha", "main"] {
            repo.write_branch(name, oid).unwrap();
        }
        let names: Vec<String> = repo
            .list_branches()
            .unwrap()
            .into_iter()
            .map(|b| b.0)
            .collect();
        assert_eq!(names, vec!["alpha", "main", "zeta"]);
    }

    #[test]
    fn branch_names_are_validated_once_at_the_repository_boundary() {
        for name in ["main", "fix-123", "release_1.0"] {
            assert!(validate_branch_name(name).is_ok(), "{name:?}");
        }
        for name in [
            "",
            "feature/x",
            r"feature\x",
            ".hidden",
            "-flag",
            "HEAD",
            "a..b",
            "two words",
        ] {
            assert!(validate_branch_name(name).is_err(), "{name:?}");
        }

        let dir = ScratchDir::new("repo-branch-traversal");
        let repo = Repository::init(dir.path()).unwrap();
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        assert!(repo.write_branch("../outside", oid).is_err());
        assert!(repo.read_branch("../outside").is_err());
        assert!(!repo.gitdir().join("refs/outside").exists());
    }
}
