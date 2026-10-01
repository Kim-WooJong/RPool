//! `--at` values and time display: RFC 3339 (`2026-09-30T14:00:00+09:00`,
//! `Z`), a local date/time without offset (`2026-09-30`, `2026-09-30 14:00`),
//! Unix seconds (`1790726400`) or a relative time (`2h ago`, `30m ago`,
//! `3 days ago`).
use crate::prelude::*;

pub(crate) fn parse(text: &str, now: u64) -> Result<u64> {
    let text = text.trim();
    if let Ok(unix) = text.parse::<u64>() {
        return Ok(unix);
    }
    if let Some(relative) = text.strip_suffix("ago") {
        return relative_ago(relative.trim(), now);
    }
    let mut value = text.replace(' ', "T");
    if !value.contains('T') {
        value.push_str("T00:00:00");
    }
    let (_, clock) = value.split_once('T').unwrap_or_default();
    let has_offset = clock.contains(['Z', 'z', '+', '-']);
    if clock.matches(':').count() == 1 && !has_offset {
        value.push_str(":00");
    }
    let nanos = crate::pool::browse_generations::parse_rfc3339(&value.replace('z', "Z"))
        .with_context(|| format!("unrecognized time {text:?} (use RFC 3339, YYYY-MM-DD[ HH:MM], Unix seconds or \"2h ago\")"))?;
    let mut seconds = i64::try_from(nanos.div_euclid(1_000_000_000))?;
    if !has_offset {
        seconds -= crate::utils::local_offset_seconds().unwrap_or(0);
    }
    u64::try_from(seconds).context("time before 1970")
}

fn relative_ago(text: &str, now: u64) -> Result<u64> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .context("relative time needs a number, e.g. \"2h ago\"")?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().context("relative time needs a number")?;
    let unit = unit.trim().trim_end_matches('s');
    let seconds = match unit {
        "" | "sec" | "second" => 1,
        "m" | "min" | "minute" => 60,
        "h" | "hr" | "hour" => 3600,
        "d" | "day" => 86_400,
        "w" | "week" => 7 * 86_400,
        _ => bail!("unknown time unit {unit:?} (use s, m, h, d or w)"),
    };
    now.checked_sub(number.saturating_mul(seconds))
        .context("relative time before 1970")
}

/// `2026-09-30 14:05 UTC+09:00` in this PC's time zone.
pub(crate) fn format(unix: u64) -> String {
    let offset = crate::utils::local_offset_seconds().unwrap_or(0);
    let local = unix as i64 + offset;
    let (year, month, day) = crate::monitor::history::civil_from_days(local.div_euclid(86_400));
    let seconds = local.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} {}",
        seconds / 3600,
        seconds % 3600 / 60,
        crate::utils::offset_label(offset)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_relative_and_unix_times() {
        let base = 1_790_726_400; // 2026-09-30T00:00:00Z
        assert_eq!(parse("2026-09-30T00:00:00Z", 0).unwrap(), base);
        assert_eq!(parse("2026-09-30T09:00:00+09:00", 0).unwrap(), base);
        // Without offset: local time (UTC under test).
        assert_eq!(parse("2026-09-30", 0).unwrap(), base);
        assert_eq!(parse("2026-09-30 01:30", 0).unwrap(), base + 5400);
        assert_eq!(parse("1790726400", 0).unwrap(), base);
        assert_eq!(parse("2h ago", base).unwrap(), base - 7200);
        assert_eq!(parse("3 days ago", base).unwrap(), base - 3 * 86_400);
        assert_eq!(parse("90s ago", base).unwrap(), base - 90);
        assert!(parse("soon", base).is_err());
        assert!(parse("2 fortnights ago", base).is_err());
        assert_eq!(format(base + 3660), "2026-09-30 01:01 UTC+00:00");
    }
}
