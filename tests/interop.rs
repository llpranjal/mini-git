//! Interoperability with real Git, proven in both directions.
//!
//! This is the headline claim of the project, so the tests here are written to
//! be hard to fool. In particular they avoid the trap of "my tool agrees with
//! itself": every assertion either compares against a hash real `git` computed,
//! or asks real `git` to read something only this tool wrote.
//!
//! The three levels of evidence, weakest to strongest:
//!
//! 1. `git` can read individual objects `mgit` wrote (`git cat-file`).
//! 2. `git` accepts an entire repository `mgit` built (`git log`, `git fsck`,
//!    `git status`) --- this checks the tree ordering, ref files, and commit
//!    headers together, not just one object at a time.
//! 3. Given identical content, identity, and timestamp, both programs produce
//!    the *same commit SHA-1*. Nothing about the format can differ at all for
//!    that to hold: not the header, not the tree layout, not the entry sort
//!    order, not a single byte of the commit payload.

mod common;

use common::*;
use mini_git::scratch::ScratchDir;
use std::path::Path;

/// Build a small repository with `mgit`: two directories, an executable, and a
/// second commit that modifies one file.
fn build_with_mgit(dir: &Path) {
    mgit(dir, &["init", "."]);
    std::fs::write(dir.join("README.md"), "# project\n\nhello\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("docs/notes.md"), "notes\n").unwrap();
    mgit(dir, &["add", "."]);
    mgit(dir, &["commit", "-m", "initial commit"]);

    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"bye\");\n}\n",
    )
    .unwrap();
    mgit(dir, &["add", "src/main.rs"]);
    mgit(dir, &["commit", "-m", "change the greeting"]);
}

// ---------------------------------------------------------------------------
// Direction 1: real git reads what mgit wrote
// ---------------------------------------------------------------------------

#[test]
fn real_git_reads_a_blob_written_by_mgit() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-blob");
    mgit(dir.path(), &["init", "."]);

    let content = "content addressed\nby sha1\n";
    std::fs::write(dir.path().join("f.txt"), content).unwrap();
    let oid = mgit(dir.path(), &["hash-object", "-w", "f.txt"])
        .trim()
        .to_string();

    // Real git independently computes the same name...
    let git_oid = git(dir.path(), &["hash-object", "f.txt"])
        .trim()
        .to_string();
    assert_eq!(oid, git_oid, "mgit and git disagree on the blob's name");

    // ...and can decompress and print the object mgit stored.
    assert_eq!(git(dir.path(), &["cat-file", "-p", &oid]), content);
    assert_eq!(git(dir.path(), &["cat-file", "-t", &oid]).trim(), "blob");
    assert_eq!(
        git(dir.path(), &["cat-file", "-s", &oid]).trim(),
        content.len().to_string()
    );
}

#[test]
fn real_git_walks_an_entire_repository_built_by_mgit() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-repo");
    build_with_mgit(dir.path());

    // `git log` requires HEAD, the ref file, both commit objects, and the
    // parent link between them all to be correct.
    let log = git(dir.path(), &["log", "--oneline"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "git log gave:\n{log}");
    assert!(lines[0].contains("change the greeting"));
    assert!(lines[1].contains("initial commit"));

    // `git ls-tree -r` requires every tree object, including the nested ones,
    // to parse and to reference blobs that exist.
    let tree = git(dir.path(), &["ls-tree", "-r", "HEAD"]);
    assert!(tree.contains("README.md"), "{tree}");
    assert!(tree.contains("src/main.rs"), "{tree}");
    assert!(tree.contains("docs/notes.md"), "{tree}");

    // `git show` reconstructs a diff between two commits mgit created.
    let show = git(dir.path(), &["show", "--format=%s", "HEAD"]);
    assert!(show.contains("change the greeting"), "{show}");
    assert!(show.contains("-    println!(\"hi\");"), "{show}");
    assert!(show.contains("+    println!(\"bye\");"), "{show}");
}

