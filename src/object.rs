//! The object model: blobs, trees, and commits, and the exact byte layout Git
//! uses for each.
//!
//! # The framing rule
//!
//! Every Git object, regardless of type, is stored as:
//!
//! ```text
//! <type> <size-in-decimal>\0<payload>
//! ```
//!
//! The object's name is the SHA-1 of *those* bytes -- header included, before
//! any compression. This is the single most important invariant in the whole
//! project, because it is what makes the on-disk format interoperable: real Git
//! and this program will independently arrive at the same 40-character name for
//! the same content only if both hash the framed form.
//!
//! Two consequences worth internalizing:
//!
//! 1. The header is inside the hash, so a blob and a commit with byte-identical
//!    payloads still get different names. Type confusion is impossible.
//! 2. The size is inside the hash, so a truncated object cannot masquerade as a
//!    shorter valid one. Reading an object re-checks the declared length.
//!
//! Compression is *not* part of the identity. zlib is a storage detail applied
//! after the name is computed (see `store.rs`). That separation is deliberate:
//! different zlib levels produce different bytes on disk, and if compression
//! were inside the hash, two Gits with different zlib settings could not share
//! objects.

use crate::oid::Oid;
use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};

/// The object types this implementation understands.
///
/// Git has a fourth type, `tag` (an annotated tag object). It is deliberately
/// not implemented here -- see README, "What is deliberately not implemented".
/// Parsing still names it in error messages so an unsupported object produces a
/// comprehensible failure rather than "unknown type".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectType {
    Blob,
    Tree,
    Commit,
}

impl ObjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Commit => "commit",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "blob" => Ok(ObjectType::Blob),
            "tree" => Ok(ObjectType::Tree),
            "commit" => Ok(ObjectType::Commit),
            "tag" => bail!("annotated tag objects are not supported by this implementation"),
            other => bail!("unknown object type {other:?}"),
        }
    }
}

/// A parsed object. `Blob` keeps raw bytes because file contents are not
/// necessarily UTF-8 and re-encoding them would change their hash.
// Keeping the three variants direct makes the core model easier to read; these
// objects are short-lived during CLI commands, so boxing Commit adds indirection
// without a measured benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    Blob(Vec<u8>),
    Tree(Tree),
    Commit(Commit),
}

impl Object {
    pub fn object_type(&self) -> ObjectType {
        match self {
            Object::Blob(_) => ObjectType::Blob,
            Object::Tree(_) => ObjectType::Tree,
            Object::Commit(_) => ObjectType::Commit,
        }
    }

    /// The type-specific payload, i.e. everything after the `\0` in the header.
    pub fn payload(&self) -> Vec<u8> {
        match self {
            Object::Blob(data) => data.clone(),
            Object::Tree(tree) => tree.serialize(),
            Object::Commit(commit) => commit.serialize(),
        }
    }

    /// Header + payload: the exact bytes that get hashed and (after zlib) the
    /// exact bytes a reader recovers.
    pub fn framed(&self) -> Vec<u8> {
        frame(self.object_type(), &self.payload())
    }

    /// The object's name. Computing this never touches the disk, which is why
    /// `hash-object` without `-w` is a pure function of its input.
    pub fn oid(&self) -> Oid {
        hash_framed(&self.framed())
    }

    /// Inverse of `framed`: split the header off, validate it, parse the body.
    pub fn deserialize(framed: &[u8]) -> Result<Object> {
        let (kind, payload) = split_frame(framed)?;
        match kind {
            ObjectType::Blob => Ok(Object::Blob(payload.to_vec())),
            ObjectType::Tree => Ok(Object::Tree(Tree::parse(payload)?)),
            ObjectType::Commit => Ok(Object::Commit(Commit::parse(payload)?)),
        }
    }
}

