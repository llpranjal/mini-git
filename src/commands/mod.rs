//! Porcelain: the user-facing commands.
//!
//! Everything here is thin. Each command reads a little state, calls into the
//! plumbing layers (`store`, `index`, `trees`, `diff`), and formats a result.
//! Any function in this module that starts doing something clever is a sign the
//! logic belongs one layer down where it can be unit-tested without a
//! repository on disk.
//!
//! Commands write to `stdout` through `println!` rather than returning strings.
//! The integration tests therefore run the real binary and assert on its actual
//! output, which is the behaviour that matters, instead of on an internal
//! representation that could drift from what a user sees.

pub mod add;
pub mod branch;
pub mod cat_file;
pub mod checkout;
pub mod commit;
pub mod diff;
pub mod hash_object;
pub mod init;
pub mod log;
pub mod status;

use crate::index::{Index, IndexEntry};
use crate::object::{FileMode, Object, Signature};
use crate::repo::{self, Repository};
use crate::store::ObjectStore;
use anyhow::{Context, Result};
use std::path::Path;

/// Read a working-tree file and stage it: write the blob, record the entry.
///
/// The blob is written to the object store *now*, not at commit time. That is
/// the whole meaning of staging -- the content is captured at the moment of
/// `add`, so editing the file afterwards does not change what will be
/// committed. It also means an `add` you later abandon leaves an unreferenced
/// blob in the store, which real Git cleans up during `gc` and this
/// implementation simply never collects (see README, limitations).
pub fn stage_file(
    store: &ObjectStore,
    index: &mut Index,
    abs_path: &Path,
    rel_path: &str,
) -> Result<()> {
    let content =
        std::fs::read(abs_path).with_context(|| format!("reading {}", abs_path.display()))?;
    let oid = store.write(&Object::Blob(content))?;
    let mode = if repo::is_executable(abs_path) {
        FileMode::Executable
    } else {
        FileMode::Regular
    };
    index.insert(rel_path.to_string(), IndexEntry { mode, oid });
    Ok(())
}

/// Who is making this commit, and when.
///
/// Reads Git's own environment variables so that this tool and real `git`
/// produce *byte-identical commit objects* for identical content. That is not a
/// convenience: it is what lets the interop test commit the same tree with both
/// programs and assert the two commit hashes are equal. Without controllable
/// identity and time, the strongest available interop check would be impossible
/// to write.
///
/// `GIT_AUTHOR_DATE` is understood only in Git's raw form, `@<seconds> <tz>`
/// (for example `@1700000000 +0000`). Git accepts a dozen other date formats;
/// parsing them would mean writing a date parser, which teaches nothing about
/// version control and is not needed by anything here.
pub fn signature(role: &str) -> Signature {
    let var = |suffix: &str| std::env::var(format!("GIT_{role}_{suffix}")).ok();

    let name = var("NAME")
        .or_else(|| std::env::var("GIT_AUTHOR_NAME").ok())
        .unwrap_or_else(|| "mini-git".to_string());
    let email = var("EMAIL")
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok())
        .unwrap_or_else(|| "mini-git@localhost".to_string());

    let (timestamp, tz_offset) = match var("DATE").as_deref().and_then(parse_raw_date) {
        Some(pair) => pair,
        None => (now_seconds(), "+0000".to_string()),
    };

    Signature {
        name,
        email,
        timestamp,
        tz_offset,
    }
}

fn parse_raw_date(raw: &str) -> Option<(i64, String)> {
    let rest = raw.trim().strip_prefix('@')?;
    let (secs, tz) = rest.split_once(' ')?;
    Some((secs.parse().ok()?, tz.to_string()))
}

/// Seconds since the Unix epoch.
///
/// Timestamps are always recorded as UTC with a `+0000` offset. Real Git stores
/// the author's local offset alongside the (still absolute) timestamp, purely so
/// it can redisplay the time as the author saw it. Reproducing that would
/// require reading the system timezone database; the cost is a cosmetic
/// difference in `log` output, and the commit remains valid and correctly
/// ordered, so it was not worth a fifth dependency.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a Unix timestamp the way `git log` does, e.g.
/// `Thu Nov 14 22:13:20 2023 +0000`.
///
/// Hand-rolled civil-date arithmetic rather than a date crate. The algorithm is
/// Howard Hinnant's `civil_from_days`: shift the epoch to March 1st so that the
/// leap day lands at the end of the year, at which point the month lengths form
/// a repeating pattern with a closed-form inverse and no lookup table or
/// iteration is needed. Correct for all dates this program can produce.
pub fn format_time(timestamp: i64, tz: &str) -> String {
    const DAY: i64 = 86_400;
    let days = timestamp.div_euclid(DAY);
    let secs_of_day = timestamp.rem_euclid(DAY);

    // 1970-01-01 was a Thursday, index 4 in a Sunday-first week.
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let weekday = WEEKDAYS[((days + 4).rem_euclid(7)) as usize];

    let z = days + 719_468; // days since 0000-03-01
    let era = z.div_euclid(146_097); // one era = 400 years
    let doe = z.rem_euclid(146_097); // day of era
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; // month index with March = 0
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{weekday} {} {day} {:02}:{:02}:{:02} {year} {tz}",
        MONTHS[(month - 1) as usize],
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

/// Resolve a list of user-supplied paths to repository-relative paths.
pub fn relative_paths(repo: &Repository, paths: &[std::path::PathBuf]) -> Result<Vec<String>> {
    paths.iter().map(|p| repo.relative_path(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_matches_git_log_for_a_known_instant() {
        // `date -u -r 1700000000` => Tue Nov 14 22:13:20 UTC 2023
        assert_eq!(
            format_time(1_700_000_000, "+0000"),
            "Tue Nov 14 22:13:20 2023 +0000"
        );
    }

    #[test]
    fn format_time_handles_the_epoch_and_a_leap_day() {
        assert_eq!(format_time(0, "+0000"), "Thu Jan 1 00:00:00 1970 +0000");
        // 2024-02-29T12:00:00Z
        assert_eq!(
            format_time(1_709_208_000, "+0000"),
            "Thu Feb 29 12:00:00 2024 +0000"
        );
        // 2000-02-29T00:00:00Z -- the century leap year that trips naive rules.
        assert_eq!(
            format_time(951_782_400, "+0000"),
            "Tue Feb 29 00:00:00 2000 +0000"
        );
    }

    #[test]
    fn raw_date_parsing_accepts_gits_at_form_and_rejects_others() {
        assert_eq!(
            parse_raw_date("@1700000000 +0000"),
            Some((1_700_000_000, "+0000".into()))
        );
        assert_eq!(
            parse_raw_date("@1700000000 -0500"),
            Some((1_700_000_000, "-0500".into()))
        );
        assert_eq!(parse_raw_date("2023-11-14 22:13:20"), None);
        assert_eq!(parse_raw_date("@notanumber +0000"), None);
        assert_eq!(parse_raw_date("@1700000000"), None);
    }
}
