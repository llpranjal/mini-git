//! The loose object store: framed bytes in, zlib-compressed files out.
//!
//! # Layout
//!
//! ```text
//! .git/objects/ce/013625030ba8dba906f756967f9e9ca394464a
//!              \/ \------------------------------------/
//!         first 2 hex           remaining 38 hex
//! ```
//!
//! The file's entire content is one zlib stream (RFC 1950 -- with the 2-byte
//! header and trailing Adler-32 checksum, not raw deflate). Decompressing it
//! yields `<type> <size>\0<payload>` exactly as `object.rs` framed it.
//!
//! # Why this store is "write once"
//!
//! An object file is never modified after creation. If the target path already
//! exists, the content is already there -- that follows from the name being the
//! hash of the content. `write` therefore short-circuits, and that short-circuit
//! *is* the deduplication mechanism: committing a thousand files where nine
//! hundred are unchanged writes one hundred files, because the other nine
//! hundred hash to paths that already exist. No diffing, no comparison, no
//! bookkeeping. See README, "Content addressing".
//!
//! # What is not implemented
//!
//! Packfiles. Real Git eventually rewrites loose objects into a compressed
//! delta-encoded pack. This store only ever reads and writes loose objects, so
//! it can read a fresh repository written by real Git but not one that has been
//! `git gc`-ed. That limitation is stated in the README and exercised by
//! nothing here, because pretending otherwise would be worse than saying so.

