//! GUI languages. English is the source text and the default; Korean,
//! Japanese and Chinese (Simplified) come from `locales/<language>/<area>.json` at the crate root (registered in
//! `TABLES` below),
//! keyed by the exact English text:
//!
//! ```json
//! { "Mount": "마운트" }
//! ```
//!
//! Wrap every user-facing literal in [`tr`], or [`trf`] when it has values
//! (`trf("{n} files", &[("n", &count)])`). A missing translation falls back to
//! English. The CLI stays in English.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Language {
    #[default]
    English,
    Korean,
    Japanese,
    Chinese,
}

impl Language {
    pub(crate) const ALL: [Language; 4] = [
        Language::English,
        Language::Korean,
        Language::Japanese,
        Language::Chinese,
    ];
    /// The language's own name, for the selector.
    pub(crate) fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Korean => "한국어",
            Language::Japanese => "日本語",
            Language::Chinese => "中文（简体）",
        }
    }
    fn index(self) -> Option<usize> {
        match self {
            Language::English => None,
            Language::Korean => Some(0),
            Language::Japanese => Some(1),
            Language::Chinese => Some(2),
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub(crate) fn set_language(language: Language) {
    let value = Language::ALL
        .iter()
        .position(|l| *l == language)
        .unwrap_or(0);
    CURRENT.store(value as u8, Ordering::Relaxed);
}

pub(crate) fn language() -> Language {
    Language::ALL
        .get(CURRENT.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
}

/// Every table: one file per GUI area and language, under
/// `locales/<ko|ja|zh>/<area>.json`, each a flat map from the exact English
/// text to its translation. Each area has its own files, so translators of
/// different screens or languages never edit the same file.
macro_rules! table {
    ($name:literal) => {
        (
            $name,
            [
                include_str!(concat!("../../../locales/ko/", $name, ".json")),
                include_str!(concat!("../../../locales/ja/", $name, ".json")),
                include_str!(concat!("../../../locales/zh/", $name, ".json")),
            ],
        )
    };
}
const TABLES: [(&str, [&str; 3]); 11] = [
    table!("core"),
    table!("drive"),
    table!("drive_history"),
    table!("files_health"),
    table!("limits"),
    table!("metadata"),
    table!("migration_drive"),
    table!("migration_retire"),
    table!("monitoring"),
    table!("storage"),
    table!("tooling"),
];

/// Language folders, in `Language::index` order.
const LANGUAGES: [&str; 3] = ["ko", "ja", "zh"];

type Table = HashMap<String, [Option<&'static str>; 3]>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table::new();
        for (name, texts) in TABLES {
            for (index, text) in texts.into_iter().enumerate() {
                let parsed: HashMap<String, String> =
                    serde_json::from_str(text).unwrap_or_else(|e| {
                        panic!(
                            "i18n table {}/{name}.json is invalid: {e}",
                            LANGUAGES[index]
                        )
                    });
                for (english, translated) in parsed {
                    // Leaked once per process: the tables are small and live forever.
                    let leaked: &'static str = Box::leak(translated.into_boxed_str());
                    table.entry(english).or_insert([None; 3])[index] = Some(leaked);
                }
            }
        }
        table
    })
}

/// `english` in the current language (English when untranslated).
pub(crate) fn tr(english: &'static str) -> &'static str {
    tr_in(language(), english)
}

pub(crate) fn tr_in(language: Language, english: &'static str) -> &'static str {
    match language.index() {
        None => english,
        Some(i) => table().get(english).and_then(|t| t[i]).unwrap_or(english),
    }
}

/// Translates `template`, then replaces each `{name}` with its value.
pub(crate) fn trf(template: &'static str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = tr(template).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// A duration phrase from `migration::speed::format_estimate` ("about
/// 12–20 min", "under 1 min", "unknown") in the current language.
pub(crate) fn duration_text(english: &str) -> String {
    match english {
        "unknown" => return tr("unknown").into(),
        "under 1 min" => return tr("under 1 min").into(),
        _ => {}
    }
    let body = english.strip_prefix("about ").unwrap_or(english);
    let words: Vec<&str> = body
        .split(' ')
        .map(|word| match word {
            "min" => tr("min"),
            "h" => tr("h"),
            "d" => tr("d"),
            other => other,
        })
        .collect();
    trf("about {duration}", &[("duration", &words.join(" "))])
}

/// "now", "5m ago", "3h ago", "2d ago" in the current language.
pub(crate) fn relative_age(timestamp: u64) -> String {
    relative_age_at(crate::utils::now_unix(), timestamp)
}

/// [`relative_age`] measured from `now` (for testable view models).
pub(crate) fn relative_age_at(now: u64, timestamp: u64) -> String {
    let elapsed = now.saturating_sub(timestamp);
    match elapsed {
        0..=59 => tr("now").into(),
        60..=3_599 => trf("{n}m ago", &[("n", &(elapsed / 60))]),
        3_600..=86_399 => trf("{n}h ago", &[("n", &(elapsed / 3_600))]),
        _ => trf("{n}d ago", &[("n", &(elapsed / 86_400))]),
    }
}

pub(crate) mod fonts;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
