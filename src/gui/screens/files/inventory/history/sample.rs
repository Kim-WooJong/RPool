//! Sample trash, versions, rollback plan and retention for tests, the
//! layout test and debug snapshots (`RPOOL_GUI_SNAPSHOT_HISTORY=1`).

use super::query::Fetch;
use super::rollback_time::TimePreset;
use super::state::{RollbackDialog, VersionsPanel};
use crate::drive_history::model::{
    ChangeAction, CleanupAccount, CleanupMode, CleanupReport, CleanupSettings, CleanupTotals,
    Retention, RollbackChange, RollbackPlan, TrashEntry, VersionEntry, VersionKind,
    HISTORY_VERSION,
};

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

pub(crate) fn trash(now: u64) -> Vec<TrashEntry> {
    let entry =
        |id: &str, path: &str, is_dir, size, ago: Option<u64>, by: Option<&str>| TrashEntry {
            id: id.into(),
            path: path.into(),
            is_dir,
            size,
            deleted_unix: ago.map(|ago| now.saturating_sub(ago)),
            deleted_by: by.map(str::to_string),
            expires_unix: ago.map(|ago| now.saturating_sub(ago) + 30 * DAY),
            path_taken: false,
        };
    let mut entries = vec![
        entry(
            "t1",
            "/Documents/report.pdf",
            false,
            2_350_000,
            Some(2 * HOUR),
            Some("Studio-Mac"),
        ),
        entry(
            "t2",
            "/Photos/2024/IMG_0003.jpg",
            false,
            3_100_000,
            Some(DAY),
            Some("Laptop"),
        ),
        entry(
            "t3",
            "/Old projects",
            true,
            120_000_000,
            Some(28 * DAY),
            Some("Laptop"),
        ),
        entry(
            "t4",
            "/Docs/new-draft.txt",
            false,
            12_000,
            Some(600),
            Some("Studio-Mac"),
        ),
        entry(
            "t5",
            "/Music/a very long file name of a live recording 2019 (remastered).flac",
            false,
            45_000_000,
            Some(31 * DAY),
            Some("Living-room-PC"),
        ),
        entry("t6", "/notes-old.txt", false, 900, None, None),
    ];
    entries[0].path_taken = true;
    entries
}

/// Paths a rollback moved to the trash (for the "from a rollback" badge).
pub(crate) fn rollback_trashed() -> Vec<String> {
    vec!["/Docs/new-draft.txt".into()]
}

pub(crate) fn versions(path: &str, now: u64) -> Vec<VersionEntry> {
    let version =
        |id: &str, kind, size, ago: u64, author: &str, current, restorable| VersionEntry {
            id: id.into(),
            path: path.into(),
            kind,
            size,
            time_unix: Some(now - ago),
            author: Some(author.into()),
            current,
            restorable,
        };
    vec![
        version(
            "r6",
            VersionKind::Modified,
            3_000_000,
            HOUR,
            "Studio-Mac",
            true,
            true,
        ),
        version(
            "r5",
            VersionKind::Restored,
            2_900_000,
            DAY,
            "Laptop",
            false,
            true,
        ),
        version(
            "r4",
            VersionKind::Deleted,
            0,
            2 * DAY,
            "Laptop",
            false,
            false,
        ),
        version(
            "r3",
            VersionKind::Modified,
            2_900_000,
            3 * DAY,
            "Studio-Mac",
            false,
            true,
        ),
        version(
            "r2",
            VersionKind::Modified,
            2_700_000,
            10 * DAY,
            "Studio-Mac",
            false,
            true,
        ),
        version(
            "r1",
            VersionKind::Created,
            2_500_000,
            120 * DAY,
            "Living-room-PC",
            false,
            false,
        ),
    ]
}

pub(crate) fn rollback_plan(scope: &str, at: u64) -> RollbackPlan {
    let base = scope.trim_end_matches('/');
    let change = |name: &str, action, size| RollbackChange {
        path: format!("{base}/{name}"),
        action,
        revision: format!("rev-{name}"),
        size,
    };
    RollbackPlan {
        version: HISTORY_VERSION,
        pool: "family".into(),
        scope: if scope.is_empty() {
            "/".into()
        } else {
            scope.into()
        },
        at_unix: at,
        changes: vec![
            change("report.pdf", ChangeAction::Revert, 2_400_000),
            change("budget.xlsx", ChangeAction::Revert, 80_000),
            change("drafts/chapter-1.docx", ChangeAction::Revert, 30_000),
            change("old-notes.txt", ChangeAction::Undelete, 4_000),
            change("drafts/outline.md", ChangeAction::Undelete, 2_000),
            change("new-draft.txt", ChangeAction::Remove, 12_000),
            change("scratch/tmp.log", ChangeAction::Remove, 200_000),
        ],
        skipped: vec![(
            format!("{base}/archive-2019.zip"),
            "the old version's data was purged".into(),
        )],
        applied: false,
    }
}

