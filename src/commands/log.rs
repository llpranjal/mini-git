//! `mgit log` -- walk the commit DAG backwards from HEAD.
//!
//! The walk is trivial because each commit stores its parent's id: start at the
//! branch tip and follow `parent` until a commit has none. That commit is the
//! root, and the absence of a parent is the only thing that marks it.
//!
//! Note the direction. Commits point *backwards* in time, never forwards. A
//! commit's bytes are fixed at creation, and its name is the hash of those
//! bytes, so it cannot later be updated to mention a child -- the name would
//! change and every reference to it would break. History is therefore a DAG of
//! backward edges, and "what came after this commit" is a question that can only
//! be answered by scanning, which is why `git log` is fast and finding a
//! commit's children is not.

use crate::commands::format_time;
use crate::repo::{Head, Repository};
use anyhow::Result;

pub fn run(max_count: Option<usize>, oneline: bool) -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;

    let head = match repo.head_commit()? {
        Some(oid) => oid,
        None => {
            let branch = match repo.read_head()? {
                Head::Branch(name) => name,
                Head::Detached(_) => unreachable!("detached HEAD always has a commit"),
            };
            println!("no commits yet on branch {branch}");
            return Ok(());
        }
    };

    // The limit goes into the walk, not around it, so `log -n 10` reads ten
    // commit objects regardless of how long the history is.
    let history = repo.history(head, max_count)?;

    for (i, (oid, commit)) in history.iter().enumerate() {
        if oneline {
            println!("{} {}", &oid.to_hex()[..7], commit.summary());
            continue;
        }
        if i > 0 {
            println!();
        }
        println!("commit {oid}");
        for parent in &commit.parents {
            println!("parent {parent}");
        }
        println!("tree   {}", commit.tree);
        println!("Author: {} <{}>", commit.author.name, commit.author.email);
        println!(
            "Date:   {}",
            format_time(commit.author.timestamp, &commit.author.tz_offset)
        );
        println!();
        for line in commit.message.lines() {
            println!("    {line}");
        }
    }
    Ok(())
}
