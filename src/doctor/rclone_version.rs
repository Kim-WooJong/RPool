//! The rclone versions RPool supports, and the check of a detected version.
//!
//! Minimum v1.64.0 (https://rclone.org/changelog/): every mount passes
//! `--vfs-cache-min-free-space` (added in v1.64.0) and the diagnostics bundle
//! uses `rclone config redacted` (added in v1.64.0). Older features RPool also
//! needs: `obscure -` from stdin (v1.53.0), `lsjson --stat` and the rc
//! `operations/stat` call (v1.57.0), rc `vfs/stats` (v1.58.0), rcd over a
//! unix socket via lib/http (v1.61.0). macOS mounts need `nfsmount`
//! (v1.65.0; checked again when mounting).
//!
//! Recommended v1.74.3: it fixes unauthenticated command execution via
//! `--rc-serve` inline remotes (CVE-2026-49980). RPool's helper daemon runs
//! `rcd --rc-serve`. It also has rc `vfs/queue` / `vfs/queue-set-expiry`
//! (v1.68.0), which RPool uses to flush the write-back queue before unmount.
use super::Diagnostic;

pub(crate) const MINIMUM: (u32, u32, u32) = (1, 64, 0);
pub(crate) const RECOMMENDED: (u32, u32, u32) = (1, 74, 3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Support {
    Ok,
    BelowRecommended,
    BelowMinimum,
    Unknown,
}

/// `(major, minor, patch)` from the first line of `rclone version`
/// (`rclone v1.75.1`, `rclone v1.60.1-DEV`, `rclone v1.66.0-beta.7701…`).
pub(crate) fn parse(first_line: &str) -> Option<(u32, u32, u32)> {
    let rest = first_line.trim().strip_prefix("rclone v")?;
    let mut parts = rest.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch_text = parts.next().unwrap_or("0");
    let digits: String = patch_text
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    Some((major, minor, digits.parse().unwrap_or(0)))
}

pub(crate) fn support(version: Option<(u32, u32, u32)>) -> Support {
    match version {
        None => Support::Unknown,
        Some(v) if v < MINIMUM => Support::BelowMinimum,
        Some(v) if v < RECOMMENDED => Support::BelowRecommended,
        Some(_) => Support::Ok,
    }
}

pub(crate) fn display((major, minor, patch): (u32, u32, u32)) -> String {
    format!("v{major}.{minor}.{patch}")
}

/// The doctor row for a detected `rclone version` first line.
pub(crate) fn diagnostic(first_line: &str) -> Diagnostic {
    let version = parse(first_line);
    let (status, message) = match support(version) {
        Support::Ok => (
            "ok",
            format!(
                "{} meets the minimum {} and recommended {}",
                first_line.trim(),
                display(MINIMUM),
                display(RECOMMENDED)
            ),
        ),
        Support::BelowRecommended => (
            "warn",
            format!(
                "{} works but is older than the recommended {} (fixes CVE-2026-49980 in --rc-serve); update rclone",
                first_line.trim(),
                display(RECOMMENDED)
            ),
        ),
        Support::BelowMinimum => (
            "fail",
            format!(
                "{} is older than the minimum {}; mounts and diagnostics need newer rclone features",
                first_line.trim(),
                display(MINIMUM)
            ),
        ),
        Support::Unknown => (
            "warn",
            format!("cannot read the rclone version from {:?}", first_line.trim()),
        ),
    };
    Diagnostic {
        check: "rclone-support".into(),
        status: status.into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_dev_and_beta_lines() {
        assert_eq!(parse("rclone v1.75.1"), Some((1, 75, 1)));
        assert_eq!(parse("rclone v1.60.1-DEV"), Some((1, 60, 1)));
        assert_eq!(parse("rclone v1.66.0-beta.7701.abc"), Some((1, 66, 0)));
        assert_eq!(parse("rclone v2.0"), Some((2, 0, 0)));
        assert_eq!(parse("rclone version unknown"), None);
        assert_eq!(parse("restic 0.16"), None);
    }

    #[test]
    fn classifies_against_minimum_and_recommended() {
        assert_eq!(support(Some((1, 63, 9))), Support::BelowMinimum);
        assert_eq!(support(Some((1, 64, 0))), Support::BelowRecommended);
        assert_eq!(support(Some((1, 74, 2))), Support::BelowRecommended);
        assert_eq!(support(Some((1, 74, 3))), Support::Ok);
        assert_eq!(support(Some((2, 0, 0))), Support::Ok);
        assert_eq!(support(None), Support::Unknown);
    }

    #[test]
    fn doctor_rows_fail_below_minimum_and_warn_below_recommended() {
        assert_eq!(diagnostic("rclone v1.60.1-DEV").status, "fail");
        assert_eq!(diagnostic("rclone v1.70.0").status, "warn");
        assert_eq!(diagnostic("rclone v1.75.1").status, "ok");
        assert_eq!(diagnostic("garbage").status, "warn");
    }
}
