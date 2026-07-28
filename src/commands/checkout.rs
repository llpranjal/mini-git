//! `mgit checkout` -- restore files from a commit, or move to a branch.
//!
//! ```text
//!   mgit checkout <rev> -- <path>...   restore those paths from that commit
//!   mgit checkout <branch>             switch HEAD to that branch
//! ```
//!
//! # Why both forms live in one command
//!
//! They are the same operation at different scopes: take content out of a
//! commit's tree and put it into the index and the working tree. Real Git bundles
//! them the same way, which is precisely why it is confusing enough that
//! `git switch` and `git restore` were later added to split them apart. Keeping
//! them together here is deliberate --- it is a small enough surface to be
//! readable, and it makes the shared machinery obvious rather than duplicated.
//!
//! # The safety rule
//!
//! Restoring a path overwrites whatever is on disk at that path. That is the
//! point, and it is what the user asked for. Switching branches is different:
//! it touches files the user did not name, so it refuses to run when there are
//! uncommitted changes. Real Git is cleverer here --- it carries changes across
//! when they do not conflict --- and reproducing that means a three-way merge,
//! which this project deliberately does not implement. Refusing is the honest
//! version of "I cannot do this safely".

use crate::index::IndexEntry;
use crate::object::Object;
use crate::repo::Repository;
use crate::trees;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub fn run(rev: &str, paths: &[PathBuf]) -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;
    if paths.is_empty() {
        switch_branch(&repo, rev)
    } else {
        restore_paths(&repo, rev, paths)
    }
}

/// `mgit checkout <rev> -- <path>...`
fn restore_paths(repo: &Repository, rev: &str, paths: &[PathBuf]) -> Result<()> {
    let store = repo.store();
    let commit_oid = repo.resolve_rev(rev)?;
    let tree = trees::commit_tree_map(&store, repo, Some(commit_oid))?;
    let mut index = repo.read_index()?;

    let mut restored = 0usize;
    for path in paths {
        let rel = repo.relative_path(path)?;

        // Accept a directory prefix as well as an exact path, so restoring a
        // whole subdirectory out of an old commit works. An exact match wins,
        // because a file and a directory prefix can otherwise both apply.
        let matches: Vec<&String> = if tree.contains_key(&rel) {
            vec![tree.get_key_value(&rel).unwrap().0]
        } else {
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            tree.keys().filter(|k| k.starts_with(&prefix)).collect()
        };

        if matches.is_empty() {
            bail!("pathspec {rel:?} did not match any file in {rev}");
        }

        for target in matches {
            let (mode, oid) = tree[target];
            let bytes = match store.read(oid)? {
                Object::Blob(b) => b,
                other => bail!("{oid} is a {}, not a blob", other.object_type().as_str()),
            };

            let abs = repo.full_path(target);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, &bytes).with_context(|| format!("writing {}", abs.display()))?;
            set_mode(&abs, mode)?;

            // The index is updated too. If it were not, the restored file would
            // immediately show as "modified" against the still-stale index,
            // which is the opposite of what "restore this file" should mean.
            index.insert(target.clone(), IndexEntry { mode, oid });
            restored += 1;
        }
    }

    index.save(&repo.index_path())?;
    println!(
        "restored {restored} file(s) from {}",
        &commit_oid.to_hex()[..7]
    );
    Ok(())
}

/// `mgit checkout <branch>`
fn switch_branch(repo: &Repository, branch: &str) -> Result<()> {
    let store = repo.store();

    let target = repo
        .read_branch(branch)?
        .with_context(|| format!("branch {branch:?} does not exist"))?;

    let index = repo.read_index()?;
    let status = super::status::compute(repo, &store, &index)?;
    if status.has_local_changes() {
        bail!(
            "you have local changes that would be overwritten by switching to {branch:?}; \
             commit them first (this implementation does not merge)"
        );
    }

    let current = trees::commit_tree_map(&store, repo, repo.head_commit()?)?;
    let wanted = trees::commit_tree_map(&store, repo, Some(target))?;

    // Delete first, then write. A path that is a file on this branch and a
    // directory on the other cannot be created until the file is gone.
    for path in current.keys() {
        if !wanted.contains_key(path) {
            let abs = repo.full_path(path);
            let _ = std::fs::remove_file(&abs);
            prune_empty_dirs(repo, &abs);
        }
    }

    let mut new_index = crate::index::Index::new();
    for (path, (mode, oid)) in &wanted {
        let bytes = match store.read(*oid)? {
            Object::Blob(b) => b,
            other => bail!("{oid} is a {}, not a blob", other.object_type().as_str()),
        };
        let abs = repo.full_path(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, &bytes)?;
        set_mode(&abs, *mode)?;
        new_index.insert(
            path.clone(),
            IndexEntry {
                mode: *mode,
                oid: *oid,
            },
        );
    }

    new_index.save(&repo.index_path())?;
    repo.set_head_to_branch(branch)?;
    println!("switched to branch {branch}");
    Ok(())
}

/// Remove directories left empty after deleting a file, stopping at the
/// repository root. Git does not track directories -- only files -- so an empty
/// directory left behind after a branch switch is an artifact of the checkout,
/// not content the other branch asked for.
fn prune_empty_dirs(repo: &Repository, from: &std::path::Path) {
    let mut dir = from.parent();
    while let Some(d) = dir {
        if d == repo.workdir() || std::fs::remove_dir(d).is_err() {
            // `remove_dir` fails on a non-empty directory, which is the stop
            // condition -- everything above it is non-empty too.
            break;
        }
        dir = d.parent();
    }
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: crate::object::FileMode) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // 0o755 / 0o644 rather than preserving whatever was there: Git records
    // exactly one bit, so restoring the file has exactly one bit to restore.
    let bits = if mode.is_dir() || mode == crate::object::FileMode::Executable {
        0o755
    } else {
        0o644
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: crate::object::FileMode) -> Result<()> {
    Ok(())
}
