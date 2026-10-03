//! Settings › Network: the global bandwidth timetable (rclone `--bwlimit`
//! syntax, validated with a preview of the limit in force now) and the
//! automatic keep-alive interval of mounts. Saved to `account_limits.json`,
//! which running mounts re-read within 30 s.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::storage::account::bandwidth::Timetable;
use crate::storage::account::limits::DEFAULT_KEEPALIVE_DAYS;
use eframe::egui;

/// Edit buffer for the Network settings tab, held in `GuiState::network`.
/// Loaded lazily from `account_limits.json` on first show.
#[derive(Debug, Default)]
pub(crate) struct NetworkForm {
    /// Set once `load` ran, so the store is read only on the first frame.
    loaded: bool,
    /// Bandwidth timetable text in rclone `--bwlimit` syntax; empty or `off` = no limit.
    pub(crate) timetable: String,
    /// Keep-alive interval for idle mount accounts, in days; 0 = never.
    pub(crate) keepalive_days: u32,
    /// Simultaneous shard uploads per account by default; 0 = built-in 16.
    pub(crate) default_uploads: u32,
    /// Simultaneous shard downloads per account by default; 0 = built-in 16.
    pub(crate) default_downloads: u32,
    /// Mounted drives on this PC upload small files as packs.
    pub(crate) small_file_packing: bool,
    /// Result of the last load/save (saved path or error), shown next to the Save button.
    pub(crate) notice: Option<String>,
}

/// The preview line for `text` at `now` (UTC offset `offset`), or why the
/// text is invalid.
pub(crate) fn preview(text: &str, now: u64, offset: Option<i64>) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("off") {
        return Ok(tr("No bandwidth limit.").into());
    }
    let table = Timetable::parse(text).map_err(|e| format!("{e:#}"))?;
    let (rate, next) = table.at_unix(now, offset.unwrap_or(0));
    let zone = offset.map_or_else(
        || tr("local time unknown, using UTC").to_string(),
        crate::utils::offset_label,
    );
    let mut line = trf(
        "Now: {rate} ({zone})",
        &[("rate", &describe(rate)), ("zone", &zone)],
    );
    if let Some(next) = next {
        line.push_str(&trf(
            " · next change in {wait}",
            &[(
                "wait",
                &crate::gui::screens::storage::providers::wait_text(next.saturating_sub(now)),
            )],
        ));
    }
    Ok(line)
}

/// Human-readable form of a timetable rate: one value when up and down
/// match, otherwise "up · down"; `None` means unlimited.
fn describe(rate: crate::storage::account::bandwidth::Rate) -> String {
    let one = |r: Option<u64>| match r {
        Some(bytes) => format!("{}/s", crate::presentation::format_bytes(bytes)),
        None => tr("unlimited").to_string(),
    };
    if rate.up == rate.down {
        one(rate.up)
    } else {
        trf(
            "{up} up · {down} down",
            &[("up", &one(rate.up)), ("down", &one(rate.down))],
        )
    }
}

/// Fills the form from the stored account limits (skipped in tests); a load
/// error goes to `notice` and leaves the defaults in place.
fn load(form: &mut NetworkForm) {
    form.loaded = true;
    form.keepalive_days = DEFAULT_KEEPALIVE_DAYS;
    if cfg!(test) {
        return;
    }
    match crate::storage::account::store::load_limits() {
        Ok(store) => {
            form.timetable = store.bandwidth.unwrap_or_default();
            form.keepalive_days = store.keepalive_days;
            form.default_uploads = store.default_max_uploads.unwrap_or(0);
            form.default_downloads = store.default_max_downloads.unwrap_or(0);
            form.small_file_packing = store.small_file_packing;
        }
        Err(error) => form.notice = Some(format!("{error:#}")),
    }
}

/// Writes the bandwidth, keep-alive and per-account transfer defaults back
/// into `account_limits.json`; 0 transfer defaults are stored as `None`.
/// Returns the saved path. Validates the timetable via `set_bandwidth`.
fn save(form: &NetworkForm) -> Result<std::path::PathBuf, String> {
    let mut store = crate::storage::account::store::load_limits().map_err(|e| format!("{e:#}"))?;
    crate::storage::account::edit::set_bandwidth(&mut store, &form.timetable)
        .map_err(|e| format!("{e:#}"))?;
    store.keepalive_days = form.keepalive_days;
    store.default_max_uploads = (form.default_uploads > 0).then_some(form.default_uploads);
    store.default_max_downloads = (form.default_downloads > 0).then_some(form.default_downloads);
    store.small_file_packing = form.small_file_packing;
    crate::storage::account::store::save_limits(&store).map_err(|e| format!("{e:#}"))
}

