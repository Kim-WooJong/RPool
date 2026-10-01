//! Sample connected providers for tests, the layout test and debug
//! snapshots (`RPOOL_GUI_SNAPSHOT_PROVIDERS=1`). No rclone is called.

use crate::gui::state::GuiState;
use crate::models::QuotaReport;

/// Name, backend type, (total, used) bytes when reported, encrypted.
type Account = (&'static str, &'static str, Option<(u64, u64)>, bool);

pub(crate) fn providers(state: &mut GuiState) {
    const GIB: u64 = 1 << 30;
    let accounts: [Account; 7] = [
        ("gdrive_1", "drive", Some((15 * GIB, 11 * GIB)), true),
        ("gdrive_2", "drive", Some((15 * GIB, 2 * GIB)), true),
        (
            "dropbox_main",
            "dropbox",
            Some((2 * GIB, 1_900_000_000)),
            true,
        ),
        (
            "onedrive_work",
            "onedrive",
            Some((1024 * GIB, 312 * GIB)),
            true,
        ),
        ("box_archive", "box", None, true),
        ("pcloud", "pcloud", Some((10 * GIB, 0)), true),
        ("mega_new", "mega", Some((20 * GIB, 5 * GIB)), false),
    ];
    state.backing_remotes.clear();
    state.usage_reports.clear();
    state.provider_details = Default::default();
    state.providers.missing_encryption.clear();
    for (name, kind, usage, encrypted) in accounts {
        state.backing_remotes.push(name.into());
        let details = &mut state.provider_details;
        details.kinds.insert(name.into(), kind.into());
        if encrypted {
            details
                .crypts
                .insert(name.into(), vec![format!("{name}_crypt:")]);
        } else {
            state.providers.missing_encryption.push(name.into());
        }
        state.usage_reports.push(QuotaReport {
            remote: format!("{name}:"),
            total: usage.map(|(total, _)| total),
            used: usage.map(|(_, used)| used),
            free: usage.map(|(total, used)| total - used),
            trashed: None,
            other: None,
            used_percent: usage.map(|(total, used)| used as f64 * 100.0 / total as f64),
            error: usage.is_none().then(|| "quota not supported".to_string()),
        });
    }
    state.providers.discovery_known = true;
}