/// Build `<type> <size>\0<payload>`.
pub fn frame(kind: ObjectType, payload: &[u8]) -> Vec<u8> {
    let header = format!("{} {}\0", kind.as_str(), payload.len());
    let mut out = Vec::with_capacity(header.len() + payload.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    out
}

/// SHA-1 over already-framed bytes. Kept separate from `Object::oid` so that
/// `hash-object` can hash a file streamed from disk without building an
/// `Object`, and so tests can hash bytes produced by real Git.
pub fn hash_framed(framed: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    hasher.update(framed);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(&digest);
    Oid::from_bytes(bytes)
}

/// Split framed bytes into type and payload, validating the declared size.
///
/// The size check is not paranoia: a loose object file is the only copy of that
/// content, and a silently truncated read would produce a tree or commit that
/// parses "successfully" with entries missing. Failing loudly here means
/// corruption surfaces at read time instead of as mysteriously missing files.
pub fn split_frame(framed: &[u8]) -> Result<(ObjectType, &[u8])> {
    let nul = framed
        .iter()
        .position(|&b| b == 0)
        .context("malformed object: no NUL terminating the header")?;
    let header = std::str::from_utf8(&framed[..nul])
        .context("malformed object: header is not valid UTF-8")?;
    let (type_str, size_str) = header
        .split_once(' ')
        .with_context(|| format!("malformed object header {header:?}: expected '<type> <size>'"))?;

    let kind = ObjectType::parse(type_str)?;
    let declared: usize = size_str
        .parse()
        .with_context(|| format!("malformed object header {header:?}: size is not a number"))?;

    let payload = &framed[nul + 1..];
    if payload.len() != declared {
        bail!(
            "corrupt object: header declares {declared} bytes but body has {}",
            payload.len()
        );
    }
    Ok((kind, payload))
}

// ---------------------------------------------------------------------------
// Trees
// ---------------------------------------------------------------------------

/// The file mode stored in a tree entry.
///
/// Git does not store a full POSIX mode. It stores one of a tiny fixed set,
/// because the only bit it actually cares about preserving is "is this
/// executable". Storing the real mode would make the same file hash differently
/// depending on the developer's umask, which would destroy deduplication across
/// machines for no benefit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileMode {
    Regular,    // 100644
    Executable, // 100755
    Directory,  // 40000  (note: five digits, no leading zero -- see below)
}

