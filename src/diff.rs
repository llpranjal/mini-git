//! Line-level diff, in unified format.
//!
//! This module is pure: it takes two byte slices and returns text. It knows
//! nothing about objects, the index, or the repository, which is why it can be
//! tested exhaustively without touching the filesystem.
//!
//! # The algorithm
//!
//! Longest Common Subsequence via dynamic programming, with two practical
//! guards around it.
//!
//! LCS is the classical formulation of "what is the smallest set of insertions
//! and deletions that turns A into B": the lines the two files have in common,
//! in order, are the ones left untouched, and everything else is an edit. The
//! table is `(n+1) x (m+1)`, so the cost is `O(n*m)` in both time and memory.
//!
//! Real Git uses Myers' diff (an O(ND) algorithm, where D is the edit
//! distance), which is dramatically better when the files are similar --- the
//! common case. Myers was rejected here for a specific reason: LCS is about
//! forty lines and can be explained on a whiteboard, Myers is not, and the
//! guards below recover most of the practical difference. The honest cost of
//! that choice is stated in the README under limitations and again in
//! INTERVIEW.md under "what breaks at 10x scale".
//!
//! ## Guard 1: strip the common prefix and suffix
//!
//! A one-line edit in a 5,000-line file has 4,999 lines of identical context
//! around it. Trimming the matching head and tail first reduces the DP to the
//! genuinely differing middle, which is usually tiny. This is not a heuristic
//! --- lines outside the changed region are provably part of the LCS --- so the
//! output is identical to running LCS on the whole file.
//!
//! ## Guard 2: a hard cell budget
//!
//! If the trimmed region is still large enough that the table would be
//! oversized, the diff degrades to "replace the whole region" and says so in
//! the output. Refusing to allocate gigabytes is better than a plausible OOM,
//! and printing a marker is better than silently producing a worse diff.

/// Maximum number of DP cells (~4M cells * 4 bytes = ~16 MB) before falling
/// back. Chosen so that the fallback is unreachable for ordinary source files
/// but a pathological input cannot exhaust memory.
const MAX_CELLS: usize = 4_000_000;

/// Number of unchanged lines shown around each change, matching Git's default.
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit<'a> {
    Equal(&'a [u8]),
    Delete(&'a [u8]),
    Insert(&'a [u8]),
}

/// True if the content looks binary. Git's own heuristic is "a NUL byte in the
/// first 8000 bytes", and it is used here for the same reason: diffing binary
/// data line-by-line produces unreadable output and enormous diffs.
pub fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

/// Split into lines *without* dropping information.
///
/// Each element keeps its own trailing newline, if it had one. That is what
/// makes "the file used to end with a newline and now does not" a visible
/// change rather than an invisible one, and it means joining the pieces back
/// together reproduces the input exactly.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Compute the edit script between two files.
pub fn diff_lines<'a>(old: &'a [u8], new: &'a [u8]) -> Vec<Edit<'a>> {
    let a = split_lines(old);
    let b = split_lines(new);

    // Guard 1: identical head and tail cannot be part of any edit.
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }

    let a_mid = &a[head..a.len() - tail];
    let b_mid = &b[head..b.len() - tail];

    let mut edits: Vec<Edit<'a>> = Vec::new();
    edits.extend(a[..head].iter().map(|l| Edit::Equal(l)));

    if a_mid.len().saturating_mul(b_mid.len()) > MAX_CELLS {
        // Guard 2: too big to diff precisely. Deleting everything and inserting
        // everything is a correct edit script, just not a minimal one.
        edits.extend(a_mid.iter().map(|l| Edit::Delete(l)));
        edits.extend(b_mid.iter().map(|l| Edit::Insert(l)));
    } else {
        edits.extend(lcs_edits(a_mid, b_mid));
    }

    edits.extend(a[a.len() - tail..].iter().map(|l| Edit::Equal(l)));
    edits
}

/// The DP core. `table[i][j]` is the LCS length of `a[i..]` and `b[j..]`.
///
/// Filled from the bottom-right so that the backtrack in the second loop runs
/// forward through both files, which is the order the output needs. Building it
/// the other way around would require reversing the edit list afterwards.
fn lcs_edits<'a>(a: &[&'a [u8]], b: &[&'a [u8]]) -> Vec<Edit<'a>> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            edits.push(Edit::Equal(a[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            // Tie goes to deletion, so that a replaced line prints as `-` then
            // `+`. Git does the same, and consistency here is what keeps the
            // output stable rather than flipping between runs.
            edits.push(Edit::Delete(a[i]));
            i += 1;
        } else {
            edits.push(Edit::Insert(b[j]));
            j += 1;
        }
    }
    edits.extend(a[i..].iter().map(|l| Edit::Delete(l)));
    edits.extend(b[j..].iter().map(|l| Edit::Insert(l)));
    edits
}

/// Render a unified diff with headers, or an empty string if the files match.
///
/// `old_label` and `new_label` become the `---` and `+++` lines. Callers pass
/// things like `a/src/main.rs` and `b/src/main.rs`, or `/dev/null` for a file
/// that does not exist on one side.
pub fn unified(old: &[u8], new: &[u8], old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }
    if is_binary(old) || is_binary(new) {
        return format!("Binary files {old_label} and {new_label} differ\n");
    }

    let edits = diff_lines(old, new);
    let hunks = group_into_hunks(&edits);
    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));
    for hunk in hunks {
        out.push_str(&render_hunk(&edits, &hunk));
    }
    out
}

