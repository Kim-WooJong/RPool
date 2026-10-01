//! Per-minute traffic history: one JSON line per account and minute in
//! `<YYYY-MM-DD>.jsonl` (UTC day of the minute), deleted after `HISTORY_DAYS`.
use super::model::{HistoryPoint, HISTORY_DAYS};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;

const DAY: u64 = 86_400;

/// Proleptic Gregorian (year, month, day) of `days` since 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD.jsonl` of the UTC day containing `unix`.
pub(crate) fn file_name(unix: u64) -> String {
    let (year, month, day) = civil_from_days((unix / DAY) as i64);
    format!("{year:04}-{month:02}-{day:02}.jsonl")
}

/// Day number of a history file name, `None` for any other file.
fn file_day(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".jsonl")?;
    let mut parts = stem.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (civil_from_days(days) == (year, month, day)).then_some(days)
}

/// Appends `points` to the day files of their minutes.
pub(crate) fn append(dir: &Path, points: &[HistoryPoint]) -> std::io::Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let mut by_day: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for point in points {
        let lines = by_day.entry(file_name(point.minute_unix)).or_default();
        serde_json::to_writer(&mut *lines, point).map_err(std::io::Error::other)?;
        lines.push(b'\n');
    }
    for (name, lines) in by_day {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))?;
        file.write_all(&lines)?;
    }
    Ok(())
}

/// Deletes history files of days more than `HISTORY_DAYS` before `now`'s day.
pub(crate) fn prune(dir: &Path, now: u64) -> std::io::Result<usize> {
    let keep_from = (now / DAY) as i64 - HISTORY_DAYS as i64;
    let mut removed = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if file_day(&name.to_string_lossy()).is_some_and(|day| day < keep_from) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Points with `minute_unix >= since_unix`, oldest first (then by remote).
/// Unreadable files and lines are skipped.
pub(crate) fn load(dir: &Path, since_unix: u64) -> Vec<HistoryPoint> {
    let first_day = (since_unix / DAY) as i64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !file_day(&name.to_string_lossy()).is_some_and(|day| day >= first_day) {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            if let Ok(point) = serde_json::from_str::<HistoryPoint>(&line) {
                if point.minute_unix >= since_unix {
                    points.push(point);
                }
            }
        }
    }
    points.sort_by(|a, b| (a.minute_unix, &a.remote).cmp(&(b.minute_unix, &b.remote)));
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(minute: u64, remote: &str) -> HistoryPoint {
        HistoryPoint {
            minute_unix: minute,
            remote: remote.into(),
            upload_bytes: minute,
            download_bytes: 1,
            ok_ops: 2,
            failed_ops: 0,
        }
    }

    #[test]
    fn dates_are_utc_days() {
        assert_eq!(file_name(0), "1970-01-01.jsonl");
        assert_eq!(file_name(951_782_400), "2000-02-29.jsonl");
        assert_eq!(file_name(1_790_812_799), "2026-09-30.jsonl");
        assert_eq!(file_name(1_790_812_800), "2026-10-01.jsonl");
        assert_eq!(
            file_day("2026-10-01.jsonl"),
            Some(1_790_812_800 / DAY as i64)
        );
        for bad in [
            "2026-02-30.jsonl",
            "2026-10-01.json",
            "x.jsonl",
            "2026-1-01.jsonl",
        ] {
            assert_eq!(file_day(bad), None, "{bad}");
        }
        for days in [-1000, 0, 11_016, 20_000, 60_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn append_splits_by_day_load_filters_sorts_and_skips_bad_lines() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("net-history");
        let day = 1_790_812_800; // 2026-10-01
        append(
            &dir,
            &[point(day - 60, "b:"), point(day, "b:"), point(day, "a:")],
        )
        .unwrap();
        append(&dir, &[point(day + 60, "a:")]).unwrap();
        append(&dir, &[]).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("2026-10-01.jsonl"))
            .unwrap();
        file.write_all(b"not json\n{\"minute_unix\":1}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(dir.join("2026-09-30.jsonl").exists());
        let all = load(&dir, 0);
        let keys: Vec<_> = all
            .iter()
            .map(|p| (p.minute_unix, p.remote.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![(day - 60, "b:"), (day, "a:"), (day, "b:"), (day + 60, "a:")]
        );
        assert_eq!(load(&dir, day).len(), 3);
        assert_eq!(load(&dir, day + 61).len(), 0);
        assert!(load(&root.path().join("missing"), 0).is_empty());
    }

    #[test]
    fn prune_keeps_the_retention_window_and_other_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let now = 1_790_812_800 + 3600;
        let today = now / DAY;
        for back in [0, HISTORY_DAYS, HISTORY_DAYS + 1, 400] {
            std::fs::write(dir.join(file_name((today - back) * DAY)), b"").unwrap();
        }
        std::fs::write(dir.join("keep.txt"), b"").unwrap();
        assert_eq!(prune(dir, now).unwrap(), 2);
        assert!(dir.join(file_name(now)).exists());
        assert!(dir.join(file_name((today - HISTORY_DAYS) * DAY)).exists());
        assert!(!dir
            .join(file_name((today - HISTORY_DAYS - 1) * DAY))
            .exists());
        assert!(dir.join("keep.txt").exists());
        assert_eq!(prune(&dir.join("missing"), now).unwrap(), 0);
    }
}
