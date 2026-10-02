//! Limits and notes for the trash/version retention of a pool (0 means
//! unlimited for every field).

use crate::drive_history::model::Retention;
use crate::gui::i18n::{tr, trf};

/// Highest accepted day count (about ten years) for trash and version age limits.
pub(crate) const MAX_DAYS: u32 = 3_650;
/// Highest accepted number of versions to keep per file.
pub(crate) const MAX_VERSIONS: u32 = 1_000;

/// Which retention field is out of range, from `validate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetentionError {
    /// `trash_days` above `MAX_DAYS`.
    TrashDays,
    /// `keep_versions` above `MAX_VERSIONS`.
    KeepVersions,
    /// `version_days` above `MAX_DAYS`.
    VersionDays,
}

impl RetentionError {
    /// Translated message shown next to the invalid field.
    pub(crate) fn text(self) -> String {
        match self {
            RetentionError::TrashDays => trf(
                "Days in the trash must be 0 to {max}.",
                &[("max", &MAX_DAYS)],
            ),
            RetentionError::KeepVersions => trf(
                "Versions to keep must be 0 to {max}.",
                &[("max", &MAX_VERSIONS)],
            ),
            RetentionError::VersionDays => trf(
                "Days to keep versions must be 0 to {max}.",
                &[("max", &MAX_DAYS)],
            ),
        }
    }
}

/// Checks the limits before `retention set`; returns the values unchanged when
/// valid. Used by the Storage › retention card.
pub(crate) fn validate(r: Retention) -> Result<Retention, RetentionError> {
    if r.trash_days > MAX_DAYS {
        Err(RetentionError::TrashDays)
    } else if r.keep_versions > MAX_VERSIONS {
        Err(RetentionError::KeepVersions)
    } else if r.version_days > MAX_DAYS {
        Err(RetentionError::VersionDays)
    } else {
        Ok(r)
    }
}

/// Notes about what the values mean for storage use.
pub(crate) fn notes(r: Retention) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if r.trash_days == 0 {
        notes.push(tr(
            "Deleted files stay in the trash until you delete them or empty the trash.",
        ));
    }
    if r.keep_versions == 0 && r.version_days == 0 {
        notes.push(tr(
            "Every previous version is kept: the pool grows with each change to a file.",
        ));
    }
    notes
}

/// "30 days" / "unlimited" for the summary line.
pub(crate) fn days_label(days: u32) -> String {
    match days {
        0 => tr("unlimited").into(),
        1 => tr("1 day").into(),
        n => trf("{n} days", &[("n", &n)]),
    }
}

/// "20" / "unlimited".
pub(crate) fn versions_label(count: u32) -> String {
    match count {
        0 => tr("unlimited").into(),
        n => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_notes() {
        let ok = Retention::default();
        assert_eq!(validate(ok), Ok(ok));
        let unlimited = Retention {
            trash_days: 0,
            keep_versions: 0,
            version_days: 0,
        };
        assert_eq!(validate(unlimited), Ok(unlimited));
        assert_eq!(notes(unlimited).len(), 2);
        assert!(notes(ok).is_empty());
        let only_count = Retention {
            version_days: 0,
            ..ok
        };
        assert!(notes(only_count).is_empty(), "the count still limits");
        for (bad, error) in [
            (
                Retention {
                    trash_days: MAX_DAYS + 1,
                    ..ok
                },
                RetentionError::TrashDays,
            ),
            (
                Retention {
                    keep_versions: MAX_VERSIONS + 1,
                    ..ok
                },
                RetentionError::KeepVersions,
            ),
            (
                Retention {
                    version_days: MAX_DAYS + 1,
                    ..ok
                },
                RetentionError::VersionDays,
            ),
        ] {
            assert_eq!(validate(bad), Err(error));
        }
        assert_eq!(days_label(0), "unlimited");
        assert_eq!(days_label(30), "30 days");
        assert_eq!(versions_label(1), "1");
        assert_eq!(versions_label(0), "unlimited");
    }
}
