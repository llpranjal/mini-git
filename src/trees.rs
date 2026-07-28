//! Converting between the flat index and Git's nested tree objects.
//!
//! The index is flat: `src/main.rs -> blob`. A commit points at a single tree
//! object, and a tree holds only one path component per entry, so `src/main.rs`
//! must become a root tree containing a `src` entry that points at another tree
//! containing `main.rs`.
//!
//! # Why nested rather than one flat tree of full paths
//!
//! A flat root tree listing `src/main.rs`, `src/lib.rs`, `docs/a.md` would be
//! simpler to build. It would also destroy most of the deduplication. With
//! nested trees, editing one file in `src/` changes the `src` tree and the root
//! tree, and every *other* directory's tree object is reused by id --- a commit
//! in a repository with a thousand directories writes a handful of new tree
//! objects, not one giant one. The saving compounds: the benchmark measures it.
//!
//! The same structure is what makes `diff` between two commits fast in real
//! Git: if two directories have the same tree id, nothing inside them changed,
//! and the entire subtree can be skipped without reading a single blob.

use crate::index::Index;
use crate::object::{FileMode, Object, Tree, TreeEntry};
use crate::oid::Oid;
use crate::store::ObjectStore;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// An in-memory directory before it becomes a tree object.
#[derive(Default)]
struct DirNode {
    files: BTreeMap<String, (FileMode, Oid)>,
    dirs: BTreeMap<String, DirNode>,
}

/// Write the index out as a tree hierarchy and return the root tree's id.
///
/// Writes bottom-up, because a tree entry stores its child's id, so a directory
/// cannot be named until everything inside it has been.
pub fn build_tree_from_index(store: &ObjectStore, index: &Index) -> Result<Oid> {
    let mut root = DirNode::default();
    for (path, entry) in index.iter() {
        insert_path(&mut root, path, entry.mode, entry.oid)?;
    }
    write_dir(store, &root)
}

fn insert_path(root: &mut DirNode, path: &str, mode: FileMode, oid: Oid) -> Result<()> {
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    let (file_name, dirs) = match components.split_last() {
        Some(split) => split,
        None => bail!("index contains an empty path"),
    };

    // Git cannot represent `a` as both a file and a directory in one tree. Both
    // orders of arrival have to be caught: `a` then `a/b` trips the check in the
    // descent loop, `a/b` then `a` trips the one after it. Only checking one
    // direction leaves a bug that depends on iteration order, which is exactly
    // the kind that survives testing. This is reachable if the index is edited
    // by hand, so it fails loudly rather than writing a tree real Git rejects.
    let mut node = root;
    for dir in dirs {
        if node.files.contains_key(*dir) {
            bail!("path {path:?} is both a file and a directory in the index");
        }
        node = node.dirs.entry((*dir).to_string()).or_default();
    }
    if node.dirs.contains_key(*file_name) {
        bail!("path {path:?} is both a file and a directory in the index");
    }
    node.files.insert((*file_name).to_string(), (mode, oid));
    Ok(())
}

fn write_dir(store: &ObjectStore, node: &DirNode) -> Result<Oid> {
    let mut entries = Vec::new();
    for (name, (mode, oid)) in &node.files {
        entries.push(TreeEntry {
            mode: *mode,
            name: name.clone(),
            oid: *oid,
        });
    }
    for (name, child) in &node.dirs {
        // Recurse first: the child's id is part of this entry.
        let child_oid = write_dir(store, child)?;
        entries.push(TreeEntry {
            mode: FileMode::Directory,
            name: name.clone(),
            oid: child_oid,
        });
    }
    // `Tree::serialize` applies Git's sort order, so the order entries were
    // pushed in here does not matter.
    store.write(&Object::Tree(Tree { entries }))
}

