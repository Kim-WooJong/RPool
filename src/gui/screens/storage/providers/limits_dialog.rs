//! "Limits…" editor of one account: daily upload budget, bandwidth,
//! request rate and inactivity warning. Validation produces the same
//! [`LimitEdit`] the CLI applies, and the dialog saves it in-process.
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::storage::account::edit::{self, DailyEdit, LimitEdit};
use crate::storage::account::limits::{
    default_daily_upload, default_inactivity_days, DailyUpload, LimitsStore,
};
use eframe::egui;

/// Bytes per GiB, for the daily budget field.
const GIB: f64 = (1u64 << 30) as f64;

/// Choice for a limit that has a backend default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    /// Use the backend's default.
    #[default]
    Default,
    /// Use the value entered in the dialog.
    Custom,
    /// No limit (daily upload) or no warning (inactivity).
    Off,
}

/// State of the "Limits…" dialog for one account (`ProviderForm::limits_editor`).
#[derive(Debug, Clone, Default)]
pub(crate) struct LimitsEditor {
    /// Whether the dialog is shown.
    pub(crate) open: bool,
    /// Account (backing remote) being edited.
    pub(crate) account: String,
    /// Backend type; picks the defaults shown.
    pub(crate) kind: String,
    /// Daily upload budget mode.
    pub(crate) daily: Mode,
    /// Custom daily upload budget, GiB.
    pub(crate) daily_gib: f64,
    /// Bandwidth limit in rclone `--bwlimit` syntax; empty or `off` = none.
    pub(crate) bwlimit: String,
    /// Requests per second (`--tpslimit`); 0 = none.
    pub(crate) tpslimit: f64,
    /// 0 = backend default.
    pub(crate) max_uploads: u32,
    /// 0 = the default.
    pub(crate) max_downloads: u32,
    /// Inactivity warning mode.
    pub(crate) warn: Mode,
    /// Custom inactivity warning, days (at least 1).
    pub(crate) warn_days: u32,
    /// Save result or validation error, shown in the dialog.
    pub(crate) notice: Option<String>,
}

impl LimitsEditor {
    /// Opens the editor with the account's saved overrides.
    pub(crate) fn open_for(account: &str, kind: &str, store: &LimitsStore) -> Self {
        let own = store.account(account).cloned().unwrap_or_default();
        let default_gib = default_daily_upload(kind).map_or(100.0, |b| b as f64 / GIB);
        let (daily, daily_gib) = match own.daily_upload {
            None => (Mode::Default, default_gib),
            Some(DailyUpload::Unlimited) => (Mode::Off, default_gib),
            Some(DailyUpload::Bytes(bytes)) => (Mode::Custom, bytes as f64 / GIB),
        };
        let default_days = default_inactivity_days(kind).unwrap_or(365);
        let (warn, warn_days) = match own.inactivity_warn_days {
            None => (Mode::Default, default_days),
            Some(0) => (Mode::Off, default_days),
            Some(days) => (Mode::Custom, days),
        };
        Self {
            open: true,
            account: account.to_owned(),
            kind: kind.to_owned(),
            daily,
            daily_gib: (daily_gib * 10.0).round() / 10.0,
            bwlimit: own.bwlimit.unwrap_or_default(),
            tpslimit: own.tpslimit.unwrap_or(0.0),
            max_uploads: own.max_uploads.unwrap_or(0),
            max_downloads: own.max_downloads.unwrap_or(0),
            warn,
            warn_days,
            notice: None,
        }
    }
    /// The full edit (every field set), or the first invalid value.
    pub(crate) fn edit(&self) -> Result<LimitEdit, String> {
        let bwlimit = self.bwlimit.trim();
        if !bwlimit.is_empty() && !bwlimit.eq_ignore_ascii_case("off") {
            crate::storage::account::bandwidth::validate(bwlimit)
                .map_err(|e| trf("Bandwidth: {error}", &[("error", &e)]))?;
        }
        if !self.tpslimit.is_finite() || self.tpslimit < 0.0 {
            return Err(tr("Requests per second must be zero or more.").into());
        }
        Ok(LimitEdit {
            daily: Some(match self.daily {
                Mode::Default => DailyEdit::BackendDefault,
                Mode::Off => DailyEdit::Unlimited,
                Mode::Custom if self.daily_gib > 0.0 => DailyEdit::Gib(self.daily_gib),
                Mode::Custom => {
                    return Err(tr("The daily upload limit must be more than 0 GiB.").into())
                }
            }),
            bwlimit: Some(bwlimit.to_owned()),
            tpslimit: Some(self.tpslimit),
            max_uploads: Some(self.max_uploads),
            max_downloads: Some(self.max_downloads),
            inactivity_warn_days: match self.warn {
                Mode::Default => None,
                Mode::Off => Some(0),
                Mode::Custom => Some(self.warn_days.max(1)),
            },
        })
    }
    /// The settings with this account's limits replaced by the editor's.
    pub(crate) fn apply_to(&self, store: &LimitsStore) -> Result<LimitsStore, String> {
        let change = self.edit()?;
        let mut next = store.clone();
        next.accounts.remove(&self.account);
        edit::apply(&mut next, &self.account, &change).map_err(|e| format!("{e:#}"))?;
        Ok(next)
    }
}

