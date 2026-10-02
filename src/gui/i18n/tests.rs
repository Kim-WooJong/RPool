use super::*;

#[test]
fn english_is_the_default_and_passes_through() {
    assert_eq!(Language::default(), Language::English);
    assert_eq!(tr_in(Language::English, "Mount"), "Mount");
    assert_eq!(
        tr_in(Language::Korean, "no such key 123"),
        "no such key 123"
    );
}

#[test]
fn tables_parse_and_every_entry_has_all_three_languages() {
    let mut incomplete = Vec::new();
    for (name, texts) in TABLES {
        let parsed: Vec<HashMap<String, String>> = texts
            .iter()
            .map(|text| serde_json::from_str(text).unwrap())
            .collect();
        let mut keys: Vec<&String> = parsed.iter().flat_map(|t| t.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            for (index, table) in parsed.iter().enumerate() {
                if !table.contains_key(key) {
                    incomplete.push(format!("{}/{name}: {key}", LANGUAGES[index]));
                }
            }
        }
    }
    assert!(
        incomplete.is_empty(),
        "incomplete translations: {incomplete:#?}"
    );
}

/// Every literal passed to `tr(`/`trf(` in the GUI sources has a
/// translation. Keeps the tables complete as screens change.
#[test]
fn every_wrapped_gui_string_is_translated() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui");
    let mut missing = std::collections::BTreeSet::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            // The i18n module itself only has examples in its docs.
            if path.extension().is_none_or(|e| e != "rs")
                || path.components().any(|c| c.as_os_str() == "i18n")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for key in wrapped_literals(&text) {
                if !table().contains_key(&key) {
                    missing.insert(format!("{}: {key}", path.display()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "untranslated GUI strings: {missing:#?}");
}

/// The string literals right after `tr(` or `trf(` (plain `"…"` literals with
/// the usual escapes; raw strings are not used for UI text).
fn wrapped_literals(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["tr(", "trf("] {
        let mut rest = source;
        while let Some(start) = rest.find(marker) {
            let before = &rest[..start];
            rest = &rest[start + marker.len()..];
            // Skip identifiers that merely end in "tr(" (e.g. `attr(`).
            if before
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            // rustfmt may put the literal on the next line.
            let Some(after_quote) = rest.trim_start().strip_prefix('"') else {
                continue;
            };
            rest = after_quote;
            let mut literal = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => literal.push('\n'),
                        Some('t') => literal.push('\t'),
                        Some(other) => literal.push(other),
                        None => break,
                    },
                    c => literal.push(c),
                }
            }
            out.push(literal);
        }
    }
    out
}

#[test]
fn trf_fills_named_values() {
    set_language(Language::English);
    assert_eq!(
        trf("{n} of {total} files", &[("n", &3), ("total", &5)]),
        "3 of 5 files"
    );
}
