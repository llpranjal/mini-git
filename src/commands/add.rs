//! `mgit add` -- stage paths into the index.

use crate::commands::stage_file;
use crate::repo::Repository;
use anyhow::{bail, Result};
use std::path::PathBuf;

pub fn run(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        bail!("nothing specified, nothing added (give at least one path, or `.`)");
    }

    let repo = Repository::discover(std::env::current_dir()?)?;
    let store = repo.store();
    let mut index = repo.read_index()?;

    let mut staged = 0usize;
    let mut removed = 0usize;

    for path in paths {
        let rel = repo.relative_path(path)?;
        let abs = repo.full_path(&rel);

        if abs.is_dir() || rel.is_empty() {
            // A directory (including `.`, which resolves to the empty relative
            // path) stages everything under it. Rather than walking the
            // directory again here, filter the repository-wide walk: that walk
            // already knows to skip `.git`, and duplicating the skip logic is
            // how the two would drift apart.
            let prefix = if rel.is_empty() {
                String::new()
            } else {
                format!("{rel}/")
            };
            for file in repo.walk_workdir()? {
                if file.starts_with(&prefix) {
                    stage_file(&store, &mut index, &repo.full_path(&file), &file)?;
                    staged += 1;
                }
            }
        } else if abs.is_file() {
            stage_file(&store, &mut index, &abs, &rel)?;
            staged += 1;
        } else if index.contains(&rel) {
            // The file is tracked but gone from disk. Staging a deletion is the
            // only sensible reading of `add <deleted path>`, and it is what real
            // Git's `add` does too (it is `add --all`'s behaviour, and plain
            // `add` on a named missing path errors -- this tool takes the more
            // useful branch and says so).
            index.remove(&rel);
            removed += 1;
        } else {
            bail!(
                "pathspec {:?} did not match any file",
                path.display().to_string()
            );
        }
    }

    index.save(&repo.index_path())?;

    let mut parts = Vec::new();
    if staged > 0 {
        parts.push(format!("staged {staged} file(s)"));
    }
    if removed > 0 {
        parts.push(format!("staged {removed} deletion(s)"));
    }
    if parts.is_empty() {
        parts.push("nothing to stage".to_string());
    }
    println!("{}", parts.join(", "));
    Ok(())
}
