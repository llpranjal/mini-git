# MiniGit interview guide

## The one-sentence version

> I built a content-addressed version-control system in Rust that writes Git's
> real loose-object format, so real Git can inspect its repositories and verify
> them with `git fsck --strict`.

## The 30-second version

> Every object is framed as `<type> <size>\0<content>`, named by the SHA-1 of
> those uncompressed bytes, and zlib-compressed under `.git/objects`. `add`
> stores file contents as blobs and records their IDs in an index; `commit`
> converts that flat index into nested tree objects, creates a commit pointing
> to the root tree and parent, then moves one branch ref.
>
> I verified the format against real Git in both directions. The strongest test
> gives both tools the same files, identity, and timestamp and asserts that they
> produce the same commit ID.

## The 60-second version

> I wanted to understand Git below the command layer, so I built a small
> version-control system around its real loose-object format.
>
> The key rule is that an object's ID is the SHA-1 of
> `<type> <size>\0<content>` before compression. Blobs store file bytes, trees
> map names to blob or subtree IDs, and commits point to one root tree and a
> parent commit. The staging index is a flat map from path to blob ID, and a
> commit turns that map into nested trees from the bottom up.
>
> Content addressing makes unchanged data reusable automatically: if an object
> hashes to an existing path, the write is skipped. In a synthetic workload of
> 200 files over 20 commits at 5% churn, 4,000 file versions collapsed to 390
> blobs, avoiding 90.2% of blob writes.
>
> Correctness is externally checked: real Git reads repositories my tool
> creates, `git fsck --strict` passes, my tool reads loose objects Git writes,
> and both produce identical commit IDs from identical input.

## The whiteboard

Draw this:

```text
working files
     |
   mgit add
     v
index: path -> (mode, blob ID)
     |
 mgit commit
     v
root tree -> subtree -> blob
     |
   commit -> parent commit
     ^
     |
refs/heads/main <- HEAD
```

Then explain the layers:

```text
commands
repo + index + trees
store
object + oid
```

- `object + oid` define the exact bytes and their identities.
- `store` maps identities to verified zlib files.
- `index + trees` turn staged paths into a snapshot.
- `repo` manages HEAD and branch pointers.
- `commands` only compose those pieces.

## Walk through one commit

This is the best five-minute discussion because it touches the whole design.

1. `add` reads a file, wraps it as a blob, hashes and stores it, then records
   `(mode, blob ID)` in the index.
2. `commit` reads the index and builds trees bottom-up. A parent tree cannot be
   named until its child tree IDs are known.
3. It reads HEAD to find the current branch and optional parent commit.
4. It compares the new root tree ID with the parent's tree ID and rejects an
   unchanged commit.
5. It writes the new commit object.
6. It moves the branch ref to the new commit ID.

Only step 6 overwrites existing state. The earlier steps create immutable
objects, so interruption before the ref update cannot damage the previous
history.

## Four points worth remembering

### 1. Compression is not identity

The object ID covers uncompressed framed bytes. Two implementations can use
different zlib encoders or compression levels and still agree on the same ID.

### 2. Snapshots are cheap because objects are reused

A commit stores a complete root tree, not a patch. Unchanged files keep their
blob IDs, and unchanged directories keep their tree IDs. Checkout does not need
to replay history.

### 3. A branch is a pointer

A branch is a text file containing a commit ID. Creating a branch is constant
time and deleting it removes only the pointer, not the objects.

### 4. Interoperability catches byte-level bugs

Unit tests can repeat the same mistaken assumption as the implementation.
Matching IDs from real Git checks framing, ordering, raw-vs-hex IDs, timestamps,
newlines, and parent structure together.

## Design choices and trade-offs

| Choice | Why | Cost |
|---|---|---|
| Real Git loose-object format | Independent interoperability oracle | Must match byte-level quirks |
| SHA-1 + zlib dependencies | Cryptography and compression are not the lesson | Four runtime dependencies total |
| Readable `.git/mini-index` | Keeps staging logic understandable and does not corrupt Git's index | No stat cache; status re-hashes files |
| Snapshots, not deltas | Simple checkout and structural sharing | Loose objects use more space than packfiles |
| LCS line diff | Easy to explain and test | `O(n*m)` worst case, guarded by a cell budget |
| Refuse dirty branch switches | Cannot silently lose work | Less flexible than Git's three-way behavior |

## Common questions

### Why hash the header too?

It separates object types and covers the declared length. A blob and commit with
the same payload get different IDs, and truncation cannot masquerade as a valid
shorter object.

### Why SHA-1 if it is collision-prone?

Compatibility with Git's SHA-1 object format was the goal. This project has no
collision detection, so it should not be presented as production-safe. A
production design would use Git's collision-detecting implementation or a
SHA-256 repository format.

### Why not store diffs?

Content addressing already makes unchanged content cheap. Snapshots avoid patch
chains, make checkout independent of history length, and let equality reduce to
comparing IDs.

### What was the hardest format detail?

Tree entries store raw 20-byte IDs, not 40 hex characters, and Git sorts a
directory as though its name ended in `/`. Either mistake produces different
tree and commit IDs even when the visible files are identical.

### What bug did measurement find?

`log -n 10` originally walked the entire history and truncated afterward.
Moving the limit inside the parent traversal made it stay around 2.5 ms from 10
to 500 total commits.

### What would you change for 10x scale?

First add stat metadata to the index so unchanged files can skip reads and
hashing. Then add packfile reads, garbage collection, and a better diff
algorithm. I would measure before adding concurrency because filesystem work is
the current bottleneck.

### How is untrusted input kept inside the repository?

User paths are normalized and rejected if they leave the worktree. Branch names
are validated once at the repository boundary before any ref path is created,
read, written, or deleted.

## Numbers you can defend

Measured by `./scripts/bench.sh` on a synthetic, text-heavy workload:

- 113 passing tests, including 11 Git interoperability tests;
- 200 files, 20 commits, 5% churn;
- 4,000 file versions represented by 390 unique blobs;
- 3,610 blob writes avoided, or 90.2%;
- 13.08 MiB of directory copies versus a 241.8 KiB object store;
- 55.42x total storage reduction, split into 10.24x deduplication and 5.41x
  compression;
- bounded `log -n 10` around 2.5 ms from 10 to 500 total commits.

Always attach the workload to the storage figures. Do not generalize the
compression ratio to binary repositories.

## Claims to avoid

Do not call it a full Git implementation. It has no remotes, merge, packfiles,
garbage collection, real Git index, or collision detection.

Do not say compressed object files are byte-identical to Git's. The valid zlib
streams may differ; the uncompressed framed bytes and object IDs match.

Do not say it reads every Git repository. It reads loose objects, so `git gc`
can move required objects into unsupported packfiles.

Do not describe 55.42x as deduplication alone. Deduplication is 10.24x in the
measured workload; the larger figure includes compression.

## Closing line

> The project taught me that Git's power comes from a very small core:
> immutable content-addressed objects, trees that compose them into snapshots,
> and refs that make those snapshots usable as history.
