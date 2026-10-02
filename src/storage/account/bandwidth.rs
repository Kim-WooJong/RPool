//! Bandwidth timetables in rclone's `--bwlimit` syntax:
//! `10M`, `10M:2M` (upload:download), `off`, or space-separated entries
//! `HH:MM,RATE` / `Day-HH:MM,RATE` (Mon…Sun), e.g.
//! `08:00,512k 18:00,30M 23:00,off`. Rates are binary (`k` = KiB/s, a bare
//! number is KiB/s like rclone, `0`/`off` = unlimited). Times are local.
//! An entry without a day applies every day; before the first entry of the
//! week the last one is still in force (wrap past midnight / Sunday).
use crate::prelude::*;

/// Minutes in one day.
const DAY_MINUTES: u32 = 24 * 60;
/// Minutes in one week; week minutes wrap at this value.
const WEEK_MINUTES: u32 = 7 * DAY_MINUTES;
/// Day prefixes accepted in `Day-HH:MM` entries; index 0 = Monday.
const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Bytes per second; `None` = unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Rate {
    /// Upload limit in bytes per second (`None` = unlimited).
    pub up: Option<u64>,
    /// Download limit in bytes per second (`None` = unlimited).
    pub down: Option<u64>,
}

impl Rate {
    /// No limit in either direction; used when a timetable has no entries.
    pub(crate) const OFF: Rate = Rate {
        up: None,
        down: None,
    };
    /// The value for rclone (`--bwlimit` / `core/bwlimit rate=`).
    pub(crate) fn rclone_value(&self) -> String {
        let one = |rate: Option<u64>| rate.map_or_else(|| "off".to_owned(), |b| format!("{b}B"));
        if self.up == self.down {
            one(self.up)
        } else {
            format!("{}:{}", one(self.up), one(self.down))
        }
    }
    /// Human text, e.g. `512 KiB/s up · unlimited down`.
    pub(crate) fn describe(&self) -> String {
        let one = |rate: Option<u64>| {
            rate.map_or_else(
                || "unlimited".to_owned(),
                |b| format!("{}/s", crate::presentation::format_bytes(b)),
            )
        };
        if self.up == self.down {
            one(self.up)
        } else {
            format!("{} up · {} down", one(self.up), one(self.down))
        }
    }
}

/// One entry: minute of the week it starts (`None` = constant) and its rate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Day 0 = Monday; `None` = every day.
    day: Option<u8>,
    /// Minute of the day (0–1439) at which the entry starts.
    minute: u32,
    /// Rate in force from this entry until the next one.
    rate: Rate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A parsed `--bwlimit` setting: either one constant rate or a weekly schedule.
/// Built by `Timetable::parse` and evaluated by `storage::account::runtime`
/// and `storage::rclone::bwlimit_schedule` to pick the current rate.
pub(crate) struct Timetable {
    /// `None`: a single constant rate.
    constant: Option<Rate>,
    /// Sorted by week minute; entries without a day are expanded to 7.
    week: Vec<(u32, Rate)>,
}

/// Parses one size such as `512k`, `10M` or `off` into bytes per second
/// (bare numbers are KiB/s); `off` or `0` yields `None` (unlimited).
fn parse_size(text: &str) -> Result<Option<u64>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| anyhow!("invalid bandwidth {text:?}"))?;
    let unit: f64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "k" | "ki" | "kib" => 1024.0,
        "b" => 1.0,
        "m" | "mi" | "mib" => 1024.0 * 1024.0,
        "g" | "gi" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "ti" | "tib" => 1024f64.powi(4),
        _ => bail!("invalid bandwidth unit in {text:?} (use B, k, M, G or off)"),
    };
    let bytes = value * unit;
    if !bytes.is_finite() || bytes >= u64::MAX as f64 {
        bail!("bandwidth {text:?} is too large");
    }
    let bytes = bytes.round() as u64;
    Ok((bytes > 0).then_some(bytes))
}

/// Parses `RATE` or `UP:DOWN` into a [`Rate`]; a single value applies to both directions.
fn parse_rate(text: &str) -> Result<Rate> {
    match text.split_once(':') {
        Some((up, down)) => Ok(Rate {
            up: parse_size(up)?,
            down: parse_size(down)?,
        }),
        None => {
            let both = parse_size(text)?;
            Ok(Rate {
                up: both,
                down: both,
            })
        }
    }
}

