//! The local time zone's UTC offset, for schedules written in local time.
//! The standard library has no time zone support, so the offset is asked from
//! the OS once an hour (`date +%z` on Unix, PowerShell on Windows) and falls
//! back to UTC when that fails.
use std::sync::Mutex;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(3600);

/// Seconds east of UTC (`+0900` = 32400). `None` when unknown.
pub(crate) fn local_offset_seconds() -> Option<i64> {
    static CACHE: Mutex<Option<(Instant, Option<i64>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, value)) = *cache {
        if at.elapsed() < REFRESH {
            return value;
        }
    }
    let value = if cfg!(test) { Some(0) } else { query() };
    *cache = Some((Instant::now(), value));
    value
}

#[cfg(unix)]
fn query() -> Option<i64> {
    let output = std::process::Command::new("date")
        .arg("+%z")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_offset(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(windows)]
fn query() -> Option<i64> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[int][TimeZoneInfo]::Local.GetUtcOffset([DateTime]::UtcNow).TotalMinutes",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    let minutes: i64 = std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (minutes.abs() <= 18 * 60).then_some(minutes * 60)
}

#[cfg(not(any(unix, windows)))]
fn query() -> Option<i64> {
    None
}

/// `+0930` / `-0500` → seconds.
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "parsed from `date` on Unix only")
)]
pub(crate) fn parse_offset(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    (hours <= 18 && minutes < 60).then_some(sign * (hours * 3600 + minutes * 60))
}

/// `UTC+09:00`.
pub(crate) fn offset_label(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_parse_and_label() {
        assert_eq!(parse_offset("+0900\n"), Some(32_400));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        assert_eq!(parse_offset("0900"), None);
        assert_eq!(parse_offset("+9"), None);
        assert_eq!(parse_offset("+2500"), None);
        assert_eq!(offset_label(32_400), "UTC+09:00");
        assert_eq!(offset_label(-19_800), "UTC-05:30");
        assert_eq!(local_offset_seconds(), Some(0));
    }
}
