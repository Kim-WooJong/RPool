//! Every page and sub-tab renders at a small, a medium and a large window,
//! in dark and light mode, without panics or content wider than the window.
use super::screens::{dashboard, files, jobs, maintenance, monitoring, settings, storage};
use super::state::{DriveTab, FilesSection, GuiState, MaintenanceSection, Page, StorageSection};
use super::task::TaskRunner;
use super::usage_refresh::UsageRefresh;
use eframe::egui;
use files::inventory::drive_sample::SampleView;
use storage::migration::state::Step;

fn state() -> GuiState {
    let pools = std::collections::BTreeMap::from([(
        "family".to_string(),
        crate::models::PoolDefinition::default(),
    )]);
    GuiState::new(Default::default(), Default::default(), pools)
}

/// Account changes with the migration wizard at `step` (fake data) and the
/// Advanced / manual section open.
fn migration(state: &mut GuiState, step: Step) {
    state.page = Page::Storage;
    state.storage_section = StorageSection::Changes;
    state.migration = storage::migration::tests::sample_form("family", step);
    state.migration.show_advanced = true;
}

/// Files › Library on the nested sample drive of the pool "family".
fn library(state: &mut GuiState, view: SampleView) {
    state.page = Page::Files;
    state.files_section = FilesSection::Inventory;
    files::inventory::drive_sample::show(&mut state.inventory.drive, "family", view);
}

/// Files › Library with a trash/versions/rollback sample open.
fn history(state: &mut GuiState, fixture: files::inventory::history::sample::Fixture) {
    state.page = Page::Files;
    state.files_section = FilesSection::Inventory;
    files::inventory::history::sample::show(&mut state.inventory.drive, "family", fixture);
}

fn page(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner, usage: &mut UsageRefresh) {
    match state.page {
        Page::Dashboard => dashboard::show(ui, state, usage),
        Page::Drive => storage::mount::show(ui, state),
        Page::Files => files::show(ui, state, task),
        Page::Storage => storage::show(ui, state, task),
        Page::Monitoring => monitoring::show(ui, state),
        Page::Maintenance => maintenance::show(ui, state, task),
        Page::Jobs => jobs::show(ui, state, task),
        Page::Settings => settings::show(ui, state, task),
    }
}

/// Renders twice (layout settles on the second pass) and returns the
/// widest point any page content reached.
fn render(state: &mut GuiState, size: egui::Vec2, dark: bool) -> f32 {
    let ctx = egui::Context::default();
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    super::theme::apply(&ctx);
    let (mut task, mut usage) = (TaskRunner::default(), UsageRefresh::default());
    let mut right = 0.0f32;
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                page(ui, state, &mut task, &mut usage);
                right = right.max(ui.min_rect().right());
            });
        });
        output.textures_delta.clear();
    }
    right
}