impl FileMode {
    /// The ASCII form written into a tree entry.
    ///
    /// Git writes the mode as octal with no zero padding, which is why a
    /// directory is `40000` and not `040000`. Getting this wrong yields trees
    /// that hash differently from real Git's for identical content -- a silent
    /// interop break that only shows up when comparing hashes, which is exactly
    /// what the interop test does.
    pub fn as_str(self) -> &'static str {
        match self {
            FileMode::Regular => "100644",
            FileMode::Executable => "100755",
            FileMode::Directory => "40000",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "100644" => Ok(FileMode::Regular),
            "100755" => Ok(FileMode::Executable),
            "40000" | "040000" => Ok(FileMode::Directory),
            "120000" => bail!("symlinks are not supported by this implementation"),
            "160000" => bail!("gitlinks (submodules) are not supported by this implementation"),
            other => bail!("unsupported file mode {other:?} in tree"),
        }
    }

    pub fn is_dir(self) -> bool {
        matches!(self, FileMode::Directory)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TreeEntry {
    pub mode: FileMode,
    /// A single path component -- never a slash-separated path. Nested
    /// directories become nested tree objects, which is what lets an unchanged
    /// subdirectory be shared by reference between commits.
    pub name: String,
    pub oid: Oid,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Serialize to Git's tree payload: a concatenation of
    /// `<mode> <name>\0<20 raw bytes of SHA-1>` with no separators and no
    /// trailing terminator.
    ///
    /// Note the raw 20-byte OID rather than 40 hex characters. That halves tree
    /// size, but more importantly it is what Git does, and trees must hash
    /// identically to interoperate.
    pub fn serialize(&self) -> Vec<u8> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(tree_sort_key);

        let mut out = Vec::new();
        for entry in &entries {
            out.extend_from_slice(entry.mode.as_str().as_bytes());
            out.push(b' ');
            out.extend_from_slice(entry.name.as_bytes());
            out.push(0);
            out.extend_from_slice(entry.oid.as_bytes());
        }
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Tree> {
        let mut entries = Vec::new();
        let mut pos = 0;
        while pos < payload.len() {
            let space = payload[pos..]
                .iter()
                .position(|&b| b == b' ')
                .context("malformed tree: entry has no space after mode")?
                + pos;
            let nul = payload[space..]
                .iter()
                .position(|&b| b == 0)
                .context("malformed tree: entry has no NUL after name")?
                + space;

            // 20 raw hash bytes follow the NUL. If they are not all present the
            // object was truncated; `split_frame` should have caught that, but a
            // bounds check here keeps the parser total rather than panicking.
            let hash_start = nul + 1;
            let hash_end = hash_start + 20;
            if hash_end > payload.len() {
                bail!("malformed tree: entry is truncated before its 20-byte object id");
            }

            let mode = FileMode::parse(std::str::from_utf8(&payload[pos..space])?)?;
            let name = std::str::from_utf8(&payload[space + 1..nul])
                .context("malformed tree: entry name is not valid UTF-8")?
                .to_string();
            let mut oid = [0u8; 20];
            oid.copy_from_slice(&payload[hash_start..hash_end]);

            entries.push(TreeEntry {
                mode,
                name,
                oid: Oid::from_bytes(oid),
            });
            pos = hash_end;
        }
        Ok(Tree { entries })
    }

    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// Git's tree ordering: plain byte order over the entry name, except that a
/// directory sorts as if its name ended in `/`.
///
/// This looks like a pointless quirk and is worth a comment, because it is the
/// kind of detail that silently breaks interop. Consider entries `lib` (a
/// directory) and `lib.rs` (a file). Naive byte order puts `lib` before
/// `lib.rs`, since `\0` < `.`. Git compares `lib/` against `lib.rs`, and `/`
/// (0x2F) > `.` (0x2E), so Git puts `lib.rs` first. Sorting the naive way
/// produces a tree with the same entries in a different order, therefore a
/// different SHA-1, therefore a commit real Git considers a different tree --
/// and `git fsck` reports "not properly sorted".
fn tree_sort_key(entry: &TreeEntry) -> Vec<u8> {
    let mut key = entry.name.as_bytes().to_vec();
    if entry.mode.is_dir() {
        key.push(b'/');
    }
    key
}

// ---------------------------------------------------------------------------
// Commits
// ---------------------------------------------------------------------------

/// An identity line: `Name <email> <unix-seconds> <+0000>`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub timestamp: i64,
    /// Fixed-format UTC offset such as `+0000`. This implementation always
    /// writes `+0000` and stores timestamps as UTC seconds -- see `Commit`.
    pub tz_offset: String,
}

impl Signature {
    pub fn serialize(&self) -> String {
        format!(
            "{} <{}> {} {}",
            self.name, self.email, self.timestamp, self.tz_offset
        )
    }

    /// Parse `Name Possibly With Spaces <email> 1700000000 +0000`.
    ///
    /// Parsed from the right, because the name is the only free-form field: it
    /// may contain spaces and even angle-bracket-free punctuation, while the
    /// trailing three fields have fixed shape. Splitting from the left would
    /// mis-parse every author with a space in their name.
    pub fn parse(line: &str) -> Result<Signature> {
        let open = line.find('<').context("malformed signature: no '<'")?;
        let close = line.find('>').context("malformed signature: no '>'")?;
        let name = line[..open].trim_end().to_string();
        let email = line[open + 1..close].to_string();
        let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
        if rest.len() != 2 {
            bail!(
                "malformed signature: expected '<timestamp> <tz>', got {:?}",
                &line[close + 1..]
            );
        }
        Ok(Signature {
            name,
            email,
            timestamp: rest[0]
                .parse()
                .context("malformed signature: bad timestamp")?,
            tz_offset: rest[1].to_string(),
        })
    }
}

/// A commit: a snapshot pointer plus provenance.
///
/// A commit does not store a diff. It stores the OID of one tree, which is a
/// complete snapshot of the project at that moment. Diffs are computed on
/// demand by walking two trees. This is the design choice that makes `checkout`
/// O(size of tree) instead of O(history length), at the cost of needing
/// deduplication -- which content addressing supplies for free, since an
/// unchanged file in the new snapshot is literally the same blob OID.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Commit {
    pub tree: Oid,
    /// Zero parents for a root commit, one for a normal commit. The field is a
    /// `Vec` because Git's format allows many (a merge), even though this
    /// implementation never creates more than one -- keeping the shape means a
    /// merge commit written by real Git still round-trips through `cat-file`.
    pub parents: Vec<Oid>,
    pub author: Signature,
    pub committer: Signature,
    pub message: String,
}

