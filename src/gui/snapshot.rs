//! Debug builds only: `RPOOL_GUI_SNAPSHOTS=<dir> rpool gui` visits every page
//! at several window sizes, saves each frame as `<dir>/<size>-<page>.ppm`,
//! then closes. Used to review the layout without screen-recording rights.
use super::state::{DriveTab, FilesSection, GuiState, MaintenanceSection, Page, StorageSection};
use eframe::egui;
use std::path::PathBuf;

const SIZES: [(&str, [f32; 2]); 3] = [
    ("wide", [1440.0, 900.0]),
    ("medium", [1024.0, 720.0]),
    ("narrow", [720.0, 640.0]),
];
/// Frames to wait after a change so layout and fonts settle.
const SETTLE: u32 = 12;

type Setter = fn(&mut GuiState);
const PAGES: &[(&str, Setter)] = &[
    ("overview", |s| s.page = Page::Dashboard),
    ("drive", |s| {
        s.page = Page::Drive;
        s.mount.tab = DriveTab::Drive;
    }),
    ("drive-options", |s| {
        s.page = Page::Drive;
        s.mount.tab = DriveTab::Options;
    }),
    ("drive-history", |s| {
        s.page = Page::Drive;
        s.mount.tab = DriveTab::History;
    }),
    ("drive-import", |s| {
        s.page = Page::Drive;
        s.mount.tab = DriveTab::Import;
    }),
    ("files-library", |s| {
        s.page = Page::Files;
        s.files_section = FilesSection::Inventory;
    }),
    ("files-upload", |s| {
        s.page = Page::Files;
        s.files_section = FilesSection::Upload;
    }),
    ("storage-providers", |s| {
        s.page = Page::Storage;
        s.storage_section = StorageSection::Providers;
    }),
    ("storage-pools", |s| {
        s.page = Page::Storage;
        s.storage_section = StorageSection::Pools;
    }),
    ("storage-changes", |s| {
        s.page = Page::Storage;
        s.storage_section = StorageSection::Changes;
    }),
    ("health-archive", |s| {
        s.page = Page::Maintenance;
        s.maintenance_section = MaintenanceSection::Archive;
    }),
    ("health-integrity", |s| {
        s.page = Page::Maintenance;
        s.maintenance_section = MaintenanceSection::Integrity;
    }),
    ("activity", |s| s.page = Page::Jobs),
    ("settings", |s| s.page = Page::Settings),
];

pub(crate) struct Snapshots {
    dir: PathBuf,
    step: usize,
    wait: u32,
    requested: bool,
}

impl Snapshots {
    pub(crate) fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("RPOOL_GUI_SNAPSHOTS")?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self {
            dir,
            step: 0,
            // The first page also waits for pools and providers to load.
            wait: SETTLE * 10,
            requested: false,
        })
    }

    fn current(&self) -> Option<(&'static str, [f32; 2], &'static str, Setter)> {
        let (size, dims) = SIZES.get(self.step / PAGES.len())?;
        let (page, set) = PAGES[self.step % PAGES.len()];
        Some((size, *dims, page, set))
    }

    /// Call once per frame before drawing.
    pub(crate) fn tick(&mut self, ctx: &egui::Context, state: &mut GuiState) {
        if self.step == 0 && self.wait == SETTLE * 10 {
            // Show real content: the first saved pool, as a user would pick it.
            if let Some(name) = state.pool_names.first().cloned() {
                state.mount.select_pool(name.clone(), &mut state.settings);
                state.pools.selected = name;
                super::screens::storage::pools::load_selected(state);
            }
            // `RPOOL_GUI_SNAPSHOT_SPEEDTEST=<report.json>` shows that speed
            // test result on the Pools and Providers cards.
            if let Some(path) = std::env::var_os("RPOOL_GUI_SNAPSHOT_SPEEDTEST") {
                use super::screens::storage::speed_test::{ReportView, Target};
                match std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                {
                    Some(report) => {
                        let report: crate::speedtest::model::SpeedTestReport = report;
                        let names: Vec<String> =
                            report.remotes.iter().map(|r| r.remote.clone()).collect();
                        if state.crypt_remotes.is_empty() {
                            state.crypt_remotes = names.clone();
                        }
                        state.speed_test.remotes = names;
                        let view = ReportView::new(&report);
                        let results = &mut state.speed_test.results;
                        if let Some(pool) = state.pool_names.first() {
                            results.insert(Target::Pool(pool.clone()), view.clone());
                        }
                        results.insert(Target::Remotes, view);
                    }
                    None => eprintln!("snapshot: cannot read speed test report {path:?}"),
                }
            }
        }
        let Some((size, dims, page, set)) = self.current() else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        set(state);
        if self.wait == SETTLE || self.step == 0 && self.wait == SETTLE * 10 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(dims.into()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            let path = self.dir.join(format!("{size}-{page}.ppm"));
            if let Err(e) = write_ppm(&path, &image) {
                eprintln!("snapshot {}: {e}", path.display());
            }
            self.step += 1;
            self.wait = SETTLE;
            self.requested = false;
        } else if self.wait > 0 {
            self.wait -= 1;
        } else if !self.requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.requested = true;
        }
        ctx.request_repaint();
    }
}

fn write_ppm(path: &std::path::Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.reserve(w * h * 3);
    for pixel in &image.pixels {
        out.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
    }
    std::fs::write(path, out)
}
