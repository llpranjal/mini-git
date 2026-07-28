//! `mgit status` -- the three-way comparison.
//!
//! Status is where the reason for having an index becomes concrete. There are
//! three versions of the project in play at once:
//!
//! ```text
//!   HEAD's tree  <--(1)-->  the index  <--(2)-->  the working tree
//! ```
//!
//! Comparison (1) is "changes to be committed": what the next commit would
//! change relative to the last one. Comparison (2) is "changes not staged":
//! edits made since the last `add`. A file can appear in both at once --- staged
//! one version, then kept editing --- and any tool with only two states could
//! not express that.
//!
//! Both comparisons are equality checks on object ids, never on content. Two
//! files are the same file if and only if their blobs hash the same, which
//! makes each comparison a 20-byte memcmp regardless of file size.

use crate::index::Index;
use crate::object::{FileMode, Object};
use crate::repo::{self, Head, Repository};
use crate::store::ObjectStore;
use crate::trees;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// index vs HEAD
    pub staged_new: Vec<String>,
    pub staged_modified: Vec<String>,
    pub staged_deleted: Vec<String>,
    /// working tree vs index
    pub unstaged_modified: Vec<String>,
    pub unstaged_deleted: Vec<String>,
    /// in the working tree, in neither the index nor HEAD
    pub untracked: Vec<String>,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.staged_new.is_empty()
            && self.staged_modified.is_empty()
            && self.staged_deleted.is_empty()
            && self.unstaged_modified.is_empty()
            && self.unstaged_deleted.is_empty()
            && self.untracked.is_empty()
    }

    /// True if there is anything that a branch switch could silently destroy.
    /// Untracked files are excluded: switching branches does not touch them.
    pub fn has_local_changes(&self) -> bool {
        !self.staged_new.is_empty()
            || !self.staged_modified.is_empty()
            || !self.staged_deleted.is_empty()
            || !self.unstaged_modified.is_empty()
            || !self.unstaged_deleted.is_empty()
    }
}

/// Compute status without printing anything, so `checkout` can reuse it to
/// decide whether switching branches is safe.
pub fn compute(repo: &Repository, store: &ObjectStore, index: &Index) -> Result<Status> {
    let head_map = trees::commit_tree_map(store, repo, repo.head_commit()?)?;
    let mut status = Status::default();

    // (1) index vs HEAD.
    let paths: BTreeSet<&String> = index.paths().chain(head_map.keys()).collect();
    for path in paths {
        match (head_map.get(path), index.get(path)) {
            (None, Some(_)) => status.staged_new.push(path.clone()),
            (Some(_), None) => status.staged_deleted.push(path.clone()),
            (Some((head_mode, head_oid)), Some(entry)) => {
                // Mode is part of the comparison: chmod +x on a committed file
                // is a real change to the tree even though the blob is
                // identical, and missing it would produce a commit that claims
                // nothing changed while the tree id moves.
                if *head_oid != entry.oid || *head_mode != entry.mode {
                    status.staged_modified.push(path.clone());
                }
            }
            (None, None) => unreachable!("path came from one of the two maps"),
        }
    }

    // (2) working tree vs index. Every tracked file is re-read and re-hashed,
    // because this index stores no stat cache -- see the trade-off documented
    // in `index.rs`.
    let workdir: BTreeSet<String> = repo.walk_workdir()?.into_iter().collect();
    for (path, entry) in index.iter() {
        if !workdir.contains(path) {
            status.unstaged_deleted.push(path.clone());
            continue;
        }
        let abs = repo.full_path(path);
        let content = std::fs::read(&abs)?;
        let oid = Object::Blob(content).oid();
        let mode = if repo::is_executable(&abs) {
            FileMode::Executable
        } else {
            FileMode::Regular
        };
        if oid != entry.oid || mode != entry.mode {
            status.unstaged_modified.push(path.clone());
        }
    }

    // (3) untracked. A file already committed but not in the index is a staged
    // deletion, not an untracked file, so HEAD has to be consulted here too.
    for path in &workdir {
        if !index.contains(path) && !head_map.contains_key(path) {
            status.untracked.push(path.clone());
        }
    }

    Ok(status)
}

pub fn run() -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;
    let store = repo.store();
    let index = repo.read_index()?;
    let status = compute(&repo, &store, &index)?;

    match repo.read_head()? {
        Head::Branch(name) => {
            println!("On branch {name}");
            if repo.read_branch(&name)?.is_none() {
                println!();
                println!("No commits yet");
            }
        }
        Head::Detached(oid) => println!("HEAD detached at {}", &oid.to_hex()[..7]),
    }

    let section = |title: &str, hint: &str, groups: &[(&str, &Vec<String>)]| {
        if groups.iter().all(|(_, v)| v.is_empty()) {
            return;
        }
        println!();
        println!("{title}");
        println!("  ({hint})");
        for (label, paths) in groups {
            for path in *paths {
                // Untracked files pass an empty label; padding it would indent
                // them by twelve spaces for no reason.
                if label.is_empty() {
                    println!("\t{path}");
                } else {
                    println!("\t{label:<12}{path}");
                }
            }
        }
    };

    section(
        "Changes to be committed:",
        "these will go into the next commit",
        &[
            ("new file:", &status.staged_new),
            ("modified:", &status.staged_modified),
            ("deleted:", &status.staged_deleted),
        ],
    );
    section(
        "Changes not staged for commit:",
        "use \"mgit add <file>\" to include in the next commit",
        &[
            ("modified:", &status.unstaged_modified),
            ("deleted:", &status.unstaged_deleted),
        ],
    );
    section(
        "Untracked files:",
        "use \"mgit add <file>\" to start tracking",
        &[("", &status.untracked)],
    );

    if status.is_clean() {
        println!();
        println!("nothing to commit, working tree clean");
    }
    Ok(())
}

/// Snapshot of the working tree as `path -> (mode, blob id)`, hashing every
/// file but writing nothing. Used by `diff` so it can compare the working tree
/// against the index without polluting the object store with blobs for content
/// the user never staged.
pub fn workdir_map(repo: &Repository) -> Result<BTreeMap<String, (FileMode, crate::Oid)>> {
    let mut out = BTreeMap::new();
    for path in repo.walk_workdir()? {
        let abs = repo.full_path(&path);
        let content = std::fs::read(&abs)?;
        let mode = if repo::is_executable(&abs) {
            FileMode::Executable
        } else {
            FileMode::Regular
        };
        out.insert(path, (mode, Object::Blob(content).oid()));
    }
    Ok(out)
}
