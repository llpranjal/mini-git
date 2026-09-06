# mini-git

A small version-control system in Rust that writes Git's real loose-object
format. Real `git` can inspect repositories created by `mgit`, and `mgit` can
read loose objects written by real Git.

Files become content-addressed objects. Trees group them into snapshots, and
commits link snapshots into history.

```text
working tree --add--> index --commit--> tree + commit
                                      |
                                      v
                         .git/objects/ab/cdef...
```

## What it implements

```text
mgit init
mgit hash-object
mgit cat-file
mgit add
mgit commit
mgit log
mgit status
mgit diff
mgit checkout
mgit branch
```

The commands cover object inspection, staging, snapshots, history, working-tree
comparison, and branches.

## The three ideas

### 1. Objects are named by their content

Every object is framed as:

```text
<type> <size>\0<content>
```

`mgit` computes the SHA-1 of those uncompressed bytes. The resulting 20-byte
identifier is the object's name, and the zlib-compressed object is stored under:

```text
.git/objects/<first 2 hex digits>/<remaining 38>
```

This gives two useful properties:

- Identical framed content produces identical IDs; SHA-1 collisions are not detected.
- Writing the same object twice is a no-op because its path already exists.

The store skips a write when the object path already exists:

```rust
if path.exists() {
    return Ok((oid, true));
}
```

### 2. A commit stores a snapshot, not a patch

The staging index is a flat map:

```text
README.md   -> blob a1b2...
src/main.rs -> blob c3d4...
```

Git tree objects are nested by directory, so `mgit commit` converts that flat
map into a hierarchy:

```text
root tree
├── README.md -> blob a1b2...
└── src       -> tree e5f6...
                └── main.rs -> blob c3d4...
```

A commit then stores:

```text
tree <root-tree-id>
parent <previous-commit-id>
author ...
committer ...

message
```

Unchanged blobs and subtrees keep the same IDs and are reused by reference.
That makes snapshots space-efficient without storing delta chains.

### 3. Interoperability is the correctness oracle

Format code can look reasonable and still differ by one byte. The strongest
tests therefore compare `mgit` with an independent implementation:

- real Git reads blobs, trees, commits, refs, and history written by `mgit`;
- `mgit` reads loose objects written by Git;
- both tools produce identical blob, tree, and commit IDs for identical input;
- `git fsck --strict` accepts a repository built entirely by `mgit`.

The test suite has 113 tests, including 11 Git interoperability tests.

## Architecture

Commands compose the storage and repository modules:

```text
commands/   CLI workflows
    |
repo + index + trees   repository state and snapshots
    |
store                  zlib files, atomic writes, verification
    |
object + oid           exact bytes and 20-byte identities
```

| Module | Responsibility |
|---|---|
| `oid.rs` | Parse and format 20-byte object IDs |
| `object.rs` | Encode and decode blobs, trees, and commits |
| `store.rs` | Compress, write, locate, read, and verify loose objects |
| `index.rs` | Persist the staging area as `path -> (mode, blob ID)` |
| `trees.rs` | Convert the flat index to nested Git trees and back |
| `repo.rs` | Discover repositories, manage HEAD/branches, walk files |
| `commands/` | Compose the lower layers into CLI behavior |
| `diff.rs` | Produce bounded line-level unified diffs |

The core commit path is:

1. Read the index.
2. Build tree objects from the bottom up.
3. Read HEAD to find the parent commit.
4. Write a commit object.
5. Move the current branch ref.

Only the last step changes existing state. If the process stops earlier, it may
leave unreachable immutable objects, but the previous commit remains intact.

## Quick start

Requires Rust 1.84+ and Git for the interoperability tests.

```bash
cargo build --release
cargo test
```

Try it:

```bash
mkdir demo
cd demo
../target/release/mgit init .

printf 'hello\n' > hello.txt
../target/release/mgit add hello.txt
../target/release/mgit commit -m "first commit"

../target/release/mgit log --oneline
git log --oneline
git fsck --strict
```

To reproduce the benchmark:

```bash
./scripts/bench.sh
```

## Measured results

The checked-in run is in [`results/benchmark.txt`](results/benchmark.txt). It
used 200 generated source-like files across 20 commits with 5% churn.

| Measurement | Result |
|---|---:|
| File versions committed | 4,000 |
| Unique blobs stored | 390 |
| Blob writes avoided | 3,610 (90.2%) |
| Directory-copy baseline | 13.08 MiB |
| `mgit` object store | 241.8 KiB |
| Total storage reduction | 55.42x |
| Deduplication alone | 10.24x |
| Compression alone | 5.41x |

The 55.42x figure combines deduplication and compression. It is not a general
compression claim: the workload is synthetic and text-heavy.

For history traversal, `log -n 10` stayed around 2.5 ms from 10 to 500 commits
because the limit is applied inside the parent-pointer walk.

## Deliberate trade-offs

### A separate index

`mgit` stores its readable staging file at `.git/mini-index`, not `.git/index`.
Git's real index is a binary format with stat data, extensions, and a checksum.
Using a separate file keeps this project focused and prevents `mgit` from
writing a fake index that breaks real Git commands.

The cost is performance: `status` and `diff` re-read and re-hash tracked files
because the mini index has no stat cache.

### Loose objects only

`mgit` reads and writes zlib-compressed loose objects. It does not implement
packfiles or garbage collection. A repository processed by `git gc` may no
longer be readable by `mgit`.

### A bounded LCS diff

The line differ uses longest common subsequence because it is compact and easy
to explain. Its worst case is `O(n*m)`, so it trims common prefixes/suffixes and
falls back to replacing the changed region when the DP table would exceed a
fixed memory budget. Real Git uses more sophisticated algorithms.

### Conservative checkout

Branch switching refuses to run with local changes instead of attempting a
three-way merge. This protects user data while keeping merge logic out of scope.

## Not implemented

- remotes, clone, fetch, or push;
- merge, rebase, cherry-pick, reset, or stash;
- packfiles, garbage collection, or delta compression;
- Git's binary index format or `.gitignore`;
- submodules, symlinks, annotated tags, or full revision syntax;
- SHA-1 collision detection;
- concurrent index updates.

## Interview guide

[Interview notes](docs/INTERVIEW.md) walk through a commit, the interoperability
tests, and the main limitations.
