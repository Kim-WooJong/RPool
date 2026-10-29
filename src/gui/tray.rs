//! Tray icon (Windows notification area, macOS menu bar). Closing the window
//! while drives are mounted or a task runs hides it there, and the mounts keep
//! running in this process; the tray menu reopens the window or unmounts and
//! quits. Linux has no tray (tray-icon needs GTK there, and Wayland cannot hide
//! windows), so closing the window unmounts and quits as before.
use eframe::egui;

/// A click in the tray, handled by `RpoolGui::logic` on the UI thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Linux has no tray, so nothing sends these there.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub(crate) enum TrayCommand {
    /// Show and focus the window.
    Show,
    /// Unmount every drive and quit.
    Quit,
}

/// What a window close request does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseAction {
    /// Cancel the close and hide the window; work keeps running.
    Hide,
    /// Unmount every drive (graceful stop) and exit, as without a tray.
    Quit,
}

/// Closing hides the window only when a tray icon exists (so the window can be
/// brought back), the setting allows it, something still runs, and the tray's
/// own Quit did not start the close.
pub(crate) fn close_action(
    tray_available: bool,
    close_to_tray: bool,
    busy: bool,
    quitting: bool,
) -> CloseAction {
    if tray_available && close_to_tray && busy && !quitting {
        CloseAction::Hide
    } else {
        CloseAction::Quit
    }
}

/// Whether this build has a tray icon at all (shows the setting).
pub(crate) const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

/// The tray icon with its menu (Open RPool, status, Unmount drives and quit).
#[cfg(any(windows, target_os = "macos"))]
pub(crate) struct Tray {
    /// Dropping the icon removes it from the tray.
    icon: tray_icon::TrayIcon,
    /// Disabled menu line with the current status.
    status: tray_icon::menu::MenuItem,
    /// Commands from the menu and icon clicks (sent from event handlers).
    commands: std::sync::mpsc::Receiver<TrayCommand>,
    /// Status text last given to the OS; it is only touched on change.
    shown: String,
}

#[cfg(any(windows, target_os = "macos"))]
impl Tray {
    /// Creates the icon. Call on the UI thread while the event loop runs (the
    /// eframe app creator). `None` when the OS refused: closing then quits.
    pub(crate) fn new(ctx: &egui::Context) -> Option<Self> {
        use crate::gui::i18n::tr;
        use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
        use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
        let open = MenuItem::new(tr("Open RPool"), true, None);
        let status = MenuItem::new("", false, None);
        let quit = MenuItem::new(tr("Unmount drives and quit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &status, &PredefinedMenuItem::separator(), &quit])
            .ok()?;
        let icon = tray_icon::Icon::from_rgba(icon_rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE).ok()?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("RPool")
            .with_menu(Box::new(menu))
            // Windows: left click opens the window, right click the menu.
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .build()
            .ok()?;
        let (sender, commands) = std::sync::mpsc::channel();
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let (menu_sender, menu_ctx) = (sender.clone(), ctx.clone());
        // Handlers can be set once per process; this runs once per GUI.
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = if event.id == open_id {
                TrayCommand::Show
            } else if event.id == quit_id {
                TrayCommand::Quit
            } else {
                return;
            };
            let _ = menu_sender.send(command);
            // Wakes eframe, also while the window is hidden.
            menu_ctx.request_repaint();
        }));
        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            // Move/Enter/Leave are ignored: they would wake the GUI on every mouse move.
            let left_click = matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            );
            if left_click && !cfg!(target_os = "macos") {
                let _ = sender.send(TrayCommand::Show);
                click_ctx.request_repaint();
            }
        }));
        Some(Self {
            icon,
            status,
            commands,
            shown: String::new(),
        })
    }

    /// The next pending tray command.
    pub(crate) fn try_recv(&self) -> Option<TrayCommand> {
        self.commands.try_recv().ok()
    }

    /// Shows `text` in the menu and tooltip (OS calls only when it changed).
    pub(crate) fn set_status(&mut self, text: &str) {
        if self.shown != text {
            self.status.set_text(text);
            let _ = self.icon.set_tooltip(Some(format!("RPool — {text}")));
            self.shown = text.to_string();
        }
    }
}

/// Placeholder: this platform has no tray icon.
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) struct Tray;

#[cfg(not(any(windows, target_os = "macos")))]
impl Tray {
    /// No tray on this platform.
    pub(crate) fn new(_ctx: &egui::Context) -> Option<Self> {
        None
    }

    /// Never called: no tray exists.
    pub(crate) fn try_recv(&self) -> Option<TrayCommand> {
        None
    }

    /// Never called: no tray exists.
    pub(crate) fn set_status(&mut self, _text: &str) {}
}

/// Edge length of the tray icon, pixels (scaled by the OS).
#[cfg(any(windows, target_os = "macos"))]
const ICON_SIZE: u32 = 32;

/// The tray icon: an anti-aliased blue disc with three white "shard" bars
/// (the repository has no icon asset).
#[cfg(any(windows, target_os = "macos"))]
fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let (center, radius) = ((s - 1.0) / 2.0, s / 2.0 - 0.5);
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for i in 0..size * size {
        let (x, y) = ((i % size) as f32, (i / size) as f32);
        let distance = ((x - center).powi(2) + (y - center).powi(2)).sqrt();
        let alpha = (radius - distance + 0.5).clamp(0.0, 1.0);
        let (u, v) = (x / s, y / s);
        let bar = (0.22..0.78).contains(&u)
            && [0.28_f32, 0.46, 0.64]
                .iter()
                .any(|top| (*top..top + 0.1).contains(&v));
        let [r, g, b] = if bar {
            [255, 255, 255]
        } else {
            [0x2f, 0x80, 0xed]
        };
        rgba.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_only_with_tray_setting_and_running_work() {
        assert_eq!(close_action(true, true, true, false), CloseAction::Hide);
        // Nothing runs: closing quits right away.
        assert_eq!(close_action(true, true, false, false), CloseAction::Quit);
        // No tray (Linux, or the OS refused): never hide a window that cannot come back.
        assert_eq!(close_action(false, true, true, false), CloseAction::Quit);
        // The user turned closing to the tray off.
        assert_eq!(close_action(true, false, true, false), CloseAction::Quit);
        // The tray's own Quit closes for real.
        assert_eq!(close_action(true, true, true, true), CloseAction::Quit);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn icon_is_square_rgba_with_transparent_corners() {
        let rgba = icon_rgba(ICON_SIZE);
        assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        assert_eq!(rgba[3], 0, "corner pixel is transparent");
        let middle = ((ICON_SIZE / 2) * ICON_SIZE + ICON_SIZE / 2) as usize * 4;
        assert_eq!(rgba[middle + 3], 255, "center is opaque");
        assert!(tray_icon::Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).is_ok());
    }
}
