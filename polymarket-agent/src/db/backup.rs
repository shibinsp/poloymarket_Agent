//! Hourly database snapshots.
//!
//! The SQLite file is the only record of what the agent believes it holds.
//! Reconciliation compares it against the venue; if it is gone or corrupt
//! there is nothing left to compare, and the answer to "what positions do I
//! have" becomes whatever the broker's web UI says with no way to tell which
//! of them this agent opened.
//!
//! `VACUUM INTO` rather than a file copy: it takes a consistent snapshot
//! through the same connection as everything else, so it cannot catch a
//! half-written WAL the way `cp` can.
//!
//! The snapshot is then opened and integrity-checked. An unverified backup is
//! a guess about the future, and the moment you find out it was wrong is the
//! moment you needed it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Row};
use tracing::{info, warn};

use crate::db::store::Store;

/// Snapshots kept at hourly granularity.
pub const KEEP_HOURLY: usize = 48;
/// Days for which the newest snapshot of that day is kept.
pub const KEEP_DAILY: usize = 30;

const PREFIX: &str = "snapshot-";
const SUFFIX: &str = ".db";
/// `snapshot-20260921T143000Z.db` — sorts lexicographically in time order,
/// which is what makes pruning a matter of reading file names.
const STAMP: &str = "%Y%m%dT%H%M%SZ";

fn file_name_for(at: DateTime<Utc>) -> String {
    format!("{PREFIX}{}{SUFFIX}", at.format(STAMP))
}

fn timestamp_of(name: &str) -> Option<DateTime<Utc>> {
    let stamp = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    chrono::NaiveDateTime::parse_from_str(stamp, STAMP)
        .ok()
        .map(|n| n.and_utc())
}

/// Ask SQLite whether the file is still coherent.
async fn integrity_check(url: &str) -> Result<()> {
    let mut conn = SqliteConnectOptions::new()
        .filename(url)
        .read_only(true)
        .connect()
        .await
        .with_context(|| format!("Could not open {url} to check it"))?;

    let row = sqlx::query("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await
        .context("integrity_check did not run")?;
    let verdict: String = row.try_get(0).context("integrity_check returned no row")?;

    // SQLite answers the single string "ok", or one row per problem found.
    if verdict != "ok" {
        bail!("integrity check failed: {verdict}");
    }
    Ok(())
}

/// Take one snapshot and verify it. Returns where it landed.
pub async fn snapshot(store: &Store, dir: &Path, at: DateTime<Utc>) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Could not create the backup directory {}", dir.display()))?;

    let path = dir.join(file_name_for(at));
    if path.exists() {
        // VACUUM INTO refuses to overwrite, and saying so plainly beats
        // surfacing SQLite's message for a condition that is really "this
        // hour has already been taken".
        return Ok(path);
    }

    // VACUUM INTO takes no bound parameters, so the path goes into the SQL
    // text. It comes from config rather than from anything a venue or a model
    // said, but doubling the quotes costs nothing and removes the question.
    let escaped = path.to_string_lossy().replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{escaped}'"))
        .execute(store.pool())
        .await
        .with_context(|| format!("VACUUM INTO {} failed", path.display()))?;

    if let Err(e) = integrity_check(&path.to_string_lossy()).await {
        // A backup that does not open is worse than no backup, because it is
        // the one you were relying on. Remove it so the next restore attempt
        // reaches for an older snapshot that might work.
        let _ = std::fs::remove_file(&path);
        return Err(e).with_context(|| format!("Snapshot {} was not usable", path.display()));
    }

    Ok(path)
}

/// Delete snapshots outside the retention policy. Returns how many went.
///
/// Keeps the newest `keep_hourly` outright, plus the newest snapshot of each
/// of the most recent `keep_daily` days. The union, so an agent that was
/// offline for a week does not lose its last snapshot for being old.
pub fn prune(dir: &Path, keep_hourly: usize, keep_daily: usize) -> Result<usize> {
    let mut snapshots: Vec<(DateTime<Utc>, PathBuf)> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // Nothing taken yet is not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).context("Could not list the backup directory"),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(ts) = timestamp_of(&name) {
            snapshots.push((ts, entry.path()));
        }
    }
    // Newest first.
    snapshots.sort_by_key(|(at, _)| std::cmp::Reverse(*at));

    let mut keep: BTreeSet<PathBuf> = snapshots
        .iter()
        .take(keep_hourly)
        .map(|(_, p)| p.clone())
        .collect();

    let mut days_kept: BTreeSet<NaiveDate> = BTreeSet::new();
    for (ts, path) in &snapshots {
        let day = ts.date_naive();
        if days_kept.contains(&day) {
            continue;
        }
        if days_kept.len() >= keep_daily {
            break;
        }
        days_kept.insert(day);
        keep.insert(path.clone());
    }

    let mut removed = 0;
    for (_, path) in &snapshots {
        if keep.contains(path) {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(e) => warn!(path = %path.display(), error = %e, "Could not remove an old snapshot"),
        }
    }
    Ok(removed)
}