/// Default / Custom / Off radio buttons for `mode`.
fn mode_row(ui: &mut egui::Ui, mode: &mut Mode, default: &str, off: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(mode, Mode::Default, default);
        ui.radio_value(mode, Mode::Custom, tr("Custom"));
        ui.radio_value(mode, Mode::Off, off);
    });
}

/// Draws the dialog; returns true when the limits were saved.
pub(crate) fn show(ctx: &egui::Context, editor: &mut LimitsEditor) -> bool {
    if !editor.open {
        return false;
    }
    let mut open = true;
    let mut saved = false;
    let title = trf("Limits of {account}", &[("account", &editor.account)]);
    egui::Window::new(title)
        .id(egui::Id::new("provider-limits-editor"))
        .open(&mut open)
        .resizable(true)
        .default_width(440.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                ui.strong(tr("Daily upload limit (rolling 24 h)"));
                let default_label = match default_daily_upload(&editor.kind) {
                    Some(bytes) => trf(
                        "Backend default ({limit})",
                        &[("limit", &crate::presentation::format_bytes(bytes))],
                    ),
                    None => tr("Backend default (none)").to_string(),
                };
                mode_row(ui, &mut editor.daily, &default_label, tr("No limit"));
                if editor.daily == Mode::Custom {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut editor.daily_gib).range(0.1..=1_000_000.0).speed(1.0));
                        ui.label(tr("GiB per 24 h"));
                    });
                }
                theme::hint(ui, tr("When the limit is reached, uploads to this account wait and resume by themselves; nothing is lost. Counted per computer."));
                ui.separator();
                ui.strong(tr("Bandwidth for this account"));
                ui.add(egui::TextEdit::singleline(&mut editor.bwlimit).hint_text("08:00,512k 18:00,10M:off").desired_width(ui.available_width().min(360.0)));
                theme::hint(ui, tr("rclone --bwlimit syntax; empty or off = only the global limit."));
                ui.horizontal(|ui| {
                    ui.label(tr("Requests per second"));
                    ui.add(egui::DragValue::new(&mut editor.tpslimit).range(0.0..=1000.0).speed(0.1));
                });
                theme::hint(ui, tr("Requests per second to this whole account, uploads and downloads each. RPool splits it over the simultaneous shards (rclone --tpslimit per call). 0 = no limit."));
                ui.horizontal(|ui| {
                    ui.label(tr("Simultaneous shard uploads"));
                    ui.add(egui::DragValue::new(&mut editor.max_uploads).range(0..=256));
                });
                theme::hint(ui, tr("How many shards are uploaded to this account at once; each is one upload request. 0 = the default from Settings › Network (Dropbox 1). Lower it for providers that answer \"too many requests\", e.g. Filen."));
                ui.horizontal(|ui| {
                    ui.label(tr("Simultaneous shard downloads"));
                    ui.add(egui::DragValue::new(&mut editor.max_downloads).range(0..=256));
                });
                theme::hint(ui, tr("How many shards are read from this account at once. 0 = the default from Settings › Network."));
                ui.separator();
                ui.strong(tr("Inactivity warning"));
                let default_warn = match default_inactivity_days(&editor.kind) {
                    Some(days) => trf("Backend default ({days} days)", &[("days", &days)]),
                    None => tr("Backend default (none)").to_string(),
                };
                mode_row(ui, &mut editor.warn, &default_warn, tr("Never warn"));
                if editor.warn == Mode::Custom {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut editor.warn_days).range(1..=3650));
                        ui.label(tr("days without activity"));
                    });
                }
                theme::hint(ui, tr("Providers decide what counts as activity; RPool cannot guarantee that its API access keeps an account."));
                ui.separator();
                let check = editor.edit();
                if let Err(error) = &check {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if theme::primary_button(ui, check.is_ok(), tr("Save limits")).clicked() {
                    editor.notice = Some(save(editor));
                    saved = editor.notice.as_deref() == Some(tr("Limits saved."));
                }
                if let Some(notice) = &editor.notice {
                    ui.label(notice);
                }
            });
        });
    editor.open = open && !saved;
    saved
}

