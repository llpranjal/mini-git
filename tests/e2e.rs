//! End-to-end tests: run the real binary in a real temporary directory.
//!
//! These cover the porcelain --- the behaviour a user sees --- while the unit
//! tests in `src/` cover the formats and algorithms underneath. Splitting them
//! that way means a formatting change breaks one test here, not thirty tests
//! everywhere, and a format bug is caught by a fast unit test rather than by an
//! end-to-end failure that has to be bisected.

mod common;

use common::*;
use mini_git::scratch::ScratchDir;
use std::path::Path;

/// A repository with one commit containing `a.txt` and `src/main.rs`.
fn repo_with_one_commit(label: &str) -> ScratchDir {
    let dir = ScratchDir::new(label);
    mgit(dir.path(), &["init", "."]);
    dir.write("a.txt", "alpha\n");
    dir.write("src/main.rs", "fn main() {}\n");
    mgit(dir.path(), &["add", "."]);
    mgit(dir.path(), &["commit", "-m", "first"]);
    dir
}

fn head_oid(dir: &Path) -> String {
    let log = mgit(dir, &["log", "--oneline"]);
    log.lines()
        .next()
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn init_creates_the_expected_layout() {
    let dir = ScratchDir::new("e2e-init");
    let out = mgit(dir.path(), &["init", "."]);
    assert!(out.contains("Initialized empty"), "{out}");

    assert!(dir.path().join(".git/objects").is_dir());
    assert!(dir.path().join(".git/refs/heads").is_dir());
    assert_eq!(dir.read(".git/HEAD"), "ref: refs/heads/main\n");
    // No ref file until the first commit.
    assert!(!dir.path().join(".git/refs/heads/main").exists());
}

#[test]
fn commands_outside_a_repository_fail_with_a_clear_message() {
    let dir = ScratchDir::new("e2e-norepo");
    let err = mgit_err(dir.path(), &["status"]);
    assert!(err.contains("not a mini-git repository"), "{err}");
}

#[test]
fn commands_work_from_a_subdirectory() {
    let dir = repo_with_one_commit("e2e-subdir");
    let log = mgit(&dir.path().join("src"), &["log", "--oneline"]);
    assert!(log.contains("first"), "{log}");
}

// ---------------------------------------------------------------------------
// hash-object / cat-file
// ---------------------------------------------------------------------------

#[test]
fn hash_object_without_w_writes_nothing() {
    let dir = ScratchDir::new("e2e-hash");
    mgit(dir.path(), &["init", "."]);
    dir.write("f.txt", "hello\n");

    let oid = mgit(dir.path(), &["hash-object", "f.txt"])
        .trim()
        .to_string();
    assert_eq!(oid, "ce013625030ba8dba906f756967f9e9ca394464a");
    assert!(
        !dir.path().join(".git/objects/ce").exists(),
        "hashing must be a pure function with no side effects"
    );

    mgit(dir.path(), &["hash-object", "-w", "f.txt"]);
    assert!(dir.path().join(".git/objects/ce").exists());
}

#[test]
fn hash_object_reads_stdin() {
    use std::io::Write;
    let dir = ScratchDir::new("e2e-stdin");
    mgit(dir.path(), &["init", "."]);

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mgit"))
        .current_dir(dir.path())
        .args(["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"hello\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
}

#[test]
fn cat_file_accepts_an_abbreviated_hash_and_rejects_an_ambiguous_one() {
    let dir = repo_with_one_commit("e2e-abbrev");
    let full = mgit(dir.path(), &["hash-object", "a.txt"])
        .trim()
        .to_string();

    assert_eq!(mgit(dir.path(), &["cat-file", "-p", &full[..8]]), "alpha\n");
    let err = mgit_err(dir.path(), &["cat-file", "-p", &full[..2]]);
    assert!(err.contains("too short"), "{err}");
}

#[test]
fn cat_file_reports_type_and_size() {
    let dir = repo_with_one_commit("e2e-catfile");
    let head = head_oid(dir.path());
    assert_eq!(
        mgit(dir.path(), &["cat-file", "-t", &head]).trim(),
        "commit"
    );

    let blob = mgit(dir.path(), &["hash-object", "a.txt"])
        .trim()
        .to_string();
    assert_eq!(mgit(dir.path(), &["cat-file", "-t", &blob]).trim(), "blob");
    assert_eq!(mgit(dir.path(), &["cat-file", "-s", &blob]).trim(), "6");
}

#[test]
fn cat_file_pretty_prints_a_tree_as_readable_lines() {
    let dir = repo_with_one_commit("e2e-tree");
    let head = head_oid(dir.path());
    let commit = mgit(dir.path(), &["cat-file", "-p", &head]);
    let tree = commit
        .lines()
        .find_map(|l| l.strip_prefix("tree "))
        .expect("commit should have a tree header")
        .to_string();

    let listing = mgit(dir.path(), &["cat-file", "-p", &tree]);
    assert!(listing.contains("100644 blob"), "{listing}");
    assert!(listing.contains("\ta.txt"), "{listing}");
    // `src` is a nested tree, not a path with a slash in it.
    assert!(listing.contains("040000 tree"), "{listing}");
    assert!(listing.contains("\tsrc"), "{listing}");
    assert!(!listing.contains("src/main.rs"), "{listing}");
}

// ---------------------------------------------------------------------------
// add / status / commit
// ---------------------------------------------------------------------------

#[test]
fn status_moves_a_file_through_untracked_then_staged_then_clean() {
    let dir = ScratchDir::new("e2e-status");
    mgit(dir.path(), &["init", "."]);
    dir.write("f.txt", "one\n");

    let untracked = mgit(dir.path(), &["status"]);
    assert!(untracked.contains("Untracked files:"), "{untracked}");
    assert!(untracked.contains("f.txt"), "{untracked}");

    mgit(dir.path(), &["add", "f.txt"]);
    let staged = mgit(dir.path(), &["status"]);
    assert!(staged.contains("Changes to be committed:"), "{staged}");
    assert!(staged.contains("new file:   f.txt"), "{staged}");

    mgit(dir.path(), &["commit", "-m", "add f"]);
    let clean = mgit(dir.path(), &["status"]);
    assert!(
        clean.contains("nothing to commit, working tree clean"),
        "{clean}"
    );
}

#[test]
fn a_file_can_be_staged_and_modified_at_the_same_time() {
    // The behaviour that only exists because the index is a separate state.
    let dir = repo_with_one_commit("e2e-both");
    dir.write("a.txt", "staged version\n");
    mgit(dir.path(), &["add", "a.txt"]);
    dir.write("a.txt", "working version\n");

    let status = mgit(dir.path(), &["status"]);
    assert!(status.contains("Changes to be committed:"), "{status}");
    assert!(
        status.contains("Changes not staged for commit:"),
        "{status}"
    );

    // And committing takes the staged version, not what is on disk.
    mgit(dir.path(), &["commit", "-m", "second"]);
    let committed = mgit(dir.path(), &["cat-file", "-p", "HEAD"]);
    let tree = committed
        .lines()
        .find_map(|l| l.strip_prefix("tree "))
        .unwrap();
    let listing = mgit(dir.path(), &["cat-file", "-p", tree]);
    let blob = listing
        .lines()
        .find(|l| l.ends_with("a.txt"))
        .unwrap()
        .split_whitespace()
        .nth(2)
        .unwrap()
        .to_string();
    assert_eq!(
        mgit(dir.path(), &["cat-file", "-p", &blob]),
        "staged version\n"
    );
}

#[test]
fn staging_a_deleted_file_records_the_deletion() {
    let dir = repo_with_one_commit("e2e-delete");
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();

    let before = mgit(dir.path(), &["status"]);
    assert!(before.contains("deleted:    a.txt"), "{before}");

    mgit(dir.path(), &["add", "a.txt"]);
    mgit(dir.path(), &["commit", "-m", "remove a"]);

    let listing = mgit(dir.path(), &["cat-file", "-p", "HEAD"]);
    let tree = listing
        .lines()
        .find_map(|l| l.strip_prefix("tree "))
        .unwrap();
    assert!(!mgit(dir.path(), &["cat-file", "-p", tree]).contains("a.txt"));
}

#[test]
fn commit_refuses_an_empty_message_and_an_unchanged_tree() {
    let dir = repo_with_one_commit("e2e-refuse");

    let err = mgit_err(dir.path(), &["commit", "-m", "   "]);
    assert!(err.contains("empty commit message"), "{err}");

    let err = mgit_err(dir.path(), &["commit", "-m", "no changes"]);
    assert!(err.contains("nothing to commit"), "{err}");
}

#[test]
fn adding_a_missing_path_fails_rather_than_doing_nothing() {
    let dir = repo_with_one_commit("e2e-missing");
    let err = mgit_err(dir.path(), &["add", "nope.txt"]);
    assert!(err.contains("did not match any file"), "{err}");
}

#[test]
fn identical_content_in_two_files_is_stored_once() {
    // Deduplication, observed from the outside.
    let dir = ScratchDir::new("e2e-dedup");
    mgit(dir.path(), &["init", "."]);
    dir.write("one.txt", "identical bytes\n");
    dir.write("two.txt", "identical bytes\n");
    dir.write("nested/three.txt", "identical bytes\n");
    mgit(dir.path(), &["add", "."]);

    let a = mgit(dir.path(), &["hash-object", "one.txt"])
        .trim()
        .to_string();
    let b = mgit(dir.path(), &["hash-object", "two.txt"])
        .trim()
        .to_string();
    assert_eq!(a, b);

    // Three files, one blob object on disk.
    let fanout = dir.path().join(".git/objects").join(&a[..2]);
    let count = std::fs::read_dir(&fanout).unwrap().count();
    assert_eq!(
        count,
        1,
        "expected exactly one object in the {} fanout",
        &a[..2]
    );
}

// ---------------------------------------------------------------------------
// log
// ---------------------------------------------------------------------------

#[test]
fn log_shows_commits_newest_first_and_honours_a_limit() {
    let dir = ScratchDir::new("e2e-log");
    mgit(dir.path(), &["init", "."]);
    for i in 1..=3 {
        dir.write("f.txt", format!("version {i}\n"));
        mgit(dir.path(), &["add", "f.txt"]);
        mgit(dir.path(), &["commit", "-m", &format!("commit {i}")]);
    }

    let log = mgit(dir.path(), &["log", "--oneline"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("commit 3"));
    assert!(lines[2].contains("commit 1"));

    let limited = mgit(dir.path(), &["log", "--oneline", "-n", "2"]);
    assert_eq!(limited.lines().count(), 2);
}

#[test]
fn log_on_an_empty_repository_says_so_instead_of_failing() {
    let dir = ScratchDir::new("e2e-emptylog");
    mgit(dir.path(), &["init", "."]);
    assert!(mgit(dir.path(), &["log"]).contains("no commits yet"));
}

#[test]
fn log_shows_the_parent_link_between_commits() {
    let dir = repo_with_one_commit("e2e-parent");
    let first = head_oid(dir.path());
    dir.write("a.txt", "changed\n");
    mgit(dir.path(), &["add", "a.txt"]);
    mgit(dir.path(), &["commit", "-m", "second"]);

    let log = mgit(dir.path(), &["log"]);
    assert!(
        log.contains(&format!("parent {first}")),
        "abbreviated parent in:\n{log}"
    );
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

#[test]
fn diff_shows_unstaged_edits_and_staged_edits_separately() {
    let dir = repo_with_one_commit("e2e-diff");
    dir.write("a.txt", "alpha\nbeta\n");

    let unstaged = mgit(dir.path(), &["diff"]);
    assert!(unstaged.contains("+beta"), "{unstaged}");
    // Nothing is staged yet, so --staged must be empty.
    assert!(mgit(dir.path(), &["diff", "--staged"]).contains("no differences"));

    mgit(dir.path(), &["add", "a.txt"]);
    assert!(mgit(dir.path(), &["diff"]).contains("no differences"));
    let staged = mgit(dir.path(), &["diff", "--staged"]);
    assert!(staged.contains("+beta"), "{staged}");
}

#[test]
fn diff_against_a_commit_covers_everything_since_it() {
    let dir = repo_with_one_commit("e2e-diffrev");
    let first = head_oid(dir.path());
    dir.write("a.txt", "alpha\nadded\n");

    let out = mgit(dir.path(), &["diff", &first]);
    assert!(out.contains("diff --mgit a/a.txt b/a.txt"), "{out}");
    assert!(out.contains("+added"), "{out}");
}

#[test]
fn diff_marks_added_and_deleted_files_with_dev_null() {
    let dir = repo_with_one_commit("e2e-diffnull");
    dir.write("new.txt", "brand new\n");
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();
    mgit(dir.path(), &["add", "."]);
    mgit(dir.path(), &["add", "a.txt"]);

    let out = mgit(dir.path(), &["diff", "--staged"]);
    assert!(out.contains("--- /dev/null"), "added file:\n{out}");
    assert!(out.contains("+++ /dev/null"), "deleted file:\n{out}");
}

#[test]
fn diff_rejects_combining_staged_with_a_revision() {
    let dir = repo_with_one_commit("e2e-diffargs");
    let err = mgit_err(dir.path(), &["diff", "--staged", "HEAD"]);
    assert!(err.contains("cannot also take a revision"), "{err}");
}

// ---------------------------------------------------------------------------
// branch / checkout
// ---------------------------------------------------------------------------

#[test]
fn branch_creates_a_41_byte_pointer_and_marks_the_current_one() {
    let dir = repo_with_one_commit("e2e-branch");
    mgit(dir.path(), &["branch", "feature"]);

    let raw = dir.read(".git/refs/heads/feature");
    assert_eq!(raw.len(), 41, "a branch is 40 hex characters and a newline");
    assert_eq!(raw, dir.read(".git/refs/heads/main"));

    let list = mgit(dir.path(), &["branch"]);
    assert!(list.contains("* main"), "{list}");
    assert!(list.contains("  feature"), "{list}");
}

#[test]
fn branch_deletion_removes_the_pointer_but_not_the_commits() {
    let dir = repo_with_one_commit("e2e-branchdel");
    let head = head_oid(dir.path());
    mgit(dir.path(), &["branch", "doomed"]);
    mgit(dir.path(), &["branch", "-d", "doomed"]);

    assert!(!dir.path().join(".git/refs/heads/doomed").exists());
    // The commit is still readable by hash.
    assert_eq!(
        mgit(dir.path(), &["cat-file", "-t", &head]).trim(),
        "commit"
    );

    let err = mgit_err(dir.path(), &["branch", "-d", "main"]);
    assert!(err.contains("HEAD points at"), "{err}");
}

#[test]
fn branch_creation_is_rejected_before_the_first_commit() {
    let dir = ScratchDir::new("e2e-branchearly");
    mgit(dir.path(), &["init", "."]);
    let err = mgit_err(dir.path(), &["branch", "early"]);
    assert!(err.contains("no commits yet"), "{err}");
}

#[test]
fn checkout_restores_a_single_file_from_an_older_commit() {
    let dir = repo_with_one_commit("e2e-restore");
    let first = head_oid(dir.path());

    dir.write("a.txt", "rewritten\n");
    mgit(dir.path(), &["add", "a.txt"]);
    mgit(dir.path(), &["commit", "-m", "rewrite"]);
    assert_eq!(dir.read("a.txt"), "rewritten\n");

    mgit(dir.path(), &["checkout", &first, "--", "a.txt"]);
    assert_eq!(dir.read("a.txt"), "alpha\n");

    // The index was updated too, so the restore does not immediately show up
    // as an unstaged modification.
    let status = mgit(dir.path(), &["status"]);
    assert!(!status.contains("not staged"), "{status}");
    assert!(status.contains("modified:   a.txt"), "{status}");
}

#[test]
fn checkout_restores_a_whole_directory() {
    let dir = repo_with_one_commit("e2e-restoredir");
    let first = head_oid(dir.path());
    std::fs::remove_file(dir.path().join("src/main.rs")).unwrap();

    mgit(dir.path(), &["checkout", &first, "--", "src"]);
    assert_eq!(dir.read("src/main.rs"), "fn main() {}\n");
}

#[test]
fn switching_branches_swaps_the_working_tree() {
    let dir = repo_with_one_commit("e2e-switch");
    mgit(dir.path(), &["branch", "feature"]);

    // Diverge main.
    dir.write("a.txt", "main version\n");
    dir.write("only-on-main.txt", "main only\n");
    mgit(dir.path(), &["add", "."]);
    mgit(dir.path(), &["commit", "-m", "main work"]);

    mgit(dir.path(), &["checkout", "feature"]);
    assert_eq!(dir.read("a.txt"), "alpha\n", "file content should revert");
    assert!(
        !dir.path().join("only-on-main.txt").exists(),
        "a file that does not exist on this branch should be removed"
    );

    mgit(dir.path(), &["checkout", "main"]);
    assert_eq!(dir.read("a.txt"), "main version\n");
    assert!(dir.path().join("only-on-main.txt").exists());
}

#[test]
fn switching_branches_refuses_to_discard_uncommitted_work() {
    let dir = repo_with_one_commit("e2e-switchdirty");
    mgit(dir.path(), &["branch", "feature"]);
    dir.write("a.txt", "uncommitted edit\n");

    let err = mgit_err(dir.path(), &["checkout", "feature"]);
    assert!(err.contains("local changes"), "{err}");
    assert_eq!(
        dir.read("a.txt"),
        "uncommitted edit\n",
        "nothing should be lost"
    );
}

#[test]
fn checkout_of_a_nonexistent_branch_or_path_fails_clearly() {
    let dir = repo_with_one_commit("e2e-checkoutfail");
    let err = mgit_err(dir.path(), &["checkout", "nope"]);
    assert!(
        err.contains("does not exist") || err.contains("cannot resolve"),
        "{err}"
    );

    let head = head_oid(dir.path());
    let err = mgit_err(dir.path(), &["checkout", &head, "--", "missing.txt"]);
    assert!(err.contains("did not match"), "{err}");
}

#[test]
fn commits_on_two_branches_share_the_unchanged_blobs() {
    // The DAG plus content addressing: two branches, one shared object store.
    let dir = repo_with_one_commit("e2e-share");
    let shared = mgit(dir.path(), &["hash-object", "src/main.rs"])
        .trim()
        .to_string();

    mgit(dir.path(), &["branch", "feature"]);
    dir.write("a.txt", "main change\n");
    mgit(dir.path(), &["add", "a.txt"]);
    mgit(dir.path(), &["commit", "-m", "main work"]);

    mgit(dir.path(), &["checkout", "feature"]);
    dir.write("a.txt", "feature change\n");
    mgit(dir.path(), &["add", "a.txt"]);
    mgit(dir.path(), &["commit", "-m", "feature work"]);

    // `src/main.rs` never changed, so both branch tips reach the same blob.
    for branch in ["main", "feature"] {
        let commit = mgit(dir.path(), &["cat-file", "-p", branch]);
        let tree = commit
            .lines()
            .find_map(|l| l.strip_prefix("tree "))
            .unwrap();
        let listing = mgit(dir.path(), &["cat-file", "-p", tree]);
        let src_tree = listing
            .lines()
            .find(|l| l.ends_with("\tsrc"))
            .unwrap()
            .split_whitespace()
            .nth(2)
            .unwrap()
            .to_string();
        let inner = mgit(dir.path(), &["cat-file", "-p", &src_tree]);
        assert!(
            inner.contains(&shared),
            "branch {branch} lost the shared blob:\n{inner}"
        );
    }
}