impl Commit {
    /// Serialize to Git's commit payload: headers, a blank line, then the
    /// message. Header order matters for byte-identical output: `tree`, then
    /// each `parent`, then `author`, then `committer`.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(&format!("tree {}\n", self.tree));
        for parent in &self.parents {
            out.push_str(&format!("parent {parent}\n"));
        }
        out.push_str(&format!("author {}\n", self.author.serialize()));
        out.push_str(&format!("committer {}\n", self.committer.serialize()));
        out.push('\n');
        out.push_str(&self.message);
        // Git's own commits end with exactly one newline after the message.
        // Normalizing here rather than at the call site means every path that
        // creates a commit produces the same bytes for the same message.
        if !self.message.ends_with('\n') {
            out.push('\n');
        }
        out.into_bytes()
    }

    pub fn parse(payload: &[u8]) -> Result<Commit> {
        let text = std::str::from_utf8(payload).context("malformed commit: not valid UTF-8")?;
        // Headers end at the first blank line. Everything after is the message,
        // verbatim -- including any blank lines inside it.
        let (headers, message) = text
            .split_once("\n\n")
            .context("malformed commit: no blank line separating headers from message")?;

        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;

        for line in headers.lines() {
            let (key, value) = line
                .split_once(' ')
                .with_context(|| format!("malformed commit header line {line:?}"))?;
            match key {
                "tree" => tree = Some(Oid::from_hex(value)?),
                "parent" => parents.push(Oid::from_hex(value)?),
                "author" => author = Some(Signature::parse(value)?),
                "committer" => committer = Some(Signature::parse(value)?),
                // Unknown headers (gpgsig, encoding, mergetag) are skipped
                // rather than rejected, so this parser can read commits written
                // by a full Git install even though it never writes them.
                _ => {}
            }
        }

        Ok(Commit {
            tree: tree.context("malformed commit: missing tree header")?,
            parents,
            author: author.context("malformed commit: missing author header")?,
            committer: committer.context("malformed commit: missing committer header")?,
            message: message.to_string(),
        })
    }

    /// First line of the message, for `log --oneline`-style output.
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical fixture: the SHA-1 of an empty blob. This value is baked
    /// into Git itself, so a test asserting it proves the framing is right
    /// without needing Git installed.
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    #[test]
    fn empty_blob_matches_gits_well_known_hash() {
        let obj = Object::Blob(Vec::new());
        assert_eq!(obj.framed(), b"blob 0\0");
        assert_eq!(obj.oid().to_hex(), EMPTY_BLOB);
    }

    #[test]
    fn hello_blob_matches_gits_well_known_hash() {
        // `printf 'hello\n' | git hash-object --stdin`
        let obj = Object::Blob(b"hello\n".to_vec());
        assert_eq!(obj.framed(), b"blob 6\0hello\n");
        assert_eq!(
            obj.oid().to_hex(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn header_is_inside_the_hash_so_types_never_collide() {
        // Same payload bytes, different type => different name.
        let blob = Object::Blob(Vec::new());
        let tree = Object::Tree(Tree::default());
        assert_eq!(blob.payload(), tree.payload());
        assert_ne!(blob.oid(), tree.oid());
    }

    #[test]
    fn frame_roundtrips_through_split() {
        let framed = frame(ObjectType::Blob, b"some content");
        let (kind, payload) = split_frame(&framed).unwrap();
        assert_eq!(kind, ObjectType::Blob);
        assert_eq!(payload, b"some content");
    }

    #[test]
    fn split_frame_rejects_size_mismatch() {
        // Header claims 99 bytes, body has 4. This is what a truncated read of
        // a loose object file looks like.
        let err = split_frame(b"blob 99\0abcd").unwrap_err().to_string();
        assert!(err.contains("declares 99"), "unexpected error: {err}");
    }

    #[test]
    fn split_frame_rejects_missing_nul() {
        assert!(split_frame(b"blob 4abcd").is_err());
    }

    #[test]
    fn tree_entry_layout_is_mode_space_name_nul_rawoid() {
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let tree = Tree {
            entries: vec![TreeEntry {
                mode: FileMode::Regular,
                name: "a.txt".into(),
                oid,
            }],
        };
        let bytes = tree.serialize();
        assert_eq!(&bytes[..6], b"100644");
        assert_eq!(bytes[6], b' ');
        assert_eq!(&bytes[7..12], b"a.txt");
        assert_eq!(bytes[12], 0);
        // Raw 20 bytes, not 40 hex characters.
        assert_eq!(&bytes[13..33], oid.as_bytes());
        assert_eq!(bytes.len(), 33);
    }

    #[test]
    fn tree_roundtrips() {
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let tree = Tree {
            entries: vec![
                TreeEntry {
                    mode: FileMode::Regular,
                    name: "a.txt".into(),
                    oid,
                },
                TreeEntry {
                    mode: FileMode::Executable,
                    name: "run.sh".into(),
                    oid,
                },
                TreeEntry {
                    mode: FileMode::Directory,
                    name: "src".into(),
                    oid,
                },
            ],
        };
        let parsed = Tree::parse(&tree.serialize()).unwrap();
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[2].mode, FileMode::Directory);
        assert_eq!(parsed.entries[1].name, "run.sh");
    }

    #[test]
    fn directories_sort_as_if_they_ended_in_slash() {
        // The `lib` vs `lib.rs` case from `tree_sort_key`'s comment. Real Git
        // orders `lib.rs` before `lib/`, because '/' (0x2F) > '.' (0x2E).
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let tree = Tree {
            entries: vec![
                TreeEntry {
                    mode: FileMode::Directory,
                    name: "lib".into(),
                    oid,
                },
                TreeEntry {
                    mode: FileMode::Regular,
                    name: "lib.rs".into(),
                    oid,
                },
            ],
        };
        let parsed = Tree::parse(&tree.serialize()).unwrap();
        let names: Vec<&str> = parsed.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["lib.rs", "lib"]);
    }

    #[test]
    fn tree_mode_is_unpadded_octal_for_directories() {
        assert_eq!(FileMode::Directory.as_str(), "40000");
        // ...but a tree written by real Git with a padded mode still parses.
        assert_eq!(FileMode::parse("040000").unwrap(), FileMode::Directory);
    }

    #[test]
    fn tree_parse_rejects_truncated_oid() {
        let mut bytes = b"100644 a.txt\0".to_vec();
        bytes.extend_from_slice(&[0u8; 10]); // only half an OID
        assert!(Tree::parse(&bytes).is_err());
    }

    fn sig(name: &str) -> Signature {
        Signature {
            name: name.into(),
            email: "dev@example.com".into(),
            timestamp: 1_700_000_000,
            tz_offset: "+0000".into(),
        }
    }

    #[test]
    fn commit_roundtrips_with_parent() {
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let commit = Commit {
            tree: oid,
            parents: vec![oid],
            author: sig("Ada Lovelace"),
            committer: sig("Ada Lovelace"),
            message: "second commit\n".into(),
        };
        let parsed = Commit::parse(&commit.serialize()).unwrap();
        assert_eq!(parsed, commit);
    }

    #[test]
    fn commit_header_order_is_tree_parents_author_committer() {
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let commit = Commit {
            tree: oid,
            parents: vec![oid],
            author: sig("Ada"),
            committer: sig("Ada"),
            message: "msg".into(),
        };
        let text = String::from_utf8(commit.serialize()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("tree "));
        assert!(lines[1].starts_with("parent "));
        assert!(lines[2].starts_with("author "));
        assert!(lines[3].starts_with("committer "));
        assert_eq!(lines[4], "");
    }

    #[test]
    fn commit_message_always_ends_in_exactly_one_newline() {
        let oid = Oid::from_hex(EMPTY_BLOB).unwrap();
        let mk = |msg: &str| {
            let c = Commit {
                tree: oid,
                parents: vec![],
                author: sig("Ada"),
                committer: sig("Ada"),
                message: msg.into(),
            };
            String::from_utf8(c.serialize()).unwrap()
        };
        assert!(mk("hi").ends_with("\nhi\n"));
        assert!(mk("hi\n").ends_with("\nhi\n"));
    }

    #[test]
    fn signature_parses_names_containing_spaces() {
        let s = Signature::parse("Ada Lovelace <ada@example.com> 1700000000 -0400").unwrap();
        assert_eq!(s.name, "Ada Lovelace");
        assert_eq!(s.email, "ada@example.com");
        assert_eq!(s.timestamp, 1_700_000_000);
        assert_eq!(s.tz_offset, "-0400");
    }

    #[test]
    fn commit_parse_ignores_headers_it_does_not_model() {
        // Real Git writes `gpgsig` and `encoding` headers. Skipping unknown
        // headers is what lets this parser read a signed commit.
        let payload = format!(
            "tree {EMPTY_BLOB}\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n\
             encoding ISO-8859-1\n\
             author Ada <a@e.com> 1700000000 +0000\n\
             committer Ada <a@e.com> 1700000000 +0000\n\
             \n\
             signed commit\n"
        );
        let commit = Commit::parse(payload.as_bytes()).unwrap();
        assert_eq!(commit.summary(), "signed commit");
    }

    #[test]
    fn deserialize_dispatches_on_the_header_type() {
        let tree = Object::Tree(Tree::default());
        assert!(matches!(
            Object::deserialize(&tree.framed()).unwrap(),
            Object::Tree(_)
        ));
        let blob = Object::Blob(b"x".to_vec());
        assert!(matches!(
            Object::deserialize(&blob.framed()).unwrap(),
            Object::Blob(_)
        ));
    }
}
