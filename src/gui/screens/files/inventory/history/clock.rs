//! Local date/time text and parsing for history screens. The offset is the
//! OS's (`crate::utils::local_offset_seconds`, UTC when unknown).

use crate::monitor::history::{civil_from_days, days_from_civil};

/// Seconds per day.
const DAY: i64 = 86_400;

/// The local offset in seconds east of UTC (0 when unknown).
pub(crate) fn offset() -> i64 {
    crate::utils::local_offset_seconds().unwrap_or(0)
}

/// `2026-09-30 14:05` of `unix` at `offset`.
pub(crate) fn format_local(unix: u64, offset: i64) -> String {
    let local = unix as i64 + offset;
    let (days, secs) = (local.div_euclid(DAY), local.rem_euclid(DAY));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// Parses `YYYY-MM-DD HH:MM` (or `YYYY-MM-DD`, meaning 00:00; `T` may
/// separate date and time) as local time at `offset`.
pub(crate) fn parse_local(text: &str, offset: i64) -> Option<u64> {
    let text = text.trim();
    let (date, time) = match text.split_once([' ', 'T']) {
        Some((date, time)) => (date, time.trim()),
        None => (text, "00:00"),
    };
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.is_empty() || day.is_empty() {
        return None;
    }
    let (year, month, day): (i64, u32, u32) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None; // 2026-02-30 and the like.
    }
    let (hour, minute) = time.split_once(':')?;
    let (hour, minute): (i64, i64) = (hour.parse().ok()?, minute.parse().ok()?);
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    let unix = days * DAY + hour * 3600 + minute * 60 - offset;
    u64::try_from(unix).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_local_time() {
        // 2024-10-01 00:00:00 UTC.
        let t = 1_727_740_800;
        assert_eq!(format_local(t, 0), "2024-10-01 00:00");
        assert_eq!(format_local(t, 9 * 3600), "2024-10-01 09:00");
        assert_eq!(format_local(t + 59, -60), "2024-09-30 23:59");
        assert_eq!(parse_local("2024-10-01 09:00", 9 * 3600), Some(t));
        assert_eq!(parse_local(" 2024-10-01T00:30 ", 0), Some(t + 1800));
        assert_eq!(parse_local("2024-10-01", 0), Some(t));
        for bad in [
            "",
            "2024-13-01 00:00",
            "2024-02-30 00:00",
            "2024-10-01 24:00",
            "01-10-2024",
            "2024-10-01 9",
        ] {
            assert_eq!(parse_local(bad, 0), None, "{bad}");
        }
        for t in [0, t, t + 12_345] {
            assert_eq!(parse_local(&format_local(t, 3600), 3600), Some(t - t % 60));
        }
    }
}
