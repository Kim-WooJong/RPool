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

/// Script order for the fallback chain: the selected language's font first,
/// so shared Han characters use that language's glyph forms.
fn order(language: super::Language) -> [&'static str; 3] {
    use super::Language;
    match language {
        Language::Japanese => ["cjk-jp", "cjk-sc", "cjk-kr"],
        Language::Chinese => ["cjk-sc", "cjk-jp", "cjk-kr"],
        Language::Korean | Language::English => ["cjk-kr", "cjk-jp", "cjk-sc"],
    }
}

/// Font files found on this PC, read once.
fn available() -> &'static Vec<(&'static str, &'static str, std::sync::Arc<egui::FontData>)> {
    static FOUND: std::sync::OnceLock<
        Vec<(&'static str, &'static str, std::sync::Arc<egui::FontData>)>,
    > = std::sync::OnceLock::new();
    FOUND.get_or_init(|| {
        let mut found = Vec::new();
        for (name, paths) in CANDIDATES {
            let Some(path) = paths.iter().find(|p| std::path::Path::new(p).is_file()) else {
                continue;
            };
            if let Ok(bytes) = std::fs::read(path) {
                found.push((
                    name,
                    *path,
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                ));
            }
        }
        found
    })
}

/// Rebuilds the font set: egui's defaults, then the CJK fonts found on this
/// PC as fallbacks, in `language`'s order. Call at start and whenever the
/// language changes. Returns the fonts used.
pub(crate) fn install(ctx: &egui::Context, language: super::Language) -> Vec<String> {
    let mut definitions = egui::FontDefinitions::default();
    let mut used = Vec::new();
    for script in order(language) {
        let Some((name, path, data)) = available().iter().find(|(n, ..)| *n == script) else {
            continue;
        };
        definitions
            .font_data
            .insert((*name).to_string(), data.clone());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .push((*name).to_string());
        }
        used.push(format!("{name}: {path}"));
    }
    ctx.set_fonts(definitions);
    used
}
