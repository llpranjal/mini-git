# Build journal

What actually happened, including the parts that do not flatter the project.
Written as I went, not reconstructed afterwards.

---

## The toolchain fought back before any code did

First `cargo test` failed to compile — not my code, the dependency tree:

```
error: failed to parse manifest at .../getrandom-0.4.3/Cargo.toml
  feature `edition2024` is required
```

This machine has Rust 1.84. The current `clap` and the current `tempfile` both
pull in transitive dependencies that need the 2024 edition, which is Rust 1.85+.
Cargo 1.84 has no MSRV-aware resolution, so it happily picked versions that
cannot build.

Two fixes. I pinned the four dependencies with `=` so a fresh checkout resolves
to versions that actually compile here. And I dropped `tempfile` entirely and
wrote `src/scratch.rs` — a self-deleting temp directory in about twenty lines.

That second one started as a workaround and turned out to be the better design
anyway. `tempfile`'s transitive tree is larger than this entire program, it would
have been a fifth dependency against a budget of five, and "a unique directory
that cleans itself up" genuinely is twenty lines. The project now has four
dependencies and zero dev-dependencies.

**Lesson:** a constraint I resented produced a better result than I would have
chosen freely.

---

## I was wrong about zlib, and the test caught it

I wrote the object store with `Compression::new(6)` and a confident comment:

> Compression level 6 is zlib's default and Git's default
> (`core.compression = -1` means "zlib default").

Then the interop test that compares loose object files byte-for-byte failed. My
file started `78 9c`; git's started `78 01`. Different compression levels.

I checked instead of guessing:

```
git header bytes: 7801
0 7801 64
1 7801 60 MATCH
6 789c 60
```

Git uses **level 1** for loose objects. The setting is `core.loosecompression`,
which defaults to `Z_BEST_SPEED`; `core.compression` defaults to 6 but governs
*packfiles*. My comment had confidently stated something false, and the only
reason it did not ship is that I had written a test strict enough to notice.

Fixed the level, fixed the comment, and made the constant carry the explanation
so the next reader does not have to rediscover it.

**Lesson:** a comment asserting a fact about someone else's system is a claim,
and claims need checking. The ones stated most confidently are the most dangerous
because nobody re-examines them.

---

## The byte-identical claim I had to walk back

Having switched to level 1, I expected the loose files to be byte-identical to
git's. They still were not — same `78 01` header, different bodies, and mine was
*smaller* (55 bytes vs 60).

The cause: `flate2` defaults to the `miniz_oxide` backend, a pure-Rust DEFLATE.
C zlib and miniz_oxide make different internal choices at the same nominal level.
Both streams are valid level-1 zlib; they are simply not the same bytes.

I could have forced it by enabling flate2's `zlib` feature to link C zlib. I
decided not to: that pulls in `libz-sys` and a C toolchain, and breaks "builds
with a plain `cargo build`", all to buy a property that **nothing requires**.
Compression is not part of an object's identity — the name is the SHA-1 of the
*uncompressed* framed bytes.

So I deleted the byte-identity assertion and replaced it with something stronger
and actually true: physically copy git's loose object file into my repository and
read it back, then copy mine into git's repository and have `git cat-file` and
`git fsck` accept it. That exercises both decoders instead of comparing two
encoders.

**Lesson:** when a test fails, check whether the *assertion* was the wrong thing
to want. Weakening a test to make it pass is usually wrong; replacing a test that
asserted the wrong property is usually right. The distinction is whether the new
test is stronger or weaker evidence for the actual claim.

---

## `git status` was "broken" and it was my documentation's fault

The interop test asserted `git status --porcelain` was empty in a repository
`mgit` had just committed. Instead:

```
D  README.md
?? README.md
```

Git thought every file was both deleted and untracked. Not a corruption bug —
`mgit` stores its index at `.git/mini-index`, so a repository it builds has no
`.git/index` at all. Git compared HEAD against an empty index and reported
everything as deleted, then compared the working tree against the same empty
index and reported everything as untracked.

The fix was to the test, not the code: run `git read-tree HEAD` first to populate
git's index *from the tree object mgit wrote*, and then assert. Everything after
that step is a genuine check — git re-hashes every file on disk and compares
against my tree, and clean means it agrees on content and mode for every path.

I kept the extra step visible in the test with a comment explaining why, rather
than quietly adding it. It documents a real limitation.

**Lesson:** worth being precise about what "compatible" means. My objects, refs,
and HEAD are the real format. My index is not, and lives where git never looks.
That is a deliberate trade, and the test should show it rather than paper over
it.

---

## A bug that depended on iteration order

My tree builder rejected a path that was both a file and a directory:

```rust
if node.dirs.contains_key(*file_name) { bail!(...) }
```

The test passed. Then I realised it only caught one of the two orders. Inserting
`a` then `a/b` hits that check; inserting `a/b` then `a` does not, because by
then `a` is already a file and the loop descending into directories never looks
at `node.files`.

Whether the bug fired depended on `BTreeMap` iteration order — which, for these
two keys, happened to be the order that got caught. A different pair of names
would have sailed through and produced a tree real git rejects.

Added the check in the descent loop too, and rewrote the test to assert **both
orders**:

```rust
for pair in [[("a", a), ("a/b", a)], [("a/b", a), ("a", a)]] { ... }
```

