//! GUI languages. English is the source text and the default; Korean,
//! Japanese and Chinese (Simplified) come from the JSON tables in this folder,
//! keyed by the exact English text:
//!
//! ```json
//! { "Mount": { "ko": "마운트", "ja": "マウント", "zh": "挂载" } }
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

/// Every table file. Each part of the GUI owns one, so translators do not
/// edit the same file.
const TABLES: [(&str, &str); 5] = [
    ("core", include_str!("core.json")),
    ("drive", include_str!("drive.json")),
    ("storage", include_str!("storage.json")),
    ("files_health", include_str!("files_health.json")),
    ("monitoring", include_str!("monitoring.json")),
];

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    ko: Option<String>,
    #[serde(default)]
    ja: Option<String>,
    #[serde(default)]
    zh: Option<String>,
}

type Table = HashMap<String, [Option<&'static str>; 3]>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Table::new();
        for (name, text) in TABLES {
            let parsed: HashMap<String, Entry> = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("i18n table {name}.json is invalid: {e}"));
            for (english, entry) in parsed {
                // Leaked once per process: the tables are small and live forever.
                let leak = |s: Option<String>| s.map(|s| &*Box::leak(s.into_boxed_str()));
                table.insert(english, [leak(entry.ko), leak(entry.ja), leak(entry.zh)]);
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
