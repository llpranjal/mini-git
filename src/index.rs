//! The staging index: a flat table of `path -> (mode, blob id)`.
//!
//! # Why this exists at all
//!
//! It is the single most-questioned part of Git's design, and the answer is
//! that a commit needs a *proposed* snapshot that is neither the last commit
//! nor the current working tree.
//!
//! - The working tree is whatever is on disk right now, including the debug
//!   `println!` you have not removed yet.
//! - HEAD's tree is what you last committed.
//! - The index is what you have decided the *next* commit should contain.
//!
//! Without it, `commit` would have to take a file list on every invocation, and
//! "commit these three hunks but not that one" would be impossible. With it,
//! `add` is a checkpoint: it hashes the file and writes the blob into the object
//! store immediately. That is why staging a file and then editing it further
//! still commits the staged version -- the content was already captured. This
//! surprises people, and it falls straight out of the design rather than being
//! a special case.
//!
//! # This format vs. real Git's
//!
//! Real Git's index is binary: a `DIRC` header, a big-endian entry count, then
//! per-entry `ctime/mtime/dev/ino/mode/uid/gid/size` stat data, the 20-byte
//! object id, flags, and a NUL-padded path, followed by optional extensions and
//! a trailing SHA-1 of the whole file.
//!
//! This implementation stores one line of text per entry instead:
//!
//! ```text
//! <mode> <40-hex blob id> <path>
//! ```
//!
//! The trade, stated plainly:
//!
//! * **Given up:** the stat cache. Real Git compares a file's `mtime`, `size`,
//!   and `ino` against the index to decide whether it even needs to read the
//!   file. Without that, `status` and `diff` re-hash every tracked file on every
//!   invocation. That cost is real and is measured in the benchmark rather than
//!   waved away.
//! * **Gained:** the format is readable with `cat`, diffable, and explainable in
//!   one sentence. For a project whose purpose is to be understood, a binary
//!   format whose only advantage is a cache would have been the wrong trade.
//!
//! Because the format differs, it is stored at `.git/mini-index`, not
//! `.git/index` -- see `repo.rs` for why that choice preserves interoperability
//! with real Git rather than breaking it.

use crate::object::FileMode;
use crate::oid::Oid;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexEntry {
    pub mode: FileMode,
    pub oid: Oid,
}

/// Entries keyed by repository-relative path.
///
/// A `BTreeMap` rather than a `Vec` because two operations dominate: lookup by
/// path (`status` and `diff`, once per file) and ordered iteration (writing the
/// file, and building trees, which requires sorted input). A `BTreeMap` gives
/// both without an explicit sort step, and its ordering is byte order over the
/// path, which is exactly what tree construction wants.
#[derive(Clone, Debug, Default)]
pub struct Index {
    entries: BTreeMap<String, IndexEntry>,
}

impl Index {
    pub fn new() -> Index {
        Index::default()
    }