#[test]
fn git_fsck_finds_no_corruption_in_a_repository_built_by_mgit() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-fsck");
    build_with_mgit(dir.path());

    // This is the strictest single check available. `git fsck` verifies every
    // object's hash, parses every tree, and checks tree entry sort order --
    // which is exactly where an implementation that "looks right" fails, since
    // a mis-sorted tree still round-trips through its own parser.
    let out = git_raw(dir.path(), &["fsck", "--strict", "--no-progress"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "git fsck failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    for stream in [&stdout, &stderr] {
        for line in stream.lines() {
            // "dangling" is informational: it reports objects no ref points at.
            // Everything else fsck prints is a defect.
            assert!(
                line.starts_with("dangling") || line.trim().is_empty(),
                "git fsck reported a problem: {line}"
            );
        }
    }
}

#[test]
fn git_status_is_clean_in_a_repository_mgit_just_committed() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-status");
    build_with_mgit(dir.path());

    // `git read-tree HEAD` is required first, and the reason is the one
    // deliberate deviation in this project: mgit stores its index at
    // `.git/mini-index`, so a repository it built has no `.git/index` at all.
    // Without this step git compares against an empty index and reports every
    // file as both deleted and untracked. That is not a defect in the objects,
    // and the test says so rather than papering over it.
    //
    // What the step does is populate git's index directly from the tree object
    // mgit wrote. Everything after it is a genuine check: git re-hashes every
    // file on disk and compares against that tree.
    git(dir.path(), &["read-tree", "HEAD"]);

    // Clean means git agrees with mgit on the content and the mode of every
    // path, having derived its expectations entirely from mgit's objects.
    let status = git(dir.path(), &["status", "--porcelain"]);
    assert_eq!(status, "", "git sees differences mgit does not:\n{status}");

    // And the reverse direction of the same idea: ask git to diff its
    // freshly-built index against the working tree. Empty output means every
    // blob mgit stored decompresses to exactly the bytes on disk.
    let diff = git(dir.path(), &["diff", "--stat"]);
    assert_eq!(diff, "", "git diff found content differences:\n{diff}");
}

#[test]
fn git_reads_the_executable_bit_mgit_recorded() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-mode");
    mgit(dir.path(), &["init", "."]);
    std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::write(dir.path().join("plain.txt"), "text\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    mgit(dir.path(), &["add", "."]);
    mgit(dir.path(), &["commit", "-m", "modes"]);

    let tree = git(dir.path(), &["ls-tree", "HEAD"]);
    assert!(
        tree.contains("100755 blob"),
        "run.sh should be executable:\n{tree}"
    );
    assert!(
        tree.contains("100644 blob"),
        "plain.txt should not be:\n{tree}"
    );
}

// ---------------------------------------------------------------------------
// Direction 2: mgit reads what real git wrote
// ---------------------------------------------------------------------------

#[test]
fn mgit_reads_blobs_trees_and_commits_written_by_real_git() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-reverse");
    git(dir.path(), &["init", "-q", "-b", "main", "."]);

    std::fs::write(dir.path().join("a.txt"), "written by real git\n").unwrap();
    std::fs::create_dir_all(dir.path().join("nested/deep")).unwrap();
    std::fs::write(dir.path().join("nested/deep/b.txt"), "deep file\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "git made this"]);

    // Blob, by the hash git reported.
    let blob = git(dir.path(), &["rev-parse", "HEAD:a.txt"])
        .trim()
        .to_string();
    assert_eq!(
        String::from_utf8(mgit_bytes(dir.path(), &["cat-file", "-p", &blob])).unwrap(),
        "written by real git\n"
    );
    assert_eq!(mgit(dir.path(), &["cat-file", "-t", &blob]).trim(), "blob");

    // Tree, including the nested one.
    let tree = git(dir.path(), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    assert_eq!(mgit(dir.path(), &["cat-file", "-t", &tree]).trim(), "tree");
    let listing = mgit(dir.path(), &["cat-file", "-p", &tree]);
    assert!(listing.contains("a.txt"), "{listing}");
    assert!(listing.contains("040000 tree"), "{listing}");

    // Commit, and the whole history from git's ref files.
    let commit = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(
        mgit(dir.path(), &["cat-file", "-t", &commit]).trim(),
        "commit"
    );
    let log = mgit(dir.path(), &["log", "--oneline"]);
    assert!(
        log.contains("git made this"),
        "mgit log on a git repo gave:\n{log}"
    );
}

#[test]
fn mgit_cat_file_reproduces_git_cat_file_byte_for_byte() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-bytes");
    git(dir.path(), &["init", "-q", "-b", "main", "."]);

    // Includes a file with no trailing newline and one with binary content,
    // because those are where a naive text-oriented reader diverges.
    std::fs::write(dir.path().join("text.txt"), "line one\nline two\n").unwrap();
    std::fs::write(dir.path().join("nonewline.txt"), "no trailing newline").unwrap();
    std::fs::write(
        dir.path().join("binary.bin"),
        [0u8, 1, 2, 255, 254, 0, 13, 10],
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "mixed content"]);

    for file in ["text.txt", "nonewline.txt", "binary.bin"] {
        let oid = git(dir.path(), &["rev-parse", &format!("HEAD:{file}")])
            .trim()
            .to_string();
        assert_eq!(
            mgit_bytes(dir.path(), &["cat-file", "-p", &oid]),
            git_bytes(dir.path(), &["cat-file", "-p", &oid]),
            "output differs for {file}"
        );
    }
}

