//! `mgit diff` -- show line-level changes between any two repository states.
//!
//! ```text
//!   mgit diff            working tree vs index
//!   mgit diff --staged   index vs HEAD
//!   mgit diff <rev>      working tree vs a commit
//! ```
//!
//! Every mode becomes two snapshots. Comparing object IDs filters out unchanged
//! paths before their contents are loaded and passed to the line differ.

use crate::diff::unified;
use crate::object::{FileMode, Object};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::store::ObjectStore;
use crate::trees;
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};

type FileMap = BTreeMap<String, (FileMode, Oid)>;

/// One side of a diff: its files, display name, and where its bytes live.
struct Snapshot {
    files: FileMap,
    label: String,
    from_workdir: bool,
}

impl Snapshot {
    fn stored(label: impl Into<String>, files: FileMap) -> Self {
        Self {
            files,
            label: label.into(),
            from_workdir: false,
        }
    }

    fn index(index: &crate::index::Index) -> Self {
        let files = index
            .iter()
            .map(|(path, entry)| (path.clone(), (entry.mode, entry.oid)))
            .collect();
        Self::stored("index", files)
    }

    fn workdir(repo: &Repository) -> Result<Self> {
        Ok(Self {
            files: crate::commands::status::workdir_map(repo)?,
            label: "working tree".into(),
            from_workdir: true,
        })
    }

    /// Working-tree bytes have not been staged, so `diff` reads them directly
    /// rather than polluting the object store.
    fn read_blob(
        &self,
        repo: &Repository,
        store: &ObjectStore,
        path: &str,
        oid: Oid,
    ) -> Result<Vec<u8>> {
        if self.from_workdir {
            return Ok(std::fs::read(repo.full_path(path))?);
        }
        match store.read(oid)? {
            Object::Blob(bytes) => Ok(bytes),
            other => bail!("{oid} is a {}, not a blob", other.object_type().as_str()),
        }
    }
}

pub fn run(staged: bool, rev: Option<&str>) -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;
    let store = repo.store();
    let index = repo.read_index()?;

    if staged && rev.is_some() {
        bail!("--staged compares the index with HEAD; it cannot also take a revision");
    }

    let (old, new) = if staged {
        let head = trees::commit_tree_map(&store, &repo, repo.head_commit()?)?;
        (Snapshot::stored("HEAD", head), Snapshot::index(&index))
    } else if let Some(rev) = rev {
        let oid = repo.resolve_rev(rev)?;
        let tree = trees::commit_tree_map(&store, &repo, Some(oid))?;
        (Snapshot::stored(rev, tree), Snapshot::workdir(&repo)?)
    } else {
        (Snapshot::index(&index), Snapshot::workdir(&repo)?)
    };

    let paths: BTreeSet<&String> = old.files.keys().chain(new.files.keys()).collect();
    let mut printed_anything = false;

    for path in paths {
        let old_file = old.files.get(path);
        let new_file = new.files.get(path);

        if let (Some((old_mode, old_oid)), Some((new_mode, new_oid))) = (old_file, new_file) {
            if old_oid == new_oid && old_mode == new_mode {
                continue;
            }
        }

        let old_bytes = match old_file {
            Some((_, oid)) => old.read_blob(&repo, &store, path, *oid)?,
            None => Vec::new(),
        };
        let new_bytes = match new_file {
            Some((_, oid)) => new.read_blob(&repo, &store, path, *oid)?,
            None => Vec::new(),
        };

        let old_label = if old_file.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".into()
        };
        let new_label = if new_file.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".into()
        };

        let body = unified(&old_bytes, &new_bytes, &old_label, &new_label);
        println!("diff --mgit a/{path} b/{path}");
        if body.is_empty() {
            if let (Some((old_mode, _)), Some((new_mode, _))) = (old_file, new_file) {
                println!("old mode {}", old_mode.as_str());
                println!("new mode {}", new_mode.as_str());
            }
        } else {
            print!("{body}");
        }
        printed_anything = true;
    }

    if !printed_anything {
        println!("no differences between {} and {}", old.label, new.label);
    }
    Ok(())
}