/// Applies the editor to the current limits file and saves it; returns the message to show.
fn save(editor: &LimitsEditor) -> String {
    let result = crate::storage::account::store::load_limits()
        .map_err(|e| format!("{e:#}"))
        .and_then(|store| editor.apply_to(&store))
        .and_then(|next| {
            crate::storage::account::store::save_limits(&next).map_err(|e| format!("{e:#}"))
        });
    match result {
        Ok(_) => tr("Limits saved.").into(),
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::account::limits::AccountLimits;

    #[test]
    fn editor_round_trips_overrides_and_validates() {
        let mut store = LimitsStore::default();
        let fresh = LimitsEditor::open_for("gd", "drive", &store);
        assert_eq!((fresh.daily, fresh.warn), (Mode::Default, Mode::Default));
        assert_eq!(fresh.daily_gib, 698.5);
        assert_eq!(fresh.warn_days, 548);
        // Defaults only: saving leaves no entry behind.
        assert!(fresh.apply_to(&store).unwrap().accounts.is_empty());

        store.accounts.insert(
            "gd".into(),
            AccountLimits {
                daily_upload: Some(DailyUpload::Bytes(10 << 30)),
                bwlimit: Some("1M".into()),
                tpslimit: Some(2.0),
                max_uploads: None,
                max_downloads: None,
                inactivity_warn_days: Some(0),
            },
        );
        let mut editor = LimitsEditor::open_for("gd", "drive", &store);
        assert_eq!((editor.daily, editor.daily_gib), (Mode::Custom, 10.0));
        assert_eq!(
            (editor.warn, editor.bwlimit.as_str(), editor.tpslimit),
            (Mode::Off, "1M", 2.0)
        );
        assert_eq!(editor.apply_to(&store).unwrap(), store);

        editor.daily = Mode::Off;
        editor.bwlimit = String::new();
        editor.tpslimit = 0.0;
        editor.warn = Mode::Custom;
        editor.warn_days = 90;
        let next = editor.apply_to(&store).unwrap();
        let gd = &next.accounts["gd"];
        assert_eq!(gd.daily_upload, Some(DailyUpload::Unlimited));
        assert_eq!(
            (gd.bwlimit.clone(), gd.tpslimit, gd.inactivity_warn_days),
            (None, None, Some(90))
        );

        editor.bwlimit = "fast".into();
        assert!(editor.edit().is_err());
        editor.bwlimit = "off".into();
        editor.daily = Mode::Custom;
        editor.daily_gib = 0.0;
        assert!(editor.edit().is_err());
    }
}
