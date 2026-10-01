//! A nested sample drive for tests, the layout test and debug snapshots
//! (`RPOOL_GUI_SNAPSHOT_LIBRARY=1`).

use super::nav::Nav;
use super::state::ViewMode;
use crate::gui::screens::files::inventory::drive_state::{DriveForm, DriveLoad};
use crate::pool::browse::{BrowseEntry, PoolBrowse};

/// How the sample drive is shown.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SampleView {
    pub(crate) folder: &'static str,
    pub(crate) selected: Option<&'static str>,
    pub(crate) icons: bool,
    pub(crate) query: &'static str,
    pub(crate) search_all: bool,
}

/// Loads the sample drive as `pool`'s listing and opens it as `view` says.
pub(crate) fn show(form: &mut DriveForm, pool: &str, view: SampleView) {
    form.select(pool.to_string());
    if !matches!(form.current(), Some(DriveLoad::Ready(_))) {
        form.apply(pool.to_string(), Ok(browse()));
    }
    let explorer = &mut form.explorer;
    if explorer.nav.current() != view.folder {
        explorer.nav = Nav::default();
        explorer.nav.open(view.folder);
    }
    explorer.view = if view.icons {
        ViewMode::Icons
    } else {
        ViewMode::List
    };
    explorer.selected = view.selected.map(str::to_string);
    explorer.search_all = view.search_all;
    form.query = view.query.to_string();
}

pub(crate) fn browse() -> PoolBrowse {
    let file = |path: &str, size: u64| BrowseEntry {
        path: path.into(),
        size,
        is_dir: false,
    };
    let mut entries = vec![
        file("Documents/report.pdf", 2_400_000),
        file("Documents/drafts/chapter-1.docx", 30_000),
        file("Music/song.mp3", 1_200_000),
        file("Photos/2024/IMG_0001.jpg", 3_000_000),
        file("Photos/2024/IMG_0002.jpg", 2_900_000),
        file("Photos/2024/trip/beach.png", 4_000_000),
        file("Photos/cover.png", 500_000),
        file("archive.tar.zst", 50_000_000),
        file("notes.txt", 1_200),
        file("README", 4_096),
    ];
    for n in 1..=30 {
        entries.push(file(
            &format!("Photos/2024/burst/DSC_{n:04}_a_rather_long_camera_file_name.jpg"),
            100_000,
        ));
    }
    PoolBrowse {
        pool: "family".into(),
        mode: "v6".into(),
        entries,
        notes: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn tree() -> crate::gui::screens::files::inventory::drive_state::DriveTree {
    crate::gui::screens::files::inventory::drive_state::DriveTree::build(browse())
}
