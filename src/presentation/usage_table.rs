use crate::prelude::*;
use crate::presentation::{format_optional_bytes, usage_bar};

pub(crate) fn print_usage_table(reports: &[QuotaReport]) {
    let remote_width = reports
        .iter()
        .map(|r| r.remote.len())
        .max()
        .unwrap_or(6)
        .max(6);

    println!(
        "{:<remote_width$}  {:>10}  {:>10}  {:>10}  {:>7}  {:<12}  {:>10}  STATUS",
        "REMOTE",
        "USED",
        "FREE",
        "TOTAL",
        "USED%",
        "USAGE",
        "TRASH",
        remote_width = remote_width
    );
    println!(
        "{}  {}  {}  {}  {}  {}  {}  {}",
        "-".repeat(remote_width),
        "-".repeat(10),
        "-".repeat(10),
        "-".repeat(10),
        "-".repeat(7),
        "-".repeat(12),
        "-".repeat(10),
        "-".repeat(18)
    );

    for report in reports {
        let status = report.error.as_deref().unwrap_or("ok");
        println!(
            "{:<remote_width$}  {:>10}  {:>10}  {:>10}  {:>7}  {:<12}  {:>10}  {}",
            report.remote,
            format_optional_bytes(report.used),
            format_optional_bytes(report.free),
            format_optional_bytes(report.total),
            report
                .used_percent
                .map(|v| format!("{v:.1}%"))
                .unwrap_or_else(|| "n/a".to_string()),
            usage_bar(report.used_percent),
            format_optional_bytes(report.trashed),
            status,
            remote_width = remote_width
        );
    }
}