/// Parses `HH:MM` or `Day-HH:MM` into an optional weekday (0 = Monday) and
/// the minute of the day.
fn parse_time(text: &str) -> Result<(Option<u8>, u32)> {
    let (day, clock) = match text.split_once('-') {
        Some((day, clock)) => {
            let day = DAYS
                .iter()
                .position(|d| d.eq_ignore_ascii_case(day.get(..3).unwrap_or(day)) && day.len() >= 3)
                .ok_or_else(|| anyhow!("invalid day {day:?} (use Mon…Sun)"))?;
            (Some(day as u8), clock)
        }
        None => (None, text),
    };
    let (hour, minute) = clock
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid time {clock:?} (use HH:MM)"))?;
    let hour: u32 = hour
        .parse()
        .map_err(|_| anyhow!("invalid hour in {clock:?}"))?;
    let minute: u32 = minute
        .parse()
        .map_err(|_| anyhow!("invalid minute in {clock:?}"))?;
    if hour > 23 || minute > 59 {
        bail!("time {clock:?} is outside 00:00–23:59");
    }
    Ok((day, hour * 60 + minute))
}

impl Timetable {
    /// Parses a full `--bwlimit` setting. A single token without `,` is a
    /// constant rate; otherwise each `TIME,RATE` entry is expanded to week
    /// minutes and sorted. Errors on empty input, bad syntax or two entries
    /// starting at the same week minute. Used by the limits store validation,
    /// the runtime limiter and the GUI network settings.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            bail!("empty bandwidth setting");
        }
        let parts: Vec<&str> = text.split_whitespace().collect();
        if parts.len() == 1 && !parts[0].contains(',') {
            return Ok(Self {
                constant: Some(parse_rate(parts[0])?),
                week: Vec::new(),
            });
        }
        let mut entries = Vec::new();
        for part in parts {
            let (time, rate) = part
                .split_once(',')
                .ok_or_else(|| anyhow!("timetable entry {part:?} must be TIME,RATE"))?;
            let (day, minute) = parse_time(time)?;
            entries.push(Entry {
                day,
                minute,
                rate: parse_rate(rate)?,
            });
        }
        let mut week: Vec<(u32, Rate)> = Vec::new();
        for entry in &entries {
            let days: Vec<u8> = match entry.day {
                Some(day) => vec![day],
                None => (0..7).collect(),
            };
            for day in days {
                let at = u32::from(day) * DAY_MINUTES + entry.minute;
                if week.iter().any(|(m, _)| *m == at) {
                    bail!("two timetable entries start at the same time");
                }
                week.push((at, entry.rate));
            }
        }
        week.sort_by_key(|(minute, _)| *minute);
        Ok(Self {
            constant: None,
            week,
        })
    }

    /// The rate in force at `week_minute` (0 = Monday 00:00 local) and the
    /// minutes until the next change (`None` = never changes).
    pub(crate) fn at(&self, week_minute: u32) -> (Rate, Option<u32>) {
        if let Some(rate) = self.constant {
            return (rate, None);
        }
        let week_minute = week_minute % WEEK_MINUTES;
        let current = self
            .week
            .iter()
            .rev()
            .find(|(minute, _)| *minute <= week_minute)
            .or_else(|| self.week.last())
            .map_or(Rate::OFF, |(_, rate)| *rate);
        let next = self
            .week
            .iter()
            .find(|(minute, _)| *minute > week_minute)
            .map(|(minute, _)| minute - week_minute)
            .or_else(|| {
                self.week
                    .first()
                    .map(|(minute, _)| minute + WEEK_MINUTES - week_minute)
            });
        (current, next)
    }

    /// Rate in force at unix time `now` with local offset `offset_seconds`,
    /// and the unix time of the next change.
    pub(crate) fn at_unix(&self, now: u64, offset_seconds: i64) -> (Rate, Option<u64>) {
        let local = now as i64 + offset_seconds;
        // 1970-01-01 was a Thursday: Monday-based day = (days + 3) % 7.
        let days = local.div_euclid(86_400);
        let weekday = (days + 3).rem_euclid(7) as u32;
        let seconds = local.rem_euclid(86_400) as u32;
        let week_minute = weekday * DAY_MINUTES + seconds / 60;
        let (rate, next) = self.at(week_minute);
        let next = next.map(|minutes| now - u64::from(seconds % 60) + u64::from(minutes) * 60);
        (rate, next)
    }
}

