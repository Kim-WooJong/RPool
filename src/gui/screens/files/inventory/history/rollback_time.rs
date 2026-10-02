//! The time a rollback goes back to: a preset ("1 hour ago", …) or a typed
//! local date and time.

use super::clock::parse_local;
use crate::gui::i18n::tr;

/// Choice of time in the rollback dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TimePreset {
    /// One hour before now (the default).
    #[default]
    HourAgo,
    /// 24 hours before now.
    DayAgo,
    /// 7 days before now.
    WeekAgo,
    /// A typed local date and time (`RollbackDialog::custom`).
    Custom,
}

impl TimePreset {
    /// Every preset, in the order the dialog shows them.
    pub(crate) const ALL: [TimePreset; 4] = [
        TimePreset::HourAgo,
        TimePreset::DayAgo,
        TimePreset::WeekAgo,
        TimePreset::Custom,
    ];

    /// Seconds before now (`None` for a typed time).
    pub(crate) fn seconds(self) -> Option<u64> {
        match self {
            TimePreset::HourAgo => Some(3_600),
            TimePreset::DayAgo => Some(86_400),
            TimePreset::WeekAgo => Some(7 * 86_400),
            TimePreset::Custom => None,
        }
    }

    /// Translated button label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            TimePreset::HourAgo => tr("1 hour ago"),
            TimePreset::DayAgo => tr("24 hours ago"),
            TimePreset::WeekAgo => tr("7 days ago"),
            TimePreset::Custom => tr("Pick a date and time"),
        }
    }
}

/// Why the chosen rollback time cannot be used, from `resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimeError {
    /// Not `YYYY-MM-DD HH:MM` or not a real date.
    Format,
    /// Now or later: there is nothing to go back to.
    Future,
}

impl TimeError {
    /// Translated message shown under the time choice.
    pub(crate) fn text(self) -> &'static str {
        match self {
            TimeError::Format => tr("Type the time as YYYY-MM-DD HH:MM, e.g. 2026-09-30 14:00."),
            TimeError::Future => tr("Pick a time in the past."),
        }
    }
}

/// The unix time to roll back to.
pub(crate) fn resolve(
    preset: TimePreset,
    custom: &str,
    now: u64,
    offset: i64,
) -> Result<u64, TimeError> {
    let at = match preset.seconds() {
        Some(seconds) => now.saturating_sub(seconds),
        None => parse_local(custom, offset).ok_or(TimeError::Format)?,
    };
    if at >= now {
        return Err(TimeError::Future);
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_custom_times() {
        let now = 1_727_740_800; // 2024-10-01 00:00 UTC
        assert_eq!(resolve(TimePreset::HourAgo, "", now, 0), Ok(now - 3_600));
        assert_eq!(
            resolve(TimePreset::DayAgo, "junk", now, 0),
            Ok(now - 86_400)
        );
        assert_eq!(resolve(TimePreset::WeekAgo, "", now, 0), Ok(now - 604_800));
        assert_eq!(
            resolve(TimePreset::Custom, "2024-09-30 21:30", now, 0),
            Ok(now - 9_000)
        );
        // 09:00 at UTC+9 is midnight UTC: not in the past.
        assert_eq!(
            resolve(TimePreset::Custom, "2024-10-01 09:00", now, 9 * 3600),
            Err(TimeError::Future)
        );
        assert_eq!(
            resolve(TimePreset::Custom, "2024-10-01 08:59", now, 9 * 3600),
            Ok(now - 60)
        );
        assert_eq!(
            resolve(TimePreset::Custom, "yesterday", now, 0),
            Err(TimeError::Format)
        );
        assert_eq!(resolve(TimePreset::HourAgo, "", 10, 0), Ok(0));
        assert_eq!(
            resolve(TimePreset::HourAgo, "", 0, 0),
            Err(TimeError::Future)
        );
    }
}
