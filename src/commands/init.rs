//! `mgit init` -- create an empty repository.

use crate::repo::{Repository, DEFAULT_BRANCH};
use anyhow::Result;
use std::path::Path;

pub fn run(path: &Path) -> Result<()> {
    let existed = path.join(".git").is_dir();
    let repo = Repository::init(path)?;
    let verb = if existed {
        "Reinitialized existing"
    } else {
        "Initialized empty"
    };
    println!(
        "{verb} mini-git repository in {} (on branch {DEFAULT_BRANCH})",
        repo.gitdir().display()
    );
    Ok(())
}
