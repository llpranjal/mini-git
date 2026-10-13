//! mini-git: Git's loose-object database in Rust.
//!
//! The commit path is `index -> trees -> commit -> ref`.
//! Files become blobs, folders become trees, a commit points at a tree.
//! Everything is named by SHA-1 of its framed bytes, so identical content
//! stores once. Stock `git log` and `git fsck --strict` pass on repositories
//! this crate writes, and both tools produce identical object IDs.
//!
//! The first six commands are the core (init, hash-object, cat-file,
//! add, commit, log). The rest make stored snapshots useful without changing
//! the model. Deliberately out of scope: packfiles, merge, remotes, the
//! binary index.
pub mod commands;
pub mod diff;
pub mod index;
pub mod object;
pub mod oid;
pub mod repo;
pub mod scratch;
pub mod store;
pub mod trees;

pub use oid::Oid;
pub use repo::Repository;