    /// Read the index, treating a missing file as an empty index.
    ///
    /// "Missing means empty" is what makes a freshly `init`-ed repository work
    /// without `init` having to create the file, and it means deleting the index
    /// is a valid way to unstage everything.
    pub fn load(path: &Path) -> Result<Index> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::new()),
            Err(e) => return Err(e).with_context(|| format!("reading index {}", path.display())),
        };

        let mut index = Index::new();
        for (lineno, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            // Split twice from the left, not `split_whitespace`: the path is the
            // remainder of the line and may itself contain spaces.
            let (mode, rest) = line
                .split_once(' ')
                .with_context(|| format!("index line {} is malformed: {line:?}", lineno + 1))?;
            let (hash, path_str) = rest
                .split_once(' ')
                .with_context(|| format!("index line {} is malformed: {line:?}", lineno + 1))?;

            if path_str.is_empty() {
                bail!("index line {} has an empty path", lineno + 1);
            }
            index.entries.insert(
                path_str.to_string(),
                IndexEntry {
                    mode: FileMode::parse(mode)?,
                    oid: Oid::from_hex(hash)?,
                },
            );
        }
        Ok(index)
    }

    /// Write the index atomically.
    ///
    /// Same temp-file-then-rename discipline as the object store, for a
    /// different reason: a half-written index is not detectable by hashing (it
    /// has no name to check against), so a crash mid-write would silently lose
    /// staged work. Rename makes the update all-or-nothing.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = String::new();
        for (path_str, entry) in &self.entries {
            text.push_str(&format!(
                "{} {} {}\n",
                entry.mode.as_str(),
                entry.oid,
                path_str
            ));
        }

        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))?;
        Ok(())
    }

    pub fn insert(&mut self, path: impl Into<String>, entry: IndexEntry) {
        self.entries.insert(path.into(), entry);
    }

    pub fn remove(&mut self, path: &str) -> bool {
        self.entries.remove(path).is_some()
    }

    pub fn get(&self, path: &str) -> Option<&IndexEntry> {
        self.entries.get(path)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &IndexEntry)> {
        self.entries.iter()
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes([byte; 20])
    }

    #[test]
    fn missing_index_file_loads_as_empty() {
        let dir = ScratchDir::new("index-missing");
        let index = Index::load(&dir.path().join("nope")).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = ScratchDir::new("index-roundtrip");
        let path = dir.path().join("mini-index");

        let mut index = Index::new();
        index.insert(
            "a.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(0xaa),
            },
        );
        index.insert(
            "src/main.rs",
            IndexEntry {
                mode: FileMode::Executable,
                oid: oid(0xbb),
            },
        );
        index.save(&path).unwrap();

        let loaded = Index::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("a.txt").unwrap().oid, oid(0xaa));
        assert_eq!(
            loaded.get("src/main.rs").unwrap().mode,
            FileMode::Executable
        );
    }

    #[test]
    fn on_disk_format_is_one_readable_line_per_entry() {
        let dir = ScratchDir::new("index-format");
        let path = dir.path().join("mini-index");
        let mut index = Index::new();
        index.insert(
            "a.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(0x00),
            },
        );
        index.save(&path).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "100644 0000000000000000000000000000000000000000 a.txt\n"
        );
    }

    #[test]
    fn entries_are_written_in_path_order_regardless_of_insertion_order() {
        let dir = ScratchDir::new("index-order");
        let path = dir.path().join("mini-index");
        let mut index = Index::new();
        for name in ["z.txt", "a.txt", "m.txt"] {
            index.insert(
                name,
                IndexEntry {
                    mode: FileMode::Regular,
                    oid: oid(1),
                },
            );
        }
        index.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let paths: Vec<&str> = text
            .lines()
            .map(|l| l.rsplit(' ').next().unwrap())
            .collect();
        assert_eq!(paths, vec!["a.txt", "m.txt", "z.txt"]);
    }

    #[test]
    fn paths_containing_spaces_survive_a_roundtrip() {
        // This is why the parser splits twice from the left rather than using
        // `split_whitespace`.
        let dir = ScratchDir::new("index-spaces");
        let path = dir.path().join("mini-index");
        let mut index = Index::new();
        index.insert(
            "my notes/todo list.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(7),
            },
        );
        index.save(&path).unwrap();

        let loaded = Index::load(&path).unwrap();
        assert!(loaded.contains("my notes/todo list.txt"));
    }

    #[test]
    fn re_adding_a_path_replaces_the_previous_entry() {
        let mut index = Index::new();
        index.insert(
            "a.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(1),
            },
        );
        index.insert(
            "a.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(2),
            },
        );
        assert_eq!(index.len(), 1);
        assert_eq!(index.get("a.txt").unwrap().oid, oid(2));
    }

    #[test]
    fn remove_reports_whether_anything_was_staged() {
        let mut index = Index::new();
        index.insert(
            "a.txt",
            IndexEntry {
                mode: FileMode::Regular,
                oid: oid(1),
            },
        );
        assert!(index.remove("a.txt"));
        assert!(!index.remove("a.txt"));
    }

    #[test]
    fn malformed_lines_are_rejected_rather_than_silently_skipped() {
        let dir = ScratchDir::new("index-bad");
        let path = dir.path().join("mini-index");
        fs::write(&path, "garbage\n").unwrap();
        assert!(Index::load(&path).is_err());

        fs::write(&path, "100644 nothex a.txt\n").unwrap();
        assert!(Index::load(&path).is_err());
    }
}