/// Shard concurrency: this PC's total, and the per-account defaults.
fn transfers(ui: &mut egui::Ui, form: &mut NetworkForm, workers: &mut usize) {
    use crate::storage::account::limits::MAX_UPLOADS;
    ui.strong(tr("Shard transfers"));
    theme::hint(ui, tr("Counted in shards: one shard is one request (RPool turns off chunk fan-out inside rclone)."));
    egui::Grid::new("network-transfers")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label(tr("Shard transfers (workers)")).on_hover_text(tr("Shards this PC transfers at once, uploads and downloads, shared by all files: drive, restore, verify, scrub, repair, drain and health checks."));
            ui.add(egui::DragValue::new(workers).range(1..=256));
            ui.end_row();
            ui.label(tr("Upload default per account")).on_hover_text(tr("Simultaneous shard uploads to one account unless its card (Providers › Limits) sets its own. 0 = built-in 16. Dropbox stays at 1 unless set on its card."));
            ui.add(egui::DragValue::new(&mut form.default_uploads).range(0..=MAX_UPLOADS));
            ui.end_row();
            ui.label(tr("Download default per account")).on_hover_text(tr("Simultaneous shard downloads from one account unless its card (Providers › Limits) sets its own. 0 = built-in 16."));
            ui.add(egui::DragValue::new(&mut form.default_downloads).range(0..=MAX_UPLOADS));
            ui.end_row();
        });
    ui.checkbox(&mut form.small_file_packing, tr("Pack small files"))
        .on_hover_text(tr("Mounted drives on this PC upload files of up to 1 MiB together as one archive per batch: far fewer requests and less space per small file. Applies within 30 s, no remount needed."));
    if form.small_file_packing {
        let (fill, fg) = theme::warning_colors(ui.visuals().dark_mode);
        egui::Frame::new()
            .fill(fill)
            .corner_radius(theme::CORNER_RADIUS)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(fg, tr("Every PC that uses these pools needs RPool 2.10 or later: older versions stop syncing a pool once it holds packed files. Turning this off later is safe; packed files stay readable."));
            });
    }
}

/// Renders the Network tab; called by `screens::settings::show`. Save writes
/// the GUI settings (worker count) first, then the account limits.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.network;
    if !form.loaded {
        load(form);
    }
    theme::card_section(
        ui,
        tr("Network & transfers"),
        Some(tr(
            "Bandwidth limits and how many shards move at once, for every RPool transfer on this computer.",
        )),
        |_| {},
        |ui| {
            ui.label(tr("Bandwidth timetable"));
            ui.add(
                egui::TextEdit::singleline(&mut form.timetable)
                    .hint_text("08:00,512k 18:00,30M 23:00,off")
                    .desired_width(ui.available_width().min(420.0)),
            );
            theme::hint(ui, tr("rclone --bwlimit syntax: a rate (10M, or 10M:2M for upload:download) or TIME,RATE entries in local time, optionally with a day (Mon-08:00,1M). off = unlimited."));
            let check = preview(
                &form.timetable,
                crate::utils::now_unix(),
                crate::utils::local_offset_seconds(),
            );
            match &check {
                Ok(line) => {
                    ui.label(egui::RichText::new(line).color(theme::pal(ui).muted));
                }
                Err(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            }
            ui.add_space(6.0);
            transfers(ui, form, &mut state.settings.workers);
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Keep idle accounts alive after"));
                ui.add(egui::DragValue::new(&mut form.keepalive_days).range(0..=3650));
                ui.label(tr("days (0 = never)"));
            });
            theme::hint(ui, tr("A mount makes one cheap authenticated call to each of its accounts when it was idle that long. Providers decide what counts as activity."));

            ui.horizontal_wrapped(|ui| {
                if theme::primary_button(ui, check.is_ok(), tr("Save network settings")).clicked() {
                    let saved = crate::gui::settings::save(&state.settings).map(|_| ());
                    form.notice = Some(match saved.and_then(|()| save(form)) {
                        Ok(path) => trf("Saved to {path}", &[("path", &path.display())]),
                        Err(error) => error,
                    });
                }
                if let Some(notice) = &form.notice {
                    ui.label(notice);
                }
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shows_the_current_rate_or_the_error() {
        crate::gui::i18n::set_language(crate::gui::i18n::Language::English);
        let thursday = 1_790_812_800; // 00:00 UTC
        assert_eq!(
            preview("", thursday, Some(0)).unwrap(),
            "No bandwidth limit."
        );
        assert_eq!(
            preview("off", thursday, Some(0)).unwrap(),
            "No bandwidth limit."
        );
        assert_eq!(
            preview("08:00,512k 23:00,off", thursday, Some(9 * 3600)).unwrap(),
            "Now: 512.00 KiB/s (UTC+09:00) · next change in 14 h 00 min"
        );
        assert_eq!(
            preview("10M:off", thursday, None).unwrap(),
            "Now: 10.00 MiB/s up · unlimited down (local time unknown, using UTC)"
        );
        assert!(preview("08:00,fast", thursday, Some(0)).is_err());
    }

    /// The Network tab fits a small window with a long, invalid timetable.
    #[test]
    fn network_section_fits_narrow_windows() {
        for (width, dark, text) in [
            (
                580.0,
                true,
                "08:00,512k 18:00,30M 23:00,off Mon-07:00,1M:off Fri-22:00,off",
            ),
            (580.0, false, "08:00,not-a-rate"),
            (1600.0, true, ""),
        ] {
            let mut state = crate::gui::state::GuiState::new(
                Default::default(),
                Default::default(),
                Default::default(),
            );
            state.network.timetable = text.into();
            let ctx = egui::Context::default();
            ctx.set_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            crate::gui::theme::apply(&ctx);
            let mut right = 0.0f32;
            for _ in 0..2 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 600.0),
                    )),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        show(ui, &mut state);
                        right = right.max(ui.min_rect().right());
                    });
                });
                output.textures_delta.clear();
            }
            assert!(right <= width + 1.0, "{width} {text:?}: {right}");
            assert!(state.network.loaded);
        }
    }
}