#[test]
fn every_page_fits_small_medium_and_large_windows() {
    let pages: &[fn(&mut GuiState)] = &[
        |s| s.page = Page::Dashboard,
        |s| {
            s.page = Page::Drive;
            s.mount.tab = DriveTab::Drive
        },
        |s| {
            storage::mount::session_tests::with_two_sessions(s);
            s.page = Page::Drive;
            s.mount.tab = DriveTab::Drive
        },
        |s| {
            s.page = Page::Drive;
            s.mount.tab = DriveTab::Options
        },
        |s| {
            s.page = Page::Drive;
            s.mount.tab = DriveTab::History
        },
        |s| {
            s.page = Page::Drive;
            s.mount.tab = DriveTab::Import
        },
        |s| {
            s.page = Page::Drive;
            s.mount.tab = DriveTab::Maintenance
        },
        |s| {
            s.page = Page::Files;
            s.files_section = FilesSection::Inventory
        },
        |s| library(s, Default::default()),
        |s| {
            library(
                s,
                SampleView {
                    folder: "Photos/2024",
                    selected: Some("Photos/2024/IMG_0001.jpg"),
                    icons: true,
                    ..Default::default()
                },
            )
        },
        |s| {
            library(
                s,
                SampleView {
                    folder: "Photos/2024/burst",
                    selected: Some("Photos/2024/burst/DSC_0003_a_rather_long_camera_file_name.jpg"),
                    query: "jpg",
                    search_all: true,
                    ..Default::default()
                },
            )
        },
        |s| history(s, files::inventory::history::sample::Fixture::Trash),
        |s| history(s, files::inventory::history::sample::Fixture::Versions),
        |s| history(s, files::inventory::history::sample::Fixture::Rollback),
        |s| {
            history(
                s,
                files::inventory::history::sample::Fixture::RollbackConfirm,
            )
        },
        |s| files::inventory::history::sample::retention_card(s, "family"),
        |s| {
            s.page = Page::Files;
            s.files_section = FilesSection::Upload
        },
        |s| {
            s.page = Page::Files;
            s.files_section = FilesSection::Restore
        },
        |s| {
            s.page = Page::Storage;
            s.storage_section = StorageSection::Providers
        },
        |s| {
            storage::providers::sample::providers(s);
            s.page = Page::Storage;
            s.storage_section = StorageSection::Providers
        },
        |s| {
            storage::providers::sample::with_limits_editor(s);
            s.page = Page::Storage;
            s.storage_section = StorageSection::Providers
        },
        |s| {
            s.page = Page::Storage;
            s.storage_section = StorageSection::Pools
        },
        |s| {
            s.page = Page::Storage;
            s.storage_section = StorageSection::Changes
        },
        |s| {
            storage::speed_test::tests::with_sample_result(s);
            s.page = Page::Storage;
            s.storage_section = StorageSection::Pools
        },
        |s| {
            storage::speed_test::tests::with_sample_result(s);
            s.page = Page::Storage;
            s.storage_section = StorageSection::Providers
        },
        |s| migration(s, Step::Plan),
        |s| migration(s, Step::Review),
        |s| migration(s, Step::Run),
        |s| migration(s, Step::Adopt),
        |s| migration(s, Step::Lost),
        |s| {
            migration(s, Step::Cleanup);
            s.migration = storage::migration::cleanup_tests::sample_cleanup_form("family");
            s.migration.cleanup.confirm_force =
                Some(storage::migration::cleanup_state::CleanupAction::Quarantine);
        },
        |s| {
            s.page = Page::Maintenance;
            s.maintenance_section = MaintenanceSection::Archive
        },
        |s| {
            s.page = Page::Maintenance;
            s.maintenance_section = MaintenanceSection::Integrity
        },
        |s| {
            s.page = Page::Maintenance;
            s.maintenance_section = MaintenanceSection::Metadata
        },
        |s| {
            s.page = Page::Maintenance;
            s.maintenance_section = MaintenanceSection::Diagnostics
        },
        |s| s.page = Page::Monitoring,
        |s| {
            s.page = Page::Monitoring;
            s.monitoring = monitoring::sample::state(crate::utils::now_unix(), false);
        },
        |s| {
            s.page = Page::Monitoring;
            s.monitoring = monitoring::sample::state(crate::utils::now_unix(), true);
        },
        |s| s.page = Page::Jobs,
        |s| s.page = Page::Settings,
    ];
    let mut overflow: Vec<String> = Vec::new();
    for (index, set) in pages.iter().enumerate() {
        for size in [
            egui::vec2(580.0, 420.0),
            egui::vec2(960.0, 680.0),
            egui::vec2(1600.0, 1000.0),
        ] {
            for dark in [true, false] {
                let mut state = state();
                set(&mut state);
                let right = render(&mut state, size, dark);
                if right > size.x + 1.0 {
                    overflow.push(format!(
                        "page {index} at {size:?} dark={dark} reaches x={right}"
                    ));
                }
            }
        }
    }
    assert!(overflow.is_empty(), "{overflow:#?}");
}