/// Snapshot hourly for as long as the agent runs.
///
/// Runs on its own task rather than inside the cycle: the cadence is a
/// property of the clock, not of how often the market is open, and a weekend
/// of `max_sleep_seconds` naps must not mean a weekend with no backups.
pub fn spawn(store: Store, dir: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        // The first tick is immediate, which is wanted here: a process that
        // crashes forty minutes in should still have left one snapshot.
        loop {
            ticker.tick().await;
            let at = Utc::now();
            match snapshot(&store, &dir, at).await {
                Ok(path) => {
                    info!(path = %path.display(), "Database snapshot taken");
                    match prune(&dir, KEEP_HOURLY, KEEP_DAILY) {
                        Ok(0) => {}
                        Ok(n) => info!(removed = n, "Pruned old snapshots"),
                        Err(e) => warn!(error = %e, "Could not prune old snapshots"),
                    }
                }
                // Logged, not fatal, and deliberately not an alert: a full
                // disk would otherwise page hourly forever, and the thing
                // that matters — the agent's own writes failing — already
                // alerts through `DbWriteFailed`.
                Err(e) => warn!(error = %e, "Database snapshot failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, day, hour, 0, 0).unwrap()
    }

    #[test]
    fn a_snapshot_name_round_trips_through_its_timestamp() {
        let when = at(21, 14);
        let name = file_name_for(when);
        assert_eq!(name, "snapshot-20260921T140000Z.db");
        assert_eq!(timestamp_of(&name), Some(when));
    }

    #[test]
    fn unrelated_files_are_not_mistaken_for_snapshots() {
        // Pruning deletes what it recognises. Recognising the live database,
        // or anything else an operator left in the directory, would make this
        // a data-loss bug rather than a retention policy.
        assert_eq!(timestamp_of("polymarket-agent.db"), None);
        assert_eq!(timestamp_of("snapshot-not-a-date.db"), None);
        assert_eq!(timestamp_of("snapshot-20260921T140000Z.db.tmp"), None);
        assert_eq!(timestamp_of("README"), None);
    }

    fn touch(dir: &Path, when: DateTime<Utc>) -> PathBuf {
        let p = dir.join(file_name_for(when));
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[tokio::test]
    async fn a_snapshot_is_taken_and_opens_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("live.db");
        let store = Store::new(db.to_str().unwrap()).await.unwrap();

        let path = snapshot(&store, &dir.path().join("backups"), at(21, 14))
            .await
            .expect("snapshot");
        assert!(path.exists());
        // The point of the exercise: the file is a working database, not
        // just a file that exists.
        integrity_check(&path.to_string_lossy())
            .await
            .expect("the snapshot must be a usable database");
    }

    #[tokio::test]
    async fn a_snapshot_preserves_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("live.db");
        let store = Store::new(db.to_str().unwrap()).await.unwrap();
        store
            .insert_halt("api", "until_resume", "before the backup", at(21, 13))
            .await
            .unwrap();

        let path = snapshot(&store, &dir.path().join("backups"), at(21, 14))
            .await
            .unwrap();

        // Read the halt back out of the *snapshot*, not the live database.
        let restored = Store::new(path.to_str().unwrap()).await.unwrap();
        let halt = restored
            .active_halt()
            .await
            .unwrap()
            .expect("halt survives");
        assert_eq!(halt.detail.as_deref(), Some("before the backup"));
    }

    #[test]
    fn pruning_keeps_the_newest_hourly_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let kept: Vec<PathBuf> = (0..5).map(|h| touch(dir.path(), at(21, h))).collect();

        let removed = prune(dir.path(), 3, 0).unwrap();
        assert_eq!(removed, 2, "five snapshots, keep three");
        assert!(!kept[0].exists() && !kept[1].exists(), "oldest two go");
        assert!(
            kept[2].exists() && kept[3].exists() && kept[4].exists(),
            "newest three stay"
        );
    }

    #[test]
    fn pruning_keeps_one_snapshot_per_day_beyond_the_hourly_window() {
        let dir = tempfile::tempdir().unwrap();
        // Two snapshots a day for three days.
        let d1_early = touch(dir.path(), at(19, 1));
        let d1_late = touch(dir.path(), at(19, 23));
        let d2_early = touch(dir.path(), at(20, 1));
        let d2_late = touch(dir.path(), at(20, 23));
        let d3_early = touch(dir.path(), at(21, 1));
        let d3_late = touch(dir.path(), at(21, 23));

        // No hourly window at all: only the daily rule applies.
        prune(dir.path(), 0, 3).unwrap();

        assert!(d1_late.exists() && d2_late.exists() && d3_late.exists());
        assert!(
            !d1_early.exists() && !d2_early.exists() && !d3_early.exists(),
            "the earlier snapshot of each day is dropped"
        );
    }

    #[test]
    fn a_long_offline_gap_does_not_lose_the_last_snapshot() {
        // The retention rules are a union, not an intersection. An agent that
        // was off for a month must not come back to an empty backup
        // directory the first time it prunes.
        let dir = tempfile::tempdir().unwrap();
        let old = touch(
            dir.path(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        );
        assert_eq!(prune(dir.path(), 48, 30).unwrap(), 0);
        assert!(old.exists());
    }

    #[test]
    fn pruning_an_empty_or_missing_directory_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(prune(&dir.path().join("nothing-here"), 48, 30).unwrap(), 0);
        assert_eq!(prune(dir.path(), 48, 30).unwrap(), 0);
    }

    #[test]
    fn pruning_leaves_files_it_does_not_recognise_alone() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("polymarket-agent.db");
        std::fs::write(&live, b"not a snapshot").unwrap();
        touch(dir.path(), at(21, 1));
        touch(dir.path(), at(21, 2));

        prune(dir.path(), 1, 0).unwrap();
        assert!(live.exists(), "an unrecognised file must never be deleted");
    }
}