pub(crate) fn retention() -> Retention {
    Retention {
        trash_days: 30,
        keep_versions: 20,
        version_days: 0,
    }
}

/// A cleanup preview: some data reclaimable, some waiting, some due, and
/// the mass-delete guard would stop the due part.
pub(crate) fn cleanup_report(now: u64) -> CleanupReport {
    let totals = |archives, files, bytes| CleanupTotals {
        archives,
        files,
        objects: archives * 3,
        bytes,
    };
    CleanupReport {
        version: HISTORY_VERSION,
        pool: "family".into(),
        mode: CleanupMode::Preview,
        candidates: totals(12, 9, 1_800_000_000),
        waiting: totals(40, 31, 6_400_000_000),
        due: totals(3, 3, 250_000_000),
        deleted: CleanupTotals::default(),
        released: 0,
        next_deletion_unix: Some(now + 3 * DAY),
        accounts: vec![
            CleanupAccount {
                account: "gdrive-family-photos".into(),
                objects: 90,
                bytes: 5_000_000_000,
            },
            CleanupAccount {
                account: "onedrive".into(),
                objects: 75,
                bytes: 3_450_000_000,
            },
        ],
        guard: Some("120 objects exceed the limit of 100 per run".into()),
        postponed: Vec::new(),
        settings: CleanupSettings::default(),
        notes: Vec::new(),
    }
}

/// What the sample shows in Library › Drive files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fixture {
    /// The trash with items, two of them selected.
    Trash,
    /// The drive with the versions panel of a photo open.
    Versions,
    /// The rollback dialog of "Documents" with a preview.
    Rollback,
    /// The same at the second confirmation step.
    RollbackConfirm,
}

/// Loads the sample drive as `pool`'s listing with `fixture` open.
pub(crate) fn show(
    form: &mut crate::gui::screens::files::inventory::drive_state::DriveForm,
    pool: &str,
    fixture: Fixture,
) {
    use crate::gui::screens::files::inventory::explorer::sample::{self as drive, SampleView};
    let now = crate::utils::now_unix();
    let view = match fixture {
        Fixture::Versions => SampleView {
            folder: "Photos/2024",
            selected: Some("Photos/2024/IMG_0001.jpg"),
            ..Default::default()
        },
        Fixture::Rollback | Fixture::RollbackConfirm => SampleView {
            folder: "Documents",
            ..Default::default()
        },
        Fixture::Trash => SampleView::default(),
    };
    drive::show(form, pool, view);
    let history = &mut form.history;
    history.trash_open = fixture == Fixture::Trash;
    let state = history.pool(pool);
    state.trash = Fetch::ready(trash(now));
    state.from_rollback = rollback_trashed().into_iter().collect();
    state.selection.clear();
    let order = ["t1", "t4"];
    state.selection.click("t1", &order, false, false);
    state.selection.click("t4", &order, true, false);
    state.retention = Fetch::ready(retention());
    history.versions = (fixture == Fixture::Versions).then(|| {
        let path = "/Photos/2024/IMG_0001.jpg";
        VersionsPanel {
            pool: pool.to_string(),
            path: path.into(),
            fetch: Fetch::ready(versions(path, now)),
            selected: Some("r3".into()),
        }
    });
    history.rollback =
        matches!(fixture, Fixture::Rollback | Fixture::RollbackConfirm).then(|| RollbackDialog {
            pool: pool.to_string(),
            scope: "/Documents".into(),
            preset: TimePreset::DayAgo,
            custom: String::new(),
            preview: Fetch::ready(rollback_plan("/Documents", now - DAY)),
            confirming: fixture == Fixture::RollbackConfirm,
        });
}

/// Storage › Pools with `pool` loaded and its retention read.
pub(crate) fn retention_card(state: &mut crate::gui::state::GuiState, pool: &str) {
    state.page = crate::gui::state::Page::Storage;
    state.storage_section = crate::gui::state::StorageSection::Pools;
    state.pools.selected = pool.to_string();
    state.pool_definitions.entry(pool.to_string()).or_default();
    state.inventory.drive.history.reveal_retention = true;
    let history = state.inventory.drive.history.pool(pool);
    history.retention = Fetch::ready(retention());
    history.cleanup = Fetch::ready(cleanup_report(crate::utils::now_unix()));
    history.cleanup_confirm = super::state::CleanupConfirm::Guard;
    history.retention_draft = Some(Retention {
        version_days: 0,
        keep_versions: 0,
        ..retention()
    });
}
