//! Reads the `--json` document of a history command from its stdout. The
//! document is the last line that opens a JSON array or object and parses
//! from there on (any progress lines before it are skipped).

use serde::de::DeserializeOwned;

/// Deserializes the JSON document from a command's stdout, or `None` when no
/// line parses as `T`. Used by `query::run` on a successful child process.
pub(crate) fn from_stdout<T: DeserializeOwned>(stdout: &str) -> Option<T> {
    let lines: Vec<&str> = stdout.lines().collect();
    from_lines(&lines)
}

/// Tries each line that opens `[`/`{` from the last one backwards, parsing it
/// and everything after it; returns the first value that parses.
fn from_lines<T: DeserializeOwned>(lines: &[&str]) -> Option<T> {
    (0..lines.len()).rev().find_map(|start| {
        let first = lines[start].trim_start();
        if !(first.starts_with('[') || first.starts_with('{')) {
            return None;
        }
        serde_json::Deserializer::from_str(&lines[start..].join("\n"))
            .into_iter::<T>()
            .next()?
            .ok()
    })
}

/// The last non-empty stderr line (the error `rpool` printed), if any.
pub(crate) fn last_error(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.strip_prefix("Error: ").unwrap_or(line).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive_history::model::{Retention, TrashEntry};

    #[test]
    fn finds_the_document_after_other_output() {
        let entries: Vec<TrashEntry> = super::super::sample::trash(1_727_740_800);
        let pretty = serde_json::to_string_pretty(&entries).unwrap();
        let text = format!("reading metadata…\n{pretty}\n");
        assert_eq!(from_stdout::<Vec<TrashEntry>>(&text), Some(entries.clone()));
        let compact = serde_json::to_string(&entries).unwrap();
        assert_eq!(from_stdout::<Vec<TrashEntry>>(&compact), Some(entries));
        assert_eq!(from_stdout::<Vec<TrashEntry>>("[]"), Some(Vec::new()));
        assert_eq!(from_stdout::<Vec<TrashEntry>>("no json"), None);
        let r: Option<Retention> =
            from_stdout(r#"{"trash_days":7,"keep_versions":0,"version_days":30}"#);
        assert_eq!(r.map(|r| r.trash_days), Some(7));
        assert_eq!(
            last_error("x\nError: pool 'a' not found\n\n").as_deref(),
            Some("pool 'a' not found")
        );
        assert_eq!(last_error(" \n"), None);
    }
}