/// Validates `text` without keeping the result.
pub(crate) fn validate(text: &str) -> Result<()> {
    Timetable::parse(text).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: u64 = 1024;
    const M: u64 = 1024 * 1024;

    fn rate(up: Option<u64>, down: Option<u64>) -> Rate {
        Rate { up, down }
    }

    #[test]
    fn rates_follow_rclone_units() {
        let t = |text: &str| Timetable::parse(text).unwrap().at(0).0;
        assert_eq!(t("512"), rate(Some(512 * K), Some(512 * K)));
        assert_eq!(t("512k"), rate(Some(512 * K), Some(512 * K)));
        assert_eq!(t("1.5M"), rate(Some(3 * M / 2), Some(3 * M / 2)));
        assert_eq!(t("100B"), rate(Some(100), Some(100)));
        assert_eq!(t("10M:off"), rate(Some(10 * M), None));
        assert_eq!(t("off:2M"), rate(None, Some(2 * M)));
        assert_eq!(t("off"), Rate::OFF);
        assert_eq!(t("0"), Rate::OFF);
        for bad in [
            "",
            "fast",
            "10X",
            "08:00,1M 25:00,off",
            "Xyz-08:00,1M",
            "08:00,1M 08:00,2M",
        ] {
            assert!(Timetable::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(rate(Some(M), Some(M)).rclone_value(), "1048576B");
        assert_eq!(rate(Some(M), None).rclone_value(), "1048576B:off");
        assert_eq!(Rate::OFF.rclone_value(), "off");
    }

    #[test]
    fn daily_timetable_wraps_past_midnight() {
        let table = Timetable::parse("08:00,512k 18:00,30M 23:00,off").unwrap();
        let day = |h: u32, m: u32| 2 * DAY_MINUTES + h * 60 + m; // a Wednesday
        assert_eq!(
            table.at(day(8, 0)),
            (rate(Some(512 * K), Some(512 * K)), Some(600))
        );
        assert_eq!(table.at(day(17, 59)).0, rate(Some(512 * K), Some(512 * K)));
        assert_eq!(table.at(day(18, 0)).0, rate(Some(30 * M), Some(30 * M)));
        assert_eq!(table.at(day(23, 30)), (Rate::OFF, Some(8 * 60 + 30)));
        // Before 08:00 the previous evening's "off" is still in force,
        // also on Monday morning (wrap across the week).
        assert_eq!(table.at(day(3, 0)), (Rate::OFF, Some(5 * 60)));
        assert_eq!(table.at(0).0, Rate::OFF);
        assert_eq!(table.at(WEEK_MINUTES - 1), (Rate::OFF, Some(8 * 60 + 1)));
    }

    #[test]
    fn weekday_entries_and_up_down_split() {
        let table = Timetable::parse("Mon-09:00,1M:4M Fri-18:00,off").unwrap();
        assert_eq!(table.at(DAY_MINUTES).0, rate(Some(M), Some(4 * M))); // Tue
        assert_eq!(table.at(5 * DAY_MINUTES).0, Rate::OFF); // Sat
        assert_eq!(table.at(60).0, Rate::OFF); // Mon 01:00, still Friday's
        assert_eq!(table.at(60).1, Some(8 * 60));
        let constant = Timetable::parse("2M").unwrap();
        assert_eq!(constant.at(123), (rate(Some(2 * M), Some(2 * M)), None));
    }

    #[test]
    fn unix_times_use_the_local_offset() {
        let table = Timetable::parse("08:00,1M 18:00,off").unwrap();
        // 2026-10-01 00:00 UTC is a Thursday; 09:00 in UTC+9 is 00:00 UTC.
        let midnight_utc = 1_790_812_800;
        let (now_rate, next) = table.at_unix(midnight_utc, 9 * 3600);
        assert_eq!(now_rate, rate(Some(M), Some(M)));
        assert_eq!(next, Some(midnight_utc + 9 * 3600)); // 18:00 local
        let (utc_rate, next) = table.at_unix(midnight_utc + 30, 0);
        assert_eq!(utc_rate, Rate::OFF);
        assert_eq!(next, Some(midnight_utc + 8 * 3600));
    }
}