use crate::object::{self, Object, ObjectType};
use crate::oid::Oid;
use anyhow::{bail, Context, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// zlib level used for loose objects.
///
/// This is **1**, not zlib's usual default of 6, because that is what Git uses:
/// `core.loosecompression` defaults to `Z_BEST_SPEED`, while `core.compression`
/// (which defaults to 6) applies to packfiles. Git's reasoning is that loose
/// objects are transient -- they get rewritten into a delta-compressed pack
/// later, so paying for slow compression twice is wasted work.
///
/// Matching it is *not* required to interoperate: the object's name is the hash
/// of the uncompressed bytes, so a store using level 9 would still be readable
/// by Git and vice versa. Matching buys something narrower and stronger --- the
/// loose object *files* come out byte-identical to Git's, which the interop
/// suite asserts directly.
///
/// This store never packs, so its objects are not transient, and level 6 would
/// genuinely store them smaller. The benchmark measures both levels rather than
/// leaving that as an assumption; see `results/` for what the difference
/// actually is.
const LOOSE_COMPRESSION_LEVEL: u32 = 1;

pub struct ObjectStore {
    /// Absolute path to `.git/objects`.
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(objects_dir: impl Into<PathBuf>) -> Self {
        ObjectStore {
            root: objects_dir.into(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, oid: Oid) -> PathBuf {
        let (dir, file) = oid.path_parts();
        self.root.join(dir).join(file)
    }

    pub fn contains(&self, oid: Oid) -> bool {
        self.path_for(oid).exists()
    }

    /// Hash `framed` bytes, and write them compressed unless already present.
    ///
    /// Returns the OID either way, so callers cannot tell (and must not care)
    /// whether a write happened. `existed` is reported separately only because
    /// the benchmark needs to count unique blobs versus total file versions.
    pub fn write_framed(&self, framed: &[u8]) -> Result<(Oid, bool)> {
        let oid = object::hash_framed(framed);
        let path = self.path_for(oid);
        if path.exists() {
            // Already stored. Skipping the write is not just an optimization:
            // rewriting would churn mtimes for no change in content.
            return Ok((oid, true));
        }

        let parent = path.parent().expect("object path always has a fanout dir");
        fs::create_dir_all(parent)
            .with_context(|| format!("creating object directory {}", parent.display()))?;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(LOOSE_COMPRESSION_LEVEL));
        encoder.write_all(framed).context("compressing object")?;
        let compressed = encoder.finish().context("finishing zlib stream")?;

        // Write to a temporary file in the same directory, then rename.
        //
        // Rename within a directory is atomic on macOS and Linux, so a reader
        // never observes a half-written object. Writing in place would leave a
        // truncated file if the process died mid-write, and because the name is
        // the hash, every future lookup would find that corrupt file and trust
        // it -- content addressing makes corruption sticky, so the write path
        // has to be careful.
        let tmp = parent.join(format!("tmp_{}", oid.to_hex()));
        fs::write(&tmp, &compressed)
            .with_context(|| format!("writing temporary object {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("installing object {}", path.display()))?;

        Ok((oid, false))
    }

    /// Convenience wrapper: frame, hash, store.
    pub fn write(&self, object: &Object) -> Result<Oid> {
        let (oid, _) = self.write_framed(&object.framed())?;
        Ok(oid)
    }

    /// Read and decompress an object, returning its framed bytes.
    pub fn read_framed(&self, oid: Oid) -> Result<Vec<u8>> {
        let path = self.path_for(oid);
        let compressed = fs::read(&path)
            .with_context(|| format!("object {oid} not found (looked in {})", path.display()))?;

        let mut framed = Vec::new();
        ZlibDecoder::new(&compressed[..])
            .read_to_end(&mut framed)
            .with_context(|| format!("object {oid} is not a valid zlib stream"))?;

        // Verify the content actually hashes to the name it was filed under.
        //
        // This is cheap and catches both disk corruption and the much likelier
        // failure during development: a bug in the writer that stored bytes
        // under the wrong name. Real Git only does this under `git fsck`; doing
        // it on every read is affordable at this scale and turns a class of
        // silent wrongness into a loud error.
        let actual = object::hash_framed(&framed);
        if actual != oid {
            bail!("object {oid} is corrupt: content hashes to {actual}");
        }
        Ok(framed)
    }

    pub fn read(&self, oid: Oid) -> Result<Object> {
        Object::deserialize(&self.read_framed(oid)?)
    }

    /// Read the type and payload size without fully parsing -- what
    /// `cat-file -t` and `cat-file -s` need.
    pub fn read_header(&self, oid: Oid) -> Result<(ObjectType, usize)> {
        let framed = self.read_framed(oid)?;
        let (kind, payload) = object::split_frame(&framed)?;
        Ok((kind, payload.len()))
    }

    /// Expand an unambiguous hash prefix (>= 4 characters, as in real Git) to a
    /// full OID by scanning the fanout directory.
    ///
    /// Only the one fanout directory is scanned rather than the whole store,
    /// which is the entire reason Git splits on the first byte: prefix lookup
    /// touches 1/256th of the objects.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<Oid> {
        if prefix.len() == 40 {
            return Oid::from_hex(prefix);
        }
        if prefix.len() < 4 {
            bail!("object name {prefix:?} is too short (need at least 4 hex characters)");
        }
        if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("object name {prefix:?} is not hexadecimal");
        }

        let (dir_part, rest) = prefix.split_at(2);
        let dir = self.root.join(dir_part);
        let mut matches = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(rest) {
                    matches.push(format!("{dir_part}{name}"));
                }
            }
        }

        match matches.len() {
            0 => bail!("no object matches prefix {prefix:?}"),
            1 => Oid::from_hex(&matches[0]),
            n => bail!("prefix {prefix:?} is ambiguous ({n} objects match)"),
        }
    }

    /// Every object currently stored, for the benchmark and for `fsck`-style
    /// counting. Order is filesystem order and therefore unspecified.
    pub fn iter_oids(&self) -> Result<Vec<Oid>> {
        let mut out = Vec::new();
        let dirs = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(_) => return Ok(out),
        };
        for fanout in dirs.flatten() {
            let name = fanout.file_name().to_string_lossy().to_string();
            // Skip `info/` and `pack/`, which Git creates but this store never
            // populates, and any leftover `tmp_` files.
            if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            for object in fs::read_dir(fanout.path())?.flatten() {
                let rest = object.file_name().to_string_lossy().to_string();
                if rest.len() == 38 {
                    if let Ok(oid) = Oid::from_hex(&format!("{name}{rest}")) {
                        out.push(oid);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Total bytes the object files occupy on disk (compressed), for the
    /// storage measurements.
    pub fn on_disk_bytes(&self) -> Result<u64> {
        let mut total = 0;
        for oid in self.iter_oids()? {
            total += fs::metadata(self.path_for(oid))?.len();
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{FileMode, Tree, TreeEntry};

    fn temp_store() -> (crate::scratch::ScratchDir, ObjectStore) {
        let dir = crate::scratch::ScratchDir::new("store");
        let store = ObjectStore::new(dir.path().join("objects"));
        (dir, store)
    }

    #[test]
    fn write_then_read_roundtrips() {
        let (_guard, store) = temp_store();
        let obj = Object::Blob(b"hello\n".to_vec());
        let oid = store.write(&obj).unwrap();
        assert_eq!(oid.to_hex(), "ce013625030ba8dba906f756967f9e9ca394464a");
        assert_eq!(store.read(oid).unwrap(), obj);
    }

    #[test]
    fn object_lands_in_two_char_fanout_directory() {
        let (_guard, store) = temp_store();
        let oid = store.write(&Object::Blob(b"hello\n".to_vec())).unwrap();
        let path = store
            .root()
            .join("ce")
            .join("013625030ba8dba906f756967f9e9ca394464a");
        assert!(path.exists(), "expected object at {}", path.display());
        assert!(store.contains(oid));
    }

    #[test]
    fn file_on_disk_is_a_zlib_stream_not_plaintext() {
        let (_guard, store) = temp_store();
        let oid = store.write(&Object::Blob(b"hello\n".to_vec())).unwrap();
        let raw = fs::read(store.root().join("ce").join(&oid.to_hex()[2..])).unwrap();
        // zlib (RFC 1950) at level 1 starts with 0x78 0x01 -- the same two
        // bytes real git writes, which the interop suite checks directly.
        assert_eq!(&raw[..2], &[0x78, 0x01]);
        // And the plaintext must not be sitting there readable.
        assert!(!raw.windows(5).any(|w| w == b"hello"));
    }

    #[test]
    fn writing_the_same_content_twice_does_not_create_a_second_file() {
        let (_guard, store) = temp_store();
        let (oid1, existed1) = store
            .write_framed(&Object::Blob(b"dup".to_vec()).framed())
            .unwrap();
        let (oid2, existed2) = store
            .write_framed(&Object::Blob(b"dup".to_vec()).framed())
            .unwrap();
        assert_eq!(oid1, oid2);
        assert!(!existed1, "first write should be new");
        assert!(existed2, "second write should have been deduplicated");
        assert_eq!(store.iter_oids().unwrap().len(), 1);
    }

    #[test]
    fn read_detects_a_corrupted_object_file() {
        let (_guard, store) = temp_store();
        let oid = store.write(&Object::Blob(b"original".to_vec())).unwrap();

        // Overwrite the file with a valid zlib stream of *different* content,
        // simulating either bit rot or a buggy writer.
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(LOOSE_COMPRESSION_LEVEL));
        enc.write_all(&Object::Blob(b"tampered".to_vec()).framed())
            .unwrap();
        let bad = enc.finish().unwrap();
        let (d, f) = oid.path_parts();
        fs::write(store.root().join(d).join(f), bad).unwrap();

        let err = store.read(oid).unwrap_err().to_string();
        assert!(err.contains("corrupt"), "unexpected error: {err}");
    }

    #[test]
    fn read_header_reports_type_and_size_without_parsing() {
        let (_guard, store) = temp_store();
        let oid = store.write(&Object::Blob(b"12345".to_vec())).unwrap();
        let (kind, size) = store.read_header(oid).unwrap();
        assert_eq!(kind, ObjectType::Blob);
        assert_eq!(size, 5);
    }

    #[test]
    fn prefix_resolution_expands_short_names_and_rejects_ambiguity() {
        let (_guard, store) = temp_store();
        let oid = store.write(&Object::Blob(b"hello\n".to_vec())).unwrap();
        assert_eq!(store.resolve_prefix("ce0136").unwrap(), oid);
        assert_eq!(store.resolve_prefix(&oid.to_hex()).unwrap(), oid);

        assert!(
            store.resolve_prefix("ce").is_err(),
            "3 chars or fewer must be rejected"
        );
        assert!(store.resolve_prefix("ffffffff").is_err(), "no such object");
        assert!(store.resolve_prefix("zzzz").is_err(), "not hex");
    }

    #[test]
    fn trees_and_commits_roundtrip_through_the_store() {
        let (_guard, store) = temp_store();
        let blob = store.write(&Object::Blob(b"content\n".to_vec())).unwrap();
        let tree = Object::Tree(Tree {
            entries: vec![TreeEntry {
                mode: FileMode::Regular,
                name: "file.txt".into(),
                oid: blob,
            }],
        });
        let tree_oid = store.write(&tree).unwrap();
        match store.read(tree_oid).unwrap() {
            Object::Tree(t) => assert_eq!(t.get("file.txt").unwrap().oid, blob),
            other => panic!("expected a tree, got {other:?}"),
        }
    }

    #[test]
    fn iter_oids_ignores_non_object_directories() {
        let (_guard, store) = temp_store();
        store.write(&Object::Blob(b"a".to_vec())).unwrap();
        fs::create_dir_all(store.root().join("pack")).unwrap();
        fs::create_dir_all(store.root().join("info")).unwrap();
        assert_eq!(store.iter_oids().unwrap().len(), 1);
    }
}