#[test]
fn mgit_reads_a_commit_after_git_amends_and_re_commits() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("interop-chain");
    git(dir.path(), &["init", "-q", "-b", "main", "."]);
    std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.path().join("f.txt"), "two\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "second"]);

    // Walking a two-commit chain requires parsing git's commit headers and
    // following the parent link.
    let log = mgit(dir.path(), &["log", "--oneline"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].contains("second"));
    assert!(lines[1].contains("first"));
}

// ---------------------------------------------------------------------------
// Direction 3: the two agree exactly
// ---------------------------------------------------------------------------

/// Create identical content in two directories and commit it with the two
/// different tools, then compare the resulting object ids.
#[test]
fn mgit_and_git_produce_identical_blob_tree_and_commit_hashes() {
    if !require_git() {
        return;
    }
    let mine = ScratchDir::new("agree-mgit");
    let theirs = ScratchDir::new("agree-git");

    // Content chosen to exercise the details that are easy to get wrong:
    // a nested directory, an empty file, a file whose name sorts adjacent to a
    // directory name (`lib` vs `lib.rs` -- the tree sort-order trap), and a
    // file with no trailing newline.
    let files: &[(&str, &str)] = &[
        ("README.md", "# title\n"),
        ("empty.txt", ""),
        ("lib.rs", "pub fn f() {}\n"),
        ("lib/mod.rs", "// nested\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("zz.txt", "no trailing newline"),
    ];

    for dir in [mine.path(), theirs.path()] {
        for (path, content) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
    }

    mgit(mine.path(), &["init", "."]);
    mgit(mine.path(), &["add", "."]);
    mgit(mine.path(), &["commit", "-m", "identical snapshot"]);

    git(theirs.path(), &["init", "-q", "-b", "main", "."]);
    git(theirs.path(), &["add", "."]);
    git(theirs.path(), &["commit", "-q", "-m", "identical snapshot"]);

    // Blobs: content addressing means these must match trivially.
    for (path, _) in files {
        let a = git(mine.path(), &["rev-parse", &format!("HEAD:{path}")])
            .trim()
            .to_string();
        let b = git(theirs.path(), &["rev-parse", &format!("HEAD:{path}")])
            .trim()
            .to_string();
        assert_eq!(a, b, "blob hash differs for {path}");
    }

    // Root tree: this is the real test. It matches only if the entry framing,
    // the raw-20-byte object ids, the unpadded directory mode, and Git's
    // "directories sort as if they end in /" rule are all exactly right.
    let my_tree = git(mine.path(), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    let git_tree = git(theirs.path(), &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    assert_eq!(my_tree, git_tree, "root tree hash differs");

    // Commit: matches only if the header order, the identity lines, the
    // timestamp format, and the blank-line-then-message layout are byte-exact.
    // Both sides used the same fixed author, committer, and date from the
    // environment, so any remaining difference is a format difference.
    let my_commit = git(mine.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let git_commit = git(theirs.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(my_commit, git_commit, "commit hash differs");

    // And the stored bytes themselves, not merely the names.
    assert_eq!(
        git_bytes(mine.path(), &["cat-file", "commit", &my_commit]),
        git_bytes(theirs.path(), &["cat-file", "commit", &git_commit]),
    );
}

#[test]
fn the_two_tools_agree_on_the_empty_tree_and_empty_blob() {
    if !require_git() {
        return;
    }
    let dir = ScratchDir::new("agree-empties");
    mgit(dir.path(), &["init", "."]);

    // These two ids are constants in every Git repository ever created. If the
    // framing were off by a single byte, neither would match.
    std::fs::write(dir.path().join("empty"), "").unwrap();
    assert_eq!(
        mgit(dir.path(), &["hash-object", "empty"]).trim(),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    );
    assert_eq!(
        git(dir.path(), &["hash-object", "empty"]).trim(),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    );

    // The empty tree, via a commit of an empty index.
    mgit(dir.path(), &["init", "."]);
    let out = mgit_raw(dir.path(), &["commit", "-m", "empty"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        git(dir.path(), &["rev-parse", "HEAD^{tree}"]).trim(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
}

#[test]
fn each_tool_can_read_the_others_loose_object_file_verbatim() {
    if !require_git() {
        return;
    }
    let mine = ScratchDir::new("bytes-mgit");
    let theirs = ScratchDir::new("bytes-git");
    let content = "the compressed file on disk should be mutually readable\n";

    mgit(mine.path(), &["init", "."]);
    std::fs::write(mine.path().join("f.txt"), content).unwrap();
    let oid = mgit(mine.path(), &["hash-object", "-w", "f.txt"])
        .trim()
        .to_string();

    git(theirs.path(), &["init", "-q", "-b", "main", "."]);
    std::fs::write(theirs.path().join("f.txt"), content).unwrap();
    git(theirs.path(), &["hash-object", "-w", "f.txt"]);

    let loose = |root: &Path| root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    let mine_bytes = std::fs::read(loose(mine.path())).unwrap();
    let theirs_bytes = std::fs::read(loose(theirs.path())).unwrap();

    // Both are zlib streams at the same nominal level: 0x78 0x01 is the RFC 1950
    // header for level 1, which is git's `core.loosecompression` default and the
    // level this store deliberately matches.
    assert_eq!(
        &mine_bytes[..2],
        &[0x78, 0x01],
        "mgit did not write a level-1 stream"
    );
    assert_eq!(
        &theirs_bytes[..2],
        &[0x78, 0x01],
        "git did not write a level-1 stream"
    );

    // The compressed bytes are *not* asserted to be equal, and it is worth being
    // precise about why rather than quietly omitting the check.
    //
    // flate2 defaults to the miniz_oxide backend, a pure-Rust deflate. C zlib
    // and miniz_oxide make different internal choices at the same nominal level,
    // so the two encoders emit different (both valid, both level-1) streams for
    // the same input. Making the files byte-identical would mean linking C zlib
    // via flate2's "zlib" feature, which pulls in libz-sys and a C toolchain --
    // a real cost for a property nothing needs.
    //
    // Nothing needs it because compression is not part of an object's identity.
    // The name is the SHA-1 of the *uncompressed* framed bytes, so any encoder
    // that produces a valid zlib stream interoperates. What actually has to hold
    // is that each tool can decode the other's file, and that is asserted below
    // by physically swapping the object files between the two repositories --
    // a stricter check than comparing bytes, because it exercises the decoders.
    // Overwrite via remove-then-write rather than `fs::copy`: git marks loose
    // object files read-only (0444), since a content-addressed file has no
    // legitimate reason to ever be modified in place.
    let replace = |path: std::path::PathBuf, bytes: &[u8]| {
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, bytes).unwrap();
    };

    replace(loose(mine.path()), &theirs_bytes);
    assert_eq!(
        String::from_utf8(mgit_bytes(mine.path(), &["cat-file", "-p", &oid])).unwrap(),
        content,
        "mgit could not read the loose object file git wrote"
    );

    replace(loose(theirs.path()), &mine_bytes);
    assert_eq!(
        git(theirs.path(), &["cat-file", "-p", &oid]),
        content,
        "git could not read the loose object file mgit wrote"
    );

    // And git independently agrees the file it just read is not corrupt --
    // meaning it decompressed the stream and re-hashed the contents to `oid`.
    let fsck = git_raw(theirs.path(), &["fsck", "--strict", "--no-progress"]);
    assert!(
        fsck.status.success(),
        "git fsck rejected mgit's object file: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}
