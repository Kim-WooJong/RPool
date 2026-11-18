//! Resolve name-only aliases retained by location renames, without weakening crypt gates.
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeSet;

/// Only zero-prefix aliases qualify. Folder aliases would alter native object keys.
pub(crate) fn crypt_entry<'a>(dump: &'a Value, name: &str) -> Result<&'a Value> {
    let mut name = name;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(name) { bail!("crypt compatibility alias cycle"); }
        let entry = dump.get(name).ok_or_else(|| anyhow::anyhow!("crypt remote is not configured"))?;
        match entry.get("type").and_then(Value::as_str) {
            Some("crypt") => return Ok(entry),
            Some("alias") => {
                let target = entry.get("remote").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("alias target missing"))?;
                let Some((next, "")) = target.split_once(':') else { bail!("only name-only crypt compatibility aliases are supported"); };
                if next.is_empty() { bail!("invalid crypt compatibility alias"); }
                name = next;
            }
            _ => bail!("refusing non-crypt destination"),
        }
    }
}
