//! Which metadata generation a workspace-less browse should read. "Apply
//! pool changes" moves a drive's metadata to `…/epochs/<epoch>/`, and the
//! epoch is recorded only in that PC's workspace. Without a workspace, the
//! generation is found in the cloud: every replica is listed once
//! (read-only) and the generation with the most recent record wins.
use crate::storage::traits::OperationContext;

/// One metadata generation found in the cloud; `discover` returns them
/// newest first (used by `metadata_pool::pool_target` and browsing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Generation {
    /// `None`: the original (pre-transition) location.
    pub epoch: Option<String>,
    /// Newest record time, nanoseconds since the Unix epoch.
    pub newest: i128,
    /// Objects listed under this generation (all replicas).
    pub records: usize,
}

/// One `rclone lsjson --recursive` entry of a replica root.
#[derive(serde::Deserialize)]
struct Listed {
    /// Path relative to the replica root (`epochs/<epoch>/…` for an epoch).
    #[serde(rename = "Path")]
    path: String,
    /// RFC 3339 modification time (empty when unknown).
    #[serde(rename = "ModTime", default)]
    mod_time: String,
}

/// Groups one replica's recursive listing by generation.
fn group(listing: &[Listed], into: &mut Vec<Generation>) {
    for item in listing {
        let epoch = item
            .path
            .strip_prefix("epochs/")
            .and_then(|rest| rest.split_once('/'))
            .map(|(epoch, _)| epoch.to_string());
        let time = parse_rfc3339(&item.mod_time).unwrap_or(i128::MIN);
        match into.iter_mut().find(|g| g.epoch == epoch) {
            Some(generation) => {
                generation.newest = generation.newest.max(time);
                generation.records += 1;
            }
            None => into.push(Generation {
                epoch,
                newest: time,
                records: 1,
            }),
        }
    }
}

/// Newest first. An error only when no replica could be listed at all.
pub(crate) fn discover(
    rclone: &str,
    pool: &str,
    remotes: &[String],
) -> anyhow::Result<Vec<Generation>> {
    let targets = crate::mount::pool_sync::roots(pool, remotes)?;
    let context = crate::storage::rclone::RcloneContext::inherited(rclone);
    let results: Vec<_> = std::thread::scope(|scope| {
        let jobs: Vec<_> = targets
            .iter()
            .map(|root| {
                let context = &context;
                scope.spawn(move || {
                    let listing = context
                        .list_recursive(&OperationContext::none(), root)
                        .map_err(|e| anyhow::anyhow!("{root}: {e}"))
                        .and_then(|bytes| Ok(serde_json::from_slice::<Vec<Listed>>(&bytes)?));
                    listing
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().expect("listing thread"))
            .collect()
    });
    let mut generations = Vec::new();
    let mut first_error = None;
    let mut listed = 0;
    for listing in results {
        match listing {
            Ok(listing) => {
                listed += 1;
                group(&listing, &mut generations);
            }
            // A replica without this metadata kind is normal ("directory not found").
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    if listed == 0 {
        if let Some(e) = first_error {
            let text = format!("{e:#}");
            if !text.contains("not found") {
                return Err(e.context("cannot list the pool's metadata on any account"));
            }
        }
    }
    sort(&mut generations);
    Ok(generations)
}

/// Newest record first.
pub(crate) fn sort(generations: &mut [Generation]) {
    generations.sort_by_key(|g| std::cmp::Reverse(g.newest));
}

/// `2026-09-30T05:06:38.549521135Z` or `…+09:00` as Unix nanoseconds.
pub(crate) fn parse_rfc3339(text: &str) -> Option<i128> {
    let (date, rest) = text.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (clock, offset) = match rest.find(['Z', '+', '-']) {
        Some(i) => rest.split_at(i),
        None => (rest, "Z"),
    };
    let offset_seconds: i64 = if offset == "Z" || offset.is_empty() {
        0
    } else {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (h, mm) = offset[1..].split_once(':')?;
        sign * (h.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60)
    };
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut t = hms.split(':');
    let (hh, mi, ss): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let nanos: i64 = if fraction.is_empty() {
        0
    } else {
        format!("{:0<9}", &fraction[..fraction.len().min(9)])
            .parse()
            .ok()?
    };
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let seconds = days * 86400 + hh * 3600 + mi * 60 + ss - offset_seconds;
    Some(seconds as i128 * 1_000_000_000 + nanos as i128)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(path: &str, time: &str) -> Listed {
        Listed {
            path: path.into(),
            mod_time: time.into(),
        }
    }

    #[test]
    fn rfc3339_times_with_offsets_compare_correctly() {
        let utc = parse_rfc3339("2026-09-30T05:06:38.5Z").unwrap();
        let kst = parse_rfc3339("2026-09-30T14:06:38.5+09:00").unwrap();
        assert_eq!(utc, kst);
        assert_eq!(parse_rfc3339("1970-01-01T00:00:01Z"), Some(1_000_000_000));
        assert!(parse_rfc3339("2026-10-01T00:00:00Z").unwrap() > utc);
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    #[test]
    fn newest_generation_wins_and_epochs_are_grouped() {
        let mut generations = Vec::new();
        group(
            &[
                listed("events/a", "2026-09-01T00:00:00Z"),
                listed("epochs/abc/events/b", "2026-09-20T00:00:00Z"),
                listed("epochs/abc/events/c", "2026-09-21T00:00:00Z"),
                listed("epochs/old/events/d", "2026-09-10T00:00:00Z"),
            ],
            &mut generations,
        );
        sort(&mut generations);
        assert_eq!(generations[0].epoch.as_deref(), Some("abc"));
        assert_eq!(generations[0].records, 2);
        assert_eq!(generations.len(), 3);
        assert!(generations[2].epoch.is_none());
    }
}
