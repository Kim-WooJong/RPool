//! Text output of `rpool provider storage list|clean`.
use super::format_bytes;
use crate::provider::storage::model::{CleanMode, CleanReport, Group, StorageView, Totals};

/// "kept", "deletable", "deletable (1 h grace)", "deletable with --flag" for a group.
fn status(group: &Group) -> String {
    match (group.deletable, group.needs.as_slice()) {
        (false, _) => "kept".into(),
        (true, []) if group.grace_seconds.is_some() => format!(
            "deletable ({} h grace)",
            group.grace_seconds.unwrap_or_default().div_ceil(3600)
        ),
        (true, []) => "deletable".into(),
        (true, needs) => format!(
            "deletable with {}",
            needs.iter().map(|a| a.flag()).collect::<Vec<_>>().join(" ")
        ),
    }
}

/// The grouped view as text: one block per group (id, size, status, then
/// the reason and pending marks indented).
pub(crate) fn view_text(view: &StorageView) -> String {
    let mut out = format!(
        "{} — {} in {} objects{}\n\n",
        view.remote,
        format_bytes(view.bytes),
        view.objects,
        if view.pools.is_empty() {
            "; no configured pool uses this provider".to_string()
        } else {
            format!("; used by pool(s) {}", view.pools.join(", "))
        }
    );
    for group in &view.groups {
        out.push_str(&format!(
            "{:<32} {:>9} objects {:>11}  {}\n    {}\n",
            group.id,
            group.objects,
            format_bytes(group.bytes),
            status(group),
            group.reason
        ));
        let pending = &group.pending;
        if pending.marked > 0 {
            let due = pending
                .last_due_unix
                .map(crate::drive_history::time_arg::format)
                .unwrap_or_default();
            out.push_str(&format!(
                "    {} unit(s) marked ({}), deletable from {due}{}\n",
                pending.marked,
                format_bytes(pending.bytes),
                if pending.deleting > 0 {
                    format!("; deletion of {} started", pending.deleting)
                } else {
                    String::new()
                }
            ));
        }
    }
    out.push('\n');
    if let Some(count) = view.undecryptable.filter(|c| *c > 0) {
        out.push_str(&format!(
            "{count} name(s) at the provider root do not decrypt with this crypt remote (another key or name encoding); they are never touched.\n"
        ));
    }
    for source in &view.uncertain {
        out.push_str(&format!("Could not read: {source}\n"));
    }
    for note in &view.notes {
        out.push_str(&format!("{note}\n"));
    }
    out.push_str(&format!(
        "Delete with `rpool provider storage clean --remote {} --group <id>` (preview; add --confirm). Marked data is deleted {} day(s) later (speed-test leftovers: 1 hour).\n",
        view.remote, view.grace_days
    ));
    out
}

/// One "label: size (units, objects)" line.
fn line(label: &str, totals: &Totals) -> String {
    format!(
        "{label}: {} ({} unit(s), {} objects)\n",
        format_bytes(totals.bytes),
        totals.units,
        totals.objects
    )
}

/// A cleanup report as text.
pub(crate) fn clean_text(report: &CleanReport) -> String {
    let mut out = format!("{} — groups: {}\n", report.remote, report.groups.join(", "));
    match report.mode {
        CleanMode::Postponed => {
            out.push_str("Postponed: nothing was marked or deleted.\n");
            for source in &report.postponed {
                out.push_str(&format!("Could not read: {source}\n"));
            }
        }
        CleanMode::Cancelled => {
            out.push_str(&format!("Released {} mark(s).\n", report.released));
        }
        CleanMode::Preview | CleanMode::Applied => {
            let applied = report.mode == CleanMode::Applied;
            out.push_str(&line(
                if applied { "Marked now" } else { "To mark" },
                &report.new,
            ));
            out.push_str(&line("Waiting for the grace period", &report.waiting));
            out.push_str(&line(
                if applied {
                    "Due (deleted unless refused)"
                } else {
                    "Due for deletion"
                },
                &report.due,
            ));
            if applied {
                out.push_str(&line("Deleted", &report.deleted));
            }
            if report.released > 0 {
                out.push_str(&format!(
                    "Released {} mark(s): the data is needed again.\n",
                    report.released
                ));
            }
        }
    }
    if let Some(next) = report.next_deletion_unix {
        out.push_str(&format!(
            "Next deletion: {}\n",
            crate::drive_history::time_arg::format(next)
        ));
    }
    if let Some(guard) = &report.guard {
        out.push_str(&format!(
            "Mass-delete guard: {guard}; add --force to delete anyway.\n"
        ));
    }
    for note in &report.notes {
        out.push_str(&format!("{note}\n"));
    }
    if report.mode == CleanMode::Preview {
        out.push_str(&format!(
            "Preview only: add --confirm to mark (deleted {} day(s) later) and delete due data.\n",
            report.grace_days
        ));
    }
    out
}
