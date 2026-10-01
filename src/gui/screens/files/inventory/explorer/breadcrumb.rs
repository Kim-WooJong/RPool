//! The path bar: `Pool › folder › sub`, each segment opens its folder.

use super::action::Action;
use eframe::egui;

/// `(label, path)` per segment after the root, e.g. `a/b` gives
/// `[("a", "a"), ("b", "a/b")]`.
pub(crate) fn segments(path: &str) -> Vec<(&str, &str)> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut end = 0;
    for part in path.split('/') {
        end += part.len();
        out.push((part, &path[..end]));
        end += 1;
    }
    out
}

/// Draws the segments into the (wrapping) row; the root segment is the
/// pool name. The current folder is shown strong and is not a link.
pub(crate) fn show(ui: &mut egui::Ui, pool: &str, current: &str, action: &mut Option<Action>) {
    {
        let segments = segments(current);
        let root = pool.to_string();
        if segments.is_empty() {
            ui.strong(root);
        } else if ui.link(root).clicked() {
            *action = Some(Action::Goto(String::new()));
        }
        let last = segments.len().saturating_sub(1);
        for (index, (label, path)) in segments.into_iter().enumerate() {
            ui.weak("›");
            if index == last {
                ui.strong(label);
            } else if ui.link(label).clicked() {
                *action = Some(Action::Goto(path.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::segments;

    #[test]
    fn segments_build_cumulative_paths() {
        assert!(segments("").is_empty());
        assert_eq!(segments("a"), vec![("a", "a")]);
        assert_eq!(
            segments("Photos/2024/trip"),
            vec![
                ("Photos", "Photos"),
                ("2024", "Photos/2024"),
                ("trip", "Photos/2024/trip")
            ]
        );
    }
}
