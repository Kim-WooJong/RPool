use crate::prelude::*;
use crate::utils::json_u64;
pub(super) fn parse(remote: &str, output: &[u8]) -> QuotaReport {
    let value: Value = match serde_json::from_slice(output) {
        Ok(value) => value,
        Err(error) => {
            return QuotaReport {
                remote: remote.to_string(),
                total: None,
                used: None,
                free: None,
                trashed: None,
                other: None,
                used_percent: None,
                error: Some(format!("invalid rclone about JSON: {error}")),
            }
        }
    };

    let raw_total = json_u64(&value, "total");
    let raw_used = json_u64(&value, "used");
    let raw_free = json_u64(&value, "free");
    let total = raw_total.or_else(|| match (raw_used, raw_free) {
        (Some(used), Some(free)) => used.checked_add(free),
        _ => None,
    });
    let used = raw_used.or_else(|| match (total, raw_free) {
        (Some(total), Some(free)) => Some(total.saturating_sub(free)),
        _ => None,
    });
    let free = raw_free.or_else(|| match (total, used) {
        (Some(total), Some(used)) => Some(total.saturating_sub(used)),
        _ => None,
    });
    let trashed = json_u64(&value, "trashed");
    let other = json_u64(&value, "other");
    let used_percent = match (used, total) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 / total as f64 * 100.0),
        _ => None,
    };

    QuotaReport {
        remote: remote.to_string(),
        total,
        used,
        free,
        trashed,
        other,
        used_percent,
        error: None,
    }
}
