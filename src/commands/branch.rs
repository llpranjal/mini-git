//! `mgit branch` -- list, create, or delete branches.
//!
//! A branch is a file in `.git/refs/heads/` containing 40 hex characters and a
//! newline. That is the entire implementation, and it is worth stating plainly
//! because it explains every property branches have:
//!
//! * Creating one is O(1) and costs 41 bytes, no matter how large the history.
//! * "Which branch am I on" is a separate question, answered by HEAD, which is
//!   why HEAD can point at a branch that does not exist yet (an unborn branch)
//!   and why detaching HEAD is possible at all.
//! * Deleting a branch deletes no commits. It removes one pointer. The commits
//!   remain in the object store, unreachable but intact, until a garbage
//!   collector that this implementation does not have would remove them.
//! * A branch "diverging" is not an event. Two refs simply name two commits
//!   whose parent chains meet further back.

use crate::repo::{Head, Repository};
use anyhow::{bail, Result};

pub fn run(name: Option<&str>, start_point: Option<&str>, delete: Option<&str>) -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;

    if let Some(target) = delete {
        return delete_branch(&repo, target);
    }

    match name {
        Some(name) => create_branch(&repo, name, start_point),
        None => list_branches(&repo),
    }
}

fn list_branches(repo: &Repository) -> Result<()> {
    let branches = repo.list_branches()?;
    if branches.is_empty() {
        println!("no branches yet (the first commit will create one)");
        return Ok(());
    }

    let current = match repo.read_head()? {
        Head::Branch(name) => Some(name),
        Head::Detached(_) => None,
    };
    for (name, oid) in branches {
        let marker = if Some(&name) == current.as_ref() {
            '*'
        } else {
            ' '
        };
        println!("{marker} {name}\t{}", &oid.to_hex()[..7]);
    }
    Ok(())
}

fn create_branch(repo: &Repository, name: &str, start_point: Option<&str>) -> Result<()> {
    if repo.read_branch(name)?.is_some() {
        bail!("branch {name:?} already exists");
    }

    let target = match start_point {
        Some(rev) => repo.resolve_rev(rev)?,
        None => match repo.head_commit()? {
            Some(oid) => oid,
            None => bail!(
                "cannot create branch {name:?}: there are no commits yet, so there is \
                 nothing for it to point at"
            ),
        },
    };

    // Creating a branch does not switch to it, matching real Git. Splitting
    // "make a pointer" from "move HEAD" is what makes both operations obvious.
    repo.write_branch(name, target)?;
    println!("created branch {name} at {}", &target.to_hex()[..7]);
    Ok(())
}

fn delete_branch(repo: &Repository, name: &str) -> Result<()> {
    if repo.read_branch(name)?.is_none() {
        bail!("branch {name:?} does not exist");
    }
    if repo.read_head()? == Head::Branch(name.to_string()) {
        bail!("cannot delete branch {name:?}: it is the branch HEAD points at");
    }
    repo.delete_branch(name)?;
    // Deliberately no reachability check before deleting. Real Git refuses to
    // delete an unmerged branch without `-D` because the commits become
    // unreachable; here there is no garbage collector, so the commits stay in
    // the object store and remain recoverable by hash. Saying so is better than
    // implementing half of a safety net.
    println!("deleted branch {name} (its commits remain in the object store)");
    Ok(())
}
