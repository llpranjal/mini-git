# Implementation notes

## Object identity and compression

The object ID hashes the uncompressed header and content. Different valid zlib
encodings can have the same object ID. Interoperability tests compare IDs and
read objects in both directions instead of requiring identical compressed files.

## Separate staging index

`mgit` writes `.git/mini-index`. Real Git uses `.git/index`, so inspecting
working-tree status with Git requires populating its index with `git read-tree
HEAD` first. Object and history inspection do not need that step.

## Tree paths

A snapshot cannot contain both a file `a` and a path beneath it, such as `a/b`.
The tree builder checks file/directory conflicts while descending and when
inserting the final entry. Tree entries store raw object IDs and use Git's
name ordering.

## History traversal

The history limit is applied inside the parent-pointer walk. `log -n 10` reads
at most ten commits rather than loading all history and truncating the output.
The recorded benchmark showed roughly 2.5 ms for this command across histories
of 10 to 500 commits; timings remain machine-dependent.

## Benchmark limits

The benchmark uses warmups and repeated runs. It reports minimum and median
timings; the minimum describes the fastest observed run, not a guaranteed
intrinsic cost. Earlier full-history measurements became nonlinear at larger
sizes, and the cause was not established.

Storage results use generated text with low churn. Deduplication reuses exact
objects only: changing one line creates a new full blob. The project does not
implement packfile delta compression.
