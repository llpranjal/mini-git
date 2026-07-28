//! Object IDs.
//!
//! An `Oid` is the SHA-1 of an object's *uncompressed* framed bytes. It is the
//! only name an object ever has. Everything in the repository -- trees, commits,
//! refs, the index -- refers to content through one of these 20-byte values.
//!
//! Why a newtype around `[u8; 20]` instead of passing `String` hex around:
//! the binary form is what gets written into tree objects (trees store raw
//! 20-byte IDs, not hex), and a distinct type makes it impossible to
//! accidentally write 40 hex characters where 20 raw bytes belong. That exact
//! bug would produce trees that real Git rejects, and it would be invisible
//! until an interop test failed.

use anyhow::{bail, Result};
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid([u8; 20]);

impl Oid {
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        Oid(bytes)
    }

    /// Parse 40 lowercase-or-uppercase hex characters into an Oid.
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() != 40 {
            bail!(
                "not a valid object name: {hex:?} (expected 40 hex chars, got {})",
                hex.len()
            );
        }
        let mut out = [0u8; 20];
        for (i, byte) in out.iter_mut().enumerate() {
            let slice = &hex[i * 2..i * 2 + 2];
            *byte = u8::from_str_radix(slice, 16)
                .map_err(|_| anyhow::anyhow!("not a valid object name: {hex:?}"))?;
        }
        Ok(Oid(out))
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(40);
        for b in self.0 {
            // `format!` in a loop is slower than this, but object IDs are
            // stringified rarely (display, ref files) and never in a hot path.
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Loose objects live at `.git/objects/ab/cdef...` -- the first byte of the
    /// hash becomes a directory. Git does this because some filesystems degrade
    /// badly with hundreds of thousands of entries in one directory; 256 evenly
    /// filled buckets keeps each directory listing small.
    pub fn path_parts(self) -> (String, String) {
        let hex = self.to_hex();
        (hex[..2].to_string(), hex[2..].to_string())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let hex = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let oid = Oid::from_hex(hex).unwrap();
        assert_eq!(oid.to_hex(), hex);
    }

    #[test]
    fn hex_is_case_insensitive_on_input_and_lowercase_on_output() {
        let oid = Oid::from_hex("E69DE29BB2D1D6434B8B29AE775AD8C2E48C5391").unwrap();
        assert_eq!(oid.to_hex(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn rejects_wrong_length_and_non_hex() {
        assert!(Oid::from_hex("abc").is_err());
        assert!(Oid::from_hex(&"z".repeat(40)).is_err());
    }

    #[test]
    fn fanout_path_splits_after_two_chars() {
        let oid = Oid::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let (dir, file) = oid.path_parts();
        assert_eq!(dir, "e6");
        assert_eq!(file, "9de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(file.len(), 38);
    }
}
