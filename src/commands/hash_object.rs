//! `mgit hash-object` -- the plumbing that shows what an object name *is*.
//!
//! Without `-w` this command touches no repository state at all. It frames the
//! input, hashes it, and prints the result. That is worth having as a separate
//! command because it makes the central claim of content addressing directly
//! observable: the same bytes always produce the same name, on any machine, in
//! any repository, with no coordination.

use crate::object::{self, ObjectType};
use crate::repo::Repository;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

pub fn run(path: Option<&Path>, stdin: bool, write: bool, type_str: &str) -> Result<()> {
    let kind = ObjectType::parse(type_str)?;

    let content = if stdin {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        buf
    } else {
        let path = path.context("expected a file path, or --stdin")?;
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
    };

    let framed = object::frame(kind, &content);

    if write {
        // Only `-w` needs a repository. Hashing is pure.
        let repo = Repository::discover(std::env::current_dir()?)?;
        let (oid, _existed) = repo.store().write_framed(&framed)?;
        println!("{oid}");
    } else {
        println!("{}", object::hash_framed(&framed));
    }
    Ok(())
}