/// The inverse: walk a tree hierarchy into a flat `path -> (mode, blob id)` map.
///
/// Used by `status`, `diff`, and `checkout`, all of which want to compare a
/// commit against the flat index or the flat working-tree listing.
pub fn flatten_tree(
    store: &ObjectStore,
    tree_oid: Oid,
) -> Result<BTreeMap<String, (FileMode, Oid)>> {
    let mut out = BTreeMap::new();
    flatten_into(store, tree_oid, "", &mut out)?;
    Ok(out)
}

fn flatten_into(
    store: &ObjectStore,
    tree_oid: Oid,
    prefix: &str,
    out: &mut BTreeMap<String, (FileMode, Oid)>,
) -> Result<()> {
    let tree = match store.read(tree_oid)? {
        Object::Tree(t) => t,
        other => bail!(
            "{tree_oid} is a {}, not a tree",
            other.object_type().as_str()
        ),
    };

    for entry in tree.entries {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if entry.mode.is_dir() {
            flatten_into(store, entry.oid, &path, out)?;
        } else {
            out.insert(path, (entry.mode, entry.oid));
        }
    }
    Ok(())
}

/// The flattened tree of a commit, or an empty map for "no commit yet".
///
/// Taking an `Option` here removes an `if let` from every caller: the state
/// before the first commit behaves exactly like a commit of an empty tree,
/// which is the correct mental model and avoids a special case in `status`,
/// `diff`, and `commit`.
pub fn commit_tree_map(
    store: &ObjectStore,
    repo: &crate::repo::Repository,
    commit: Option<Oid>,
) -> Result<BTreeMap<String, (FileMode, Oid)>> {
    match commit {
        None => Ok(BTreeMap::new()),
        Some(oid) => {
            let commit = repo
                .read_commit(oid)
                .with_context(|| format!("reading commit {oid}"))?;
            flatten_tree(store, commit.tree)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexEntry;
    use crate::scratch::ScratchDir;

    fn store() -> (ScratchDir, ObjectStore) {
        let dir = ScratchDir::new("trees");
        let store = ObjectStore::new(dir.path().join("objects"));
        (dir, store)
    }

    fn blob(store: &ObjectStore, content: &str) -> Oid {
        store
            .write(&Object::Blob(content.as_bytes().to_vec()))
            .unwrap()
    }

    fn index_of(pairs: &[(&str, Oid)]) -> Index {
        let mut index = Index::new();
        for (path, oid) in pairs {
            index.insert(
                *path,
                IndexEntry {
                    mode: FileMode::Regular,
                    oid: *oid,
                },
            );
        }
        index
    }

    #[test]
    fn flat_index_becomes_nested_trees() {
        let (_g, store) = store();
        let a = blob(&store, "a");
        let b = blob(&store, "b");
        let index = index_of(&[("README.md", a), ("src/main.rs", b)]);

        let root_oid = build_tree_from_index(&store, &index).unwrap();
        let root = match store.read(root_oid).unwrap() {
            Object::Tree(t) => t,
            other => panic!("expected tree, got {other:?}"),
        };

        // The root has two entries and one of them is a directory holding the
        // nested file -- not a single entry literally named "src/main.rs".
        assert_eq!(root.entries.len(), 2);
        let src = root.get("src").expect("root should contain a `src` entry");
        assert_eq!(src.mode, FileMode::Directory);
        assert!(root.get("src/main.rs").is_none());

        let sub = match store.read(src.oid).unwrap() {
            Object::Tree(t) => t,
            other => panic!("expected tree, got {other:?}"),
        };
        assert_eq!(sub.entries.len(), 1);
        assert_eq!(sub.entries[0].name, "main.rs");
        assert_eq!(sub.entries[0].oid, b);
    }

    #[test]
    fn deep_nesting_creates_one_tree_per_level() {
        let (_g, store) = store();
        let x = blob(&store, "x");
        let index = index_of(&[("a/b/c/d.txt", x)]);
        let root = build_tree_from_index(&store, &index).unwrap();

        let flat = flatten_tree(&store, root).unwrap();
        assert_eq!(flat.keys().collect::<Vec<_>>(), vec!["a/b/c/d.txt"]);
        // root + a + a/b + a/b/c = 4 trees, plus the 1 blob.
        assert_eq!(store.iter_oids().unwrap().len(), 5);
    }

    #[test]
    fn build_then_flatten_is_the_identity() {
        let (_g, store) = store();
        let a = blob(&store, "a");
        let b = blob(&store, "b");
        let c = blob(&store, "c");
        let index = index_of(&[("z.txt", a), ("src/main.rs", b), ("src/deep/mod.rs", c)]);

        let root = build_tree_from_index(&store, &index).unwrap();
        let flat = flatten_tree(&store, root).unwrap();

        assert_eq!(flat.len(), 3);
        assert_eq!(flat["z.txt"].1, a);
        assert_eq!(flat["src/main.rs"].1, b);
        assert_eq!(flat["src/deep/mod.rs"].1, c);
    }

    #[test]
    fn identical_content_produces_an_identical_root_tree_id() {
        // Content addressing: the tree id is a pure function of what is in it.
        let (_g, store) = store();
        let a = blob(&store, "a");
        let one = build_tree_from_index(&store, &index_of(&[("f.txt", a)])).unwrap();
        let two = build_tree_from_index(&store, &index_of(&[("f.txt", a)])).unwrap();
        assert_eq!(one, two);
    }

    #[test]
    fn an_unchanged_subdirectory_keeps_its_tree_id_across_commits() {
        // This is the property the whole nested-tree design exists for: change
        // one file in one directory, and every other directory's tree object is
        // reused by name rather than rewritten.
        let (_g, store) = store();
        let (a, b, b2) = (blob(&store, "a"), blob(&store, "b"), blob(&store, "b2"));

        let first = index_of(&[("untouched/x.txt", a), ("changing/y.txt", b)]);
        let second = index_of(&[("untouched/x.txt", a), ("changing/y.txt", b2)]);

        let root1 = build_tree_from_index(&store, &first).unwrap();
        let root2 = build_tree_from_index(&store, &second).unwrap();
        assert_ne!(root1, root2, "the root must change");

        let get = |root: Oid, name: &str| match store.read(root).unwrap() {
            Object::Tree(t) => t.get(name).unwrap().oid,
            other => panic!("expected tree, got {other:?}"),
        };
        assert_eq!(
            get(root1, "untouched"),
            get(root2, "untouched"),
            "the untouched subtree must be shared by id"
        );
        assert_ne!(get(root1, "changing"), get(root2, "changing"));
    }

    #[test]
    fn an_empty_index_produces_gits_canonical_empty_tree() {
        // 4b825dc642cb6eb9a060e54bf8d69288fbee4904 is the id of the empty tree
        // in every Git repository in existence. Reproducing it is a strong
        // check that tree framing is byte-exact.
        let (_g, store) = store();
        let root = build_tree_from_index(&store, &Index::new()).unwrap();
        assert_eq!(root.to_hex(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    }

    #[test]
    fn executable_bit_survives_the_round_trip() {
        let (_g, store) = store();
        let x = blob(&store, "#!/bin/sh\n");
        let mut index = Index::new();
        index.insert(
            "run.sh",
            IndexEntry {
                mode: FileMode::Executable,
                oid: x,
            },
        );

        let root = build_tree_from_index(&store, &index).unwrap();
        let flat = flatten_tree(&store, root).unwrap();
        assert_eq!(flat["run.sh"].0, FileMode::Executable);
    }

    #[test]
    fn a_path_that_is_both_file_and_directory_is_rejected() {
        let (_g, store) = store();
        let a = blob(&store, "a");
        // Both arrival orders must be rejected, not just the one the BTreeMap
        // happens to yield first.
        for pair in [[("a", a), ("a/b", a)], [("a/b", a), ("a", a)]] {
            let err = build_tree_from_index(&store, &index_of(&pair))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("both a file and a directory"),
                "unexpected: {err}"
            );
        }
    }
}
