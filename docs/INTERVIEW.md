# Interview notes

## Short explanation

MiniGit is a Rust version-control tool that writes Git-compatible loose objects.
Files become blobs, directories become trees, and commits point to a root tree
and parent commit. Git can inspect those objects and check them with `git fsck`.

## Walk through a commit

1. `add` hashes file contents, writes blobs, and records paths and IDs in the index.
2. `commit` builds directory trees from the index, from children to parents.
3. It writes a commit containing the root tree ID, parent, identity, and message.
4. It updates the current branch ref to the new commit ID.

Unchanged objects keep their IDs and can be reused. The branch update follows
object creation, so an interruption before that update leaves the previous
commit reachable.

## Details to explain

- Object IDs hash `<type> <size>\0<content>` before zlib compression.
- Tree entries use raw 20-byte IDs. Directory names follow Git's tree ordering.
- The separate `.git/mini-index` is readable but has no stat cache, so status
  re-reads and hashes tracked files.
- The line differ uses LCS with a memory budget. Its worst case is `O(n*m)`;
  large changed regions fall back to replacement.
- Checkout refuses local changes. Merge logic is outside the project's scope.
- SHA-1 is used for format compatibility. The implementation has no collision
  detection, packfiles, remotes, or concurrent index updates.

## Verification and results

The tests compare object and commit IDs with Git, read loose objects in both
directions, and run `git fsck --strict`. Different compression bytes are allowed
when they decode to the same framed object.

The checked-in synthetic benchmark represents 4,000 file versions with 390
unique blobs. Its 55.42x storage reduction combines compression and exact-object
deduplication on text with low churn. Changed files still need full new blobs;
that figure is not a general compression ratio.

Run `cargo test` and `./scripts/bench.sh` before using test counts or timings.