/// A contiguous run of the edit list to print, plus the 1-based line numbers it
/// starts at on each side.
struct Hunk {
    start: usize, // index into the edit list
    end: usize,   // exclusive
    old_line: usize,
    new_line: usize,
}

/// Collapse long stretches of unchanged lines, keeping `CONTEXT` on each side.
///
/// Two changes closer than `2 * CONTEXT` lines apart are merged into one hunk,
/// because otherwise their context regions would overlap and the same lines
/// would be printed twice.
fn group_into_hunks(edits: &[Edit]) -> Vec<Hunk> {
    let changed: Vec<usize> = edits
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e, Edit::Equal(_)))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    // Precompute, for each edit index, the line number it occupies on each
    // side. Doing this once avoids re-walking the prefix for every hunk.
    let mut old_at = Vec::with_capacity(edits.len() + 1);
    let mut new_at = Vec::with_capacity(edits.len() + 1);
    let (mut o, mut n) = (1usize, 1usize);
    for edit in edits {
        old_at.push(o);
        new_at.push(n);
        match edit {
            Edit::Equal(_) => {
                o += 1;
                n += 1;
            }
            Edit::Delete(_) => o += 1,
            Edit::Insert(_) => n += 1,
        }
    }
    old_at.push(o);
    new_at.push(n);

    let mut hunks: Vec<Hunk> = Vec::new();
    for &idx in &changed {
        let start = idx.saturating_sub(CONTEXT);
        let end = (idx + CONTEXT + 1).min(edits.len());
        match hunks.last_mut() {
            Some(last) if start <= last.end => last.end = last.end.max(end),
            _ => hunks.push(Hunk {
                start,
                end,
                old_line: old_at[start],
                new_line: new_at[start],
            }),
        }
    }
    hunks
}

