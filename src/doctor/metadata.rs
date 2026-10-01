//! `metadata-<pool>` checks: drive metadata growth per saved pool, from one
//! listing of its records and checkpoint heads (no chunk reads).
use super::check::Diagnostic;
use crate::mount::metadata_limits::{LEGACY_BOOTSTRAP_RECORDS, WARN_BYTES, WARN_RECORDS};
use crate::mount::metadata_pool::Stats;

/// Records without a checkpoint for which a recent checkpoint is expected.
const CHECKPOINT_EXPECTED: usize = 2_000;
const STALE_DAYS: u64 = 30;

pub(crate) fn check_metadata(rclone: &str) -> Vec<Diagnostic> {
    let pools = match crate::pool::load_pool_store() {
        Ok(store) => store.pools.into_keys().collect::<Vec<_>>(),
        Err(_) => return Vec::new(), // reported by the pools check
    };
    let now = crate::storage::rclone::traffic::now_unix();
    pools
        .iter()
        .map(
            |pool| match crate::mount::metadata_pool::pool_stats(rclone, pool) {
                Ok(Some(stats)) => assess(pool, &stats, now),
                Ok(None) => diagnostic(pool, "info", "no drive metadata yet".into()),
                Err(error) => diagnostic(pool, "warn", format!("metadata not checked: {error:#}")),
            },
        )
        .collect()
}

fn diagnostic(pool: &str, status: &str, message: String) -> Diagnostic {
    Diagnostic {
        check: format!("metadata-{pool}"),
        status: status.into(),
        message,
    }
}

pub(crate) fn assess(pool: &str, stats: &Stats, now: u64) -> Diagnostic {
    let uncovered = stats.records.saturating_sub(stats.checkpointed as usize);
    let age_days = stats
        .newest_checkpoint_unix
        .map(|t| now.saturating_sub(t) / 86_400);
    let summary = format!(
        "{} {} records ({} MiB), {} checkpoint(s){}",
        stats.family,
        stats.records,
        stats.record_bytes / (1024 * 1024),
        stats.checkpoints,
        age_days.map_or(String::new(), |d| format!(", newest {d} day(s) old"))
    );
    let compact = format!("run `rpool pool compact {pool}`");
    if uncovered >= WARN_RECORDS || (stats.record_bytes >= WARN_BYTES && stats.checkpoints == 0) {
        return diagnostic(
            pool,
            "warn",
            format!("{summary}; about {uncovered} records are not in a checkpoint and a new PC must read each one; {compact}"),
        );
    }
    if stats.records >= WARN_RECORDS && !stats.deletion_enabled {
        return diagnostic(
            pool,
            "warn",
            format!("{summary}; RPool versions without checkpoints cannot open this drive on a new PC beyond {LEGACY_BOOTSTRAP_RECORDS} records. Upgrade every PC, then `rpool pool compact {pool} --enable-deletion`"),
        );
    }
    if stats.records >= CHECKPOINT_EXPECTED && age_days.is_none_or(|d| d > STALE_DAYS) {
        return diagnostic(
            pool,
            "warn",
            format!("{summary}; no checkpoint in the last {STALE_DAYS} days; {compact}"),
        );
    }
    diagnostic(pool, "ok", summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    const DAY: u64 = 86_400;
    fn stats(records: usize, checkpointed: u64, newest: Option<u64>, gate: bool) -> Stats {
        Stats {
            family: "v6".into(),
            records,
            record_bytes: records as u64 * 1024,
            checkpoints: usize::from(newest.is_some()),
            newest_checkpoint_unix: newest,
            checkpointed,
            deletion_enabled: gate,
            ..Default::default()
        }
    }
    #[test]
    fn warns_near_the_old_limit_and_without_recent_checkpoints() {
        let now = 100 * DAY;
        let status = |s: &Stats| assess("p", s, now).status;
        assert_eq!(status(&stats(10, 0, None, false)), "ok");
        assert_eq!(status(&stats(1_999, 0, None, false)), "ok");
        // Checkpoint expected but missing or stale.
        assert_eq!(status(&stats(2_000, 0, None, false)), "warn");
        assert_eq!(
            status(&stats(3_000, 3_000, Some(now - 40 * DAY), true)),
            "warn"
        );
        assert_eq!(status(&stats(3_000, 3_000, Some(now - DAY), true)), "ok");
        // Uncovered records near the old 10,000 limit.
        let near = assess("p", &stats(8_000, 0, Some(now), true), now);
        assert_eq!(near.status, "warn");
        assert!(near.message.contains("rpool pool compact p"));
        // Covered, but old RPool still reads every record until deletion is on.
        let legacy = assess("p", &stats(9_000, 9_000, Some(now), false), now);
        assert_eq!(legacy.status, "warn");
        assert!(legacy.message.contains("--enable-deletion"));
        assert_eq!(status(&stats(9_000, 9_000, Some(now), true)), "ok");
        assert_eq!(
            assess("p", &stats(1, 0, None, false), now).check,
            "metadata-p"
        );
    }
}
