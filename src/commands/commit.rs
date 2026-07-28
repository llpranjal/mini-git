//! `mgit commit` -- turn the index into a permanent snapshot.
//!
//! The whole operation is four steps, and they are worth reading as a unit
//! because this is the core of the system:
//!
//! 1. Build tree objects from the index (`trees::build_tree_from_index`).
//! 2. Read HEAD to find the parent commit, if any.
//! 3. Write a commit object pointing at the new root tree and that parent.
//! 4. Move the current branch ref to the new commit.
//!
//! Only step 4 mutates anything that already existed. Steps 1 and 3 add new,
//! immutable objects; if the process dies before step 4, the repository is
//! exactly as it was, with some unreferenced objects lying around. Making the
//! single-file ref write the last and only destructive act is what gives commit
//! its effectively atomic feel.

use crate::commands::signature;
use crate::object::{Commit, Object};
use crate::repo::{Head, Repository};
use crate::trees;
use anyhow::{bail, Result};

pub fn run(message: &str) -> Result<()> {
    if message.trim().is_empty() {
        bail!("aborting commit due to empty commit message");
    }

    let repo = Repository::discover(std::env::current_dir()?)?;
    let store = repo.store();
    let index = repo.read_index()?;

    let branch = match repo.read_head()? {
        Head::Branch(name) => name,
        Head::Detached(oid) => bail!(
            "HEAD is detached at {oid}; committing here would create a commit no branch \
             points to. Check out a branch first."
        ),
    };
    let parent = repo.read_branch(&branch)?;

    // Refuse an empty commit. Comparing tree ids is the cheapest possible check
    // and needs no file comparison at all: if the index produces the same root
    // tree the parent commit already points at, nothing changed. That is
    // content addressing doing the work -- one 20-byte comparison stands in for
    // "are these two entire directory trees identical".
    let tree_oid = trees::build_tree_from_index(&store, &index)?;
    if let Some(parent_oid) = parent {
        if repo.read_commit(parent_oid)?.tree == tree_oid {
            bail!("nothing to commit, working tree matches HEAD (index is unchanged)");
        }
    }

    let commit = Commit {
        tree: tree_oid,
        parents: parent.into_iter().collect(),
        author: signature("AUTHOR"),
        committer: signature("COMMITTER"),
        message: message.to_string(),
    };
    let commit_oid = store.write(&Object::Commit(commit.clone()))?;

    repo.write_branch(&branch, commit_oid)?;

    let root_marker = if parent.is_none() {
        " (root-commit)"
    } else {
        ""
    };
    println!(
        "[{branch}{root_marker} {}] {}",
        &commit_oid.to_hex()[..7],
        commit.summary()
    );
    println!("{} file(s) in this snapshot", index.len());
    Ok(())
}