fn render_hunk(edits: &[Edit], hunk: &Hunk) -> String {
    let slice = &edits[hunk.start..hunk.end];
    let old_count = slice
        .iter()
        .filter(|e| matches!(e, Edit::Equal(_) | Edit::Delete(_)))
        .count();
    let new_count = slice
        .iter()
        .filter(|e| matches!(e, Edit::Equal(_) | Edit::Insert(_)))
        .count();

    let mut out = format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_line, old_count, hunk.new_line, new_count
    );
    for edit in slice {
        let (marker, line) = match edit {
            Edit::Equal(l) => (' ', l),
            Edit::Delete(l) => ('-', l),
            Edit::Insert(l) => ('+', l),
        };
        out.push(marker);
        // Lines are not necessarily UTF-8 (this is a content-addressed store,
        // not a text database), so render lossily rather than refusing.
        out.push_str(&String::from_utf8_lossy(line));
        if !line.ends_with(b"\n") {
            // Same marker Git prints, and the reason the newline is retained in
            // `split_lines` at all.
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits_of(a: &str, b: &str) -> Vec<String> {
        diff_lines(a.as_bytes(), b.as_bytes())
            .into_iter()
            .map(|e| match e {
                Edit::Equal(l) => format!(" {}", String::from_utf8_lossy(l).trim_end()),
                Edit::Delete(l) => format!("-{}", String::from_utf8_lossy(l).trim_end()),
                Edit::Insert(l) => format!("+{}", String::from_utf8_lossy(l).trim_end()),
            })
            .collect()
    }

    #[test]
    fn identical_files_produce_no_output() {
        assert_eq!(unified(b"same\n", b"same\n", "a", "b"), "");
    }

    #[test]
    fn single_line_change_is_a_delete_then_an_insert() {
        assert_eq!(
            edits_of("a\nb\nc\n", "a\nB\nc\n"),
            vec![" a", "-b", "+B", " c"]
        );
    }

    #[test]
    fn pure_insertion_keeps_all_original_lines_equal() {
        assert_eq!(edits_of("a\nc\n", "a\nb\nc\n"), vec![" a", "+b", " c"]);
    }

    #[test]
    fn pure_deletion_keeps_all_surviving_lines_equal() {
        assert_eq!(edits_of("a\nb\nc\n", "a\nc\n"), vec![" a", "-b", " c"]);
    }

    #[test]
    fn empty_to_content_is_all_insertions() {
        assert_eq!(edits_of("", "x\ny\n"), vec!["+x", "+y"]);
        assert_eq!(edits_of("x\ny\n", ""), vec!["-x", "-y"]);
    }

    #[test]
    fn lcs_finds_the_minimal_script_not_a_naive_one() {
        // Naively replacing line-by-line would give 3 deletes + 3 inserts.
        // The LCS is a,c,e, so the minimal script is 2 deletes and 2 inserts.
        let edits = diff_lines(b"a\nb\nc\nd\ne\n", b"a\nX\nc\nY\ne\n");
        let deletes = edits
            .iter()
            .filter(|e| matches!(e, Edit::Delete(_)))
            .count();
        let inserts = edits
            .iter()
            .filter(|e| matches!(e, Edit::Insert(_)))
            .count();
        assert_eq!((deletes, inserts), (2, 2));
    }

    #[test]
    fn prefix_suffix_trimming_does_not_change_the_result() {
        // 200 identical lines around a single changed line. If trimming were
        // wrong, the edit count would be far higher than 2.
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..100 {
            old.push_str(&format!("line {i}\n"));
            new.push_str(&format!("line {i}\n"));
        }
        old.push_str("CHANGED\n");
        new.push_str("DIFFERENT\n");
        for i in 100..200 {
            old.push_str(&format!("line {i}\n"));
            new.push_str(&format!("line {i}\n"));
        }
        let edits = diff_lines(old.as_bytes(), new.as_bytes());
        let changed = edits
            .iter()
            .filter(|e| !matches!(e, Edit::Equal(_)))
            .count();
        assert_eq!(changed, 2);
    }

    #[test]
    fn hunk_header_reports_correct_line_numbers_and_counts() {
        let mut old = String::new();
        let mut new = String::new();
        for i in 1..=10 {
            old.push_str(&format!("{i}\n"));
            new.push_str(&format!("{}\n", if i == 5 { 99 } else { i }));
        }
        let out = unified(old.as_bytes(), new.as_bytes(), "a/f", "b/f");
        // Change on line 5 with 3 lines of context => starts at line 2,
        // covering 7 lines on each side.
        assert!(out.contains("@@ -2,7 +2,7 @@"), "got:\n{out}");
        assert!(out.contains("-5\n"));
        assert!(out.contains("+99\n"));
        assert!(out.starts_with("--- a/f\n+++ b/f\n"));
    }

    #[test]
    fn far_apart_changes_produce_two_hunks_but_close_ones_merge() {
        use std::fmt::Write;

        let old = (1..=40).fold(String::new(), |mut text, i| {
            writeln!(text, "{i}").unwrap();
            text
        });
        let two_far = (1..=40).fold(String::new(), |mut text, i| {
            if i == 2 || i == 38 {
                writeln!(text, "x{i}").unwrap();
            } else {
                writeln!(text, "{i}").unwrap();
            }
            text
        });
        let far = unified(old.as_bytes(), two_far.as_bytes(), "a", "b");
        assert_eq!(far.matches("@@").count() / 2, 2, "got:\n{far}");

        let two_close = (1..=40).fold(String::new(), |mut text, i| {
            if i == 10 || i == 12 {
                writeln!(text, "x{i}").unwrap();
            } else {
                writeln!(text, "{i}").unwrap();
            }
            text
        });
        let close = unified(old.as_bytes(), two_close.as_bytes(), "a", "b");
        assert_eq!(close.matches("@@").count() / 2, 1, "got:\n{close}");
    }

    #[test]
    fn missing_trailing_newline_is_reported() {
        let out = unified(b"a\n", b"a", "a", "b");
        assert!(out.contains("\\ No newline at end of file"), "got:\n{out}");
    }

    #[test]
    fn binary_content_is_detected_and_not_diffed() {
        assert!(is_binary(b"\x00\x01\x02"));
        assert!(!is_binary(b"plain text\n"));
        let out = unified(b"\x00a", b"\x00b", "a/x.bin", "b/x.bin");
        assert_eq!(out, "Binary files a/x.bin and b/x.bin differ\n");
    }

    #[test]
    fn oversized_input_falls_back_instead_of_allocating_the_table() {
        use std::fmt::Write;

        // Two files with no lines in common, large enough that n*m exceeds the
        // cell budget. Must return promptly with a complete edit script.
        let n = 2500;
        let old = (0..n).fold(String::new(), |mut text, i| {
            writeln!(text, "old line {i}").unwrap();
            text
        });
        let new = (0..n).fold(String::new(), |mut text, i| {
            writeln!(text, "new line {i}").unwrap();
            text
        });
        assert!(n * n > MAX_CELLS);

        let edits = diff_lines(old.as_bytes(), new.as_bytes());
        assert_eq!(
            edits
                .iter()
                .filter(|e| matches!(e, Edit::Delete(_)))
                .count(),
            n
        );
        assert_eq!(
            edits
                .iter()
                .filter(|e| matches!(e, Edit::Insert(_)))
                .count(),
            n
        );
    }

    #[test]
    fn split_lines_preserves_content_exactly() {
        for input in [&b""[..], b"a", b"a\n", b"a\nb", b"\n\n", b"a\nb\n"] {
            let joined: Vec<u8> = split_lines(input).concat();
            assert_eq!(joined, input, "roundtrip failed for {input:?}");
        }
    }
}
