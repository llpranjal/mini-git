//! `mgit cat-file` -- read an object back out of the store.
//!
//! The counterpart to `hash-object`, and the other half of the interoperability
//! claim: this command is what proves the tool can read objects real Git wrote,
//! not just write ones Git can read.
//!
//! Output formatting follows `git cat-file -p`:
//!
//! * a blob prints verbatim, bytes unchanged;
//! * a commit prints its raw payload, headers and all;
//! * a tree prints one readable line per entry, `<mode> <type> <oid>\t<name>`,
//!   because the on-disk form contains raw binary hashes and dumping it to a
//!   terminal would be useless.

use crate::object::{Object, ObjectType};
use crate::repo::Repository;
use anyhow::Result;
use std::io::Write;

pub enum Mode {
    Pretty,
    Type,
    Size,
}

pub fn run(rev: &str, mode: Mode) -> Result<()> {
    let repo = Repository::discover(std::env::current_dir()?)?;
    let store = repo.store();

    // Accept a branch name or `HEAD` as well as a hash, so `cat-file -p HEAD`
    // works the way a user expects.
    let oid = repo.resolve_rev(rev)?;

    match mode {
        Mode::Type => {
            let (kind, _) = store.read_header(oid)?;
            println!("{}", kind.as_str());
        }
        Mode::Size => {
            let (_, size) = store.read_header(oid)?;
            println!("{size}");
        }
        Mode::Pretty => match store.read(oid)? {
            Object::Blob(bytes) => {
                // Write raw bytes to stdout rather than going through a String.
                // A blob is not necessarily UTF-8, and `cat-file -p` on a binary
                // file must reproduce it exactly -- lossy conversion would
                // corrupt the very content the store exists to preserve.
                std::io::stdout().write_all(&bytes)?;
            }
            Object::Commit(commit) => {
                std::io::stdout().write_all(&commit.serialize())?;
            }
            Object::Tree(tree) => {
                for entry in &tree.entries {
                    let kind = if entry.mode.is_dir() {
                        ObjectType::Tree
                    } else {
                        ObjectType::Blob
                    };
                    // Git pads the mode to six characters here (so `40000`
                    // prints as `040000`) even though the stored form is
                    // unpadded. Matching that keeps output diffable against
                    // real `git cat-file -p`.
                    println!(
                        "{:0>6} {} {}\t{}",
                        entry.mode.as_str(),
                        kind.as_str(),
                        entry.oid,
                        entry.name
                    );
                }
            }
        },
    }
    Ok(())
}
