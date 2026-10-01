//! `rpool doctor --bundle FILE.zip`: a redacted diagnostics archive to attach
//! to a bug report. Collection, redaction, the manifest and the ZIP writer
//! each live in their own file.
mod collect;
mod manifest;
mod rclone_info;
pub(crate) mod redact;
#[cfg(test)]
mod tests;
pub(crate) mod zip;

use crate::doctor::Diagnostic;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(crate) use rclone_info::{RcloneCommand, RcloneInfo};

/// One file of the bundle, already redacted.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) note: String,
}

impl Entry {
    pub(crate) fn new(name: &str, bytes: Vec<u8>, note: &str) -> Self {
        Self {
            name: name.into(),
            bytes,
            note: note.into(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub(crate) entries: Vec<Entry>,
    /// `name: reason` of files looked for but not included.
    pub(crate) skipped: Vec<String>,
}

pub(crate) struct Sources<'a> {
    pub(crate) config_dir: PathBuf,
    /// `None` with `--local-only`.
    pub(crate) rclone: Option<&'a dyn RcloneInfo>,
    pub(crate) doctor: Vec<Diagnostic>,
    pub(crate) now_unix: u64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Summary {
    pub(crate) path: PathBuf,
    pub(crate) files: Vec<String>,
    pub(crate) skipped: Vec<String>,
    pub(crate) bytes: u64,
}

/// Runs the doctor and writes the bundle for the current configuration.
pub(crate) fn export(rclone: &str, output: &Path, local_only: bool) -> Result<Summary> {
    let command = RcloneCommand::new(rclone);
    let sources = Sources {
        config_dir: crate::config::app_config_dir()?,
        rclone: (!local_only).then_some(&command as &dyn RcloneInfo),
        doctor: if local_only {
            crate::doctor::run_checks_with(None, None)
        } else {
            crate::doctor::run_checks(rclone)
        },
        now_unix: crate::utils::now_unix(),
    };
    write_bundle(&sources, output)
}

/// Collects, redacts and writes `output` atomically (temp file + rename).
pub(crate) fn write_bundle(sources: &Sources, output: &Path) -> Result<Summary> {
    let collected = collect::collect(sources);
    let manifest = manifest::render(&collected, sources.now_unix);
    let parent = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let temp = tempfile::Builder::new()
        .prefix(".rpool-diagnostics-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("cannot write in {}", parent.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(temp), sources.now_unix);
    zip.add(manifest::NAME, manifest.as_bytes())?;
    for entry in &collected.entries {
        zip.add(&entry.name, &entry.bytes)?;
    }
    let temp = zip
        .finish()?
        .into_inner()
        .map_err(|error| error.into_error())?;
    temp.as_file().sync_all()?;
    let file = temp
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("cannot save {}", output.display()))?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut files = vec![manifest::NAME.to_string()];
    files.extend(collected.entries.iter().map(|entry| entry.name.clone()));
    Ok(Summary {
        path: output.to_path_buf(),
        files,
        skipped: collected.skipped,
        bytes,
    })
}