**Lesson:** "the test passes" and "the logic is symmetric" are different claims.
When a check is about a relationship between two things, test both directions.

---

## The benchmark lied to me, twice

This was the most instructive part of the project, and the most embarrassing.

### First lie: single-run timings

The first timing table reported:

```
   50 files    243.1 ms
  100 files     53.1 ms
  200 files    297.1 ms
```

Staging 50 files taking five times longer than staging 100 is impossible. I had
timed each case once and printed it.

I switched to the median of 7 runs. It got better and was still non-monotonic.

### Second lie: my own harness was the noise

Direct measurement outside the benchmark showed something clean:

```
216.9 ms   <- first run
 46.4 ms
 43.8 ms
 ...
```

Two separate effects, not one:

1. **macOS first-exec penalty.** The very first run of a freshly built binary
   pays a one-time launch cost — 217 ms against a 44 ms steady state for
   identical work. Fixed with warmup runs.
2. **My harness was generating the interference it was measuring.** Each
   repetition created a fresh directory with up to 800 files and deleted it on
   drop. Tearing that down is real filesystem work, macOS defers part of it, and
   the deferred work landed inside the *next* repetition's timing window.

The fix was to reuse one directory per size, rewrite the project files in place,
and delete only `.git` between runs. The table became clean and linear
immediately:

```
   50 files     28.5 ms
  100 files     50.8 ms
  200 files     97.9 ms
  400 files    169.0 ms
  800 files    320.5 ms
```

I also switched the headline statistic from median to **minimum**, with the
median printed beside it. The noise here is one-sided — a run can be arbitrarily
slow, never faster than the work takes — so the minimum is the best estimate of
intrinsic cost, and showing both makes the spread visible instead of hidden.

**Lesson:** the benchmark harness needs debugging as much as the code does, and a
measurement I cannot explain is not a measurement. Both times, the number that
looked wrong *was* wrong, and it was mine.

---

## A real bug the benchmark found

While chasing the `log` timings I noticed `log -n 10` got slower as the
repository grew. It should not — a bounded log should read ten commits.

The cause was in the shape of the code, not in any single line:

```rust
let history = repo.history(head)?;          // walks everything
for (..) in history.iter().take(limit)      // then throws most of it away
```

The limit was applied *around* the traversal instead of *inside* it. Pushing it
into `history()` was five lines, and the column went flat: about 2.5 ms whether
the repository holds 10 commits or 500.

This is the one genuine performance bug the measurements caught, and I would not
have found it by reading the code — it looks perfectly reasonable.

---

## The measurement I could not explain, and did not pretend to

Full-history `log` grows faster than linearly past about 250 commits. 100 commits
cost 4.4 ms; 250 cost 7.9 ms; 500 cost 27.4 ms.

I tried to isolate it and ruled two things out by direct measurement:

- **Output volume.** Printing 3,600 lines instead of 400 for the same 400-commit
  walk cost 10% more — 49.3 ms vs 44.9 ms. Formatting is not the cost.
- **Object store size.** Walking 200 commits cost the same in a 4,005-object
  repository as in a 2,005-object one — 7.5 ms vs 7.8 ms.

Every operation inside the walk is O(1) per commit: one file read, one inflate,
one SHA-1 verify over a ~250-byte object, one parent pointer. The remaining
candidates are outside the program — process scheduling across Apple Silicon's
efficiency and performance cores, allocator behaviour as the retained commit list
grows — and I did not chase them further.

So I wrote it down as unresolved, in the benchmark output, in the README, and in
`RESUME_BULLETS.txt` under what not to claim. The defensible figure is the linear
region, roughly 25 µs per commit up to ~250 commits.

I would rather have a documented open question than a number I cannot defend if
someone asks how I know.

---

## Designs I rejected

**A JSON object format.** Would have been faster to write and trivially
debuggable. Rejected because the real format *is* the project — a simplified one
teaches the shape of the design while skipping every detail that actually
matters, and only a real format lets `git fsck` grade the work.

**Git's binary `DIRC` index.** Rejected for explainability, and because the
interoperability that matters does not depend on it. The cost is the stat cache,
which is the biggest performance gap versus real git and is stated as such.

**Myers' diff.** Rejected because LCS fits on a whiteboard and Myers does not.
Prefix/suffix trimming plus a cell budget recovers most of the practical
difference. If diff time ever appeared in a profile I would switch.

**Storing deltas instead of snapshots.** Rejected because content addressing
already provides the deduplication a delta scheme is buying, and snapshots make
`checkout` O(tree) rather than O(history). Worth noting the honest limit: my
deduplication is **exact-match only**. Two versions of a large file differing by
one line are stored twice at full size. Real git's packfiles would store one plus
a delta. That is the largest storage difference between this and the real thing.

**Carrying local changes across a branch switch.** Real git does this. Doing it
correctly needs a three-way merge, which is out of scope, so `checkout` refuses
when there are uncommitted changes. Refusing is the honest version of "I cannot
do this safely"; the alternative silently destroys work.

---

## What I would do differently

**Write the interop test first.** It caught two format errors — the compression
level and the tree sort order — that I would otherwise have shipped, and it would
have caught them sooner.

**Put stat data in the index from the beginning.** It is the one omission I kept
paying for, and it shows up in every `status` and `diff`.

**Distrust the first measurement, always.** Both times a number looked wrong, it
was wrong, and both times the fault was in my harness rather than the code.
