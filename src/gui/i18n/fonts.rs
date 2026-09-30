//! CJK glyphs. egui's bundled fonts cover Latin only, so Korean, Japanese
//! and Chinese text (translations, but also file and pool names) would show as
//! boxes. The first installed system font per script is added as a fallback
//! after the default fonts, so Latin text is unchanged.
use eframe::egui;

/// Candidates per script, most common first: Windows, macOS, Linux.
const CANDIDATES: [(&str, &[&str]); 3] = [
    (
        "cjk-kr",
        &[
            "C:\\Windows\\Fonts\\malgun.ttf",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        ],
    ),
    (
        "cjk-jp",
        &[
            "C:\\Windows\\Fonts\\YuGothM.ttc",
            "C:\\Windows\\Fonts\\meiryo.ttc",
            "C:\\Windows\\Fonts\\msgothic.ttc",
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ],
    ),
    (
        "cjk-sc",
        &[
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ],
    ),
];

/// Adds the fonts that exist on this PC. Returns the ones added.
pub(crate) fn install(ctx: &egui::Context) -> Vec<String> {
    let mut added = Vec::new();
    let mut loaded_paths = Vec::new();
    for (name, paths) in CANDIDATES {
        let Some(path) = paths.iter().find(|p| std::path::Path::new(p).is_file()) else {
            continue;
        };
        if loaded_paths.contains(path) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        loaded_paths.push(*path);
        let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
            .into_iter()
            .map(|family| egui::epaint::text::InsertFontFamily {
                family,
                priority: egui::epaint::text::FontPriority::Lowest,
            })
            .collect();
        ctx.add_font(egui::epaint::text::FontInsert::new(
            name,
            egui::FontData::from_owned(bytes),
            families,
        ));
        added.push(format!("{name}: {path}"));
    }
    added
}
