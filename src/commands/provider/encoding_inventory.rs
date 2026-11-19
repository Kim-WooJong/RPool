//! Bounded, read-only namespace observations, not archive recovery or integrity verification.
use crate::config_sync::encoding_probe::Probe;
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
use std::path::Path;

/// Explicit bounds for the read-only encoding inventory. No source contents are read.
#[derive(clap::Args, Debug)]
pub(crate) struct EncodingInventoryArgs {
    /// Exact direct crypt name, without colon or path (not a compatibility alias).
    #[arg(long)]
    pub(crate) remote: String,
    /// Only filename encoding varies between views; existing config is not changed.
    #[arg(long, value_delimiter = ',', default_value = "base32,base32768")]
    pub(crate) encodings: Vec<String>,
    /// Metadata directory depth below the crypt root; archive payloads are never descended.
    #[arg(long, default_value_t = 2)]
    pub(crate) max_depth: usize,
    /// Total processed entries across views, including out-of-scope top entries.
    #[arg(long, default_value_t = 10000)]
    pub(crate) max_entries: usize,
    /// Total directory requests across views. Each child has a 120-second deadline.
    #[arg(long, default_value_t = 64)]
    pub(crate) max_listings: usize,
    /// Bound for each raw listing response (names never copied into errors).
    #[arg(long, default_value_t = 1048576)]
    pub(crate) max_listing_bytes: usize,
    /// Maximum JSON report bytes; excess details are omitted and incomplete is true.
    #[arg(long, default_value_t = 1048576)]
    pub(crate) max_output_bytes: usize,
    /// Create a new JSON report file; refuses overwrite and creates no parent folders.
    #[arg(long)]
    pub(crate) output: Option<std::path::PathBuf>,
    /// Emit the bounded JSON report on stdout, including incomplete observations.
    #[arg(long)]
    pub(crate) json: bool,
}

/// One successfully listed or rejected metadata directory; paths are pseudonymous.
#[derive(Serialize)]
struct Directory {
    path_id: String,
    complete: bool,
    reason: Option<&'static str>,
    entries_observed: Option<usize>,
    items: Vec<Item>,
}
/// An observed metadata object, never its user-controlled cleartext name.
#[derive(Serialize)]
struct Item {
    path_id: String,
    is_dir: bool,
    size: Option<u64>,
}
/// Provenance stays per encoding; counts are unknown on failed listings, never zero.
#[derive(Serialize)]
struct View {
    encoding: String,
    top_complete: bool,
    top_entries_observed: Option<usize>,
    virtual_archive_directory_count: Option<usize>,
    virtual_archive_ids: Vec<String>,
    sync_present: Option<bool>,
    metadata_complete: bool,
    metadata: Vec<Directory>,
    reasons: Vec<&'static str>,
}
/// Same logical top name is not proof of same content, completeness or identity.
#[derive(Serialize)]
struct Overlap {
    logical_id: String,
    encodings: Vec<String>,
}
/// Versioned report has no keys, root paths, personal filenames or child stderr.
#[derive(Serialize)]
struct Report {
    format: &'static str,
    version: u32,
    scope: &'static str,
    snapshot_same_key_root: bool,
    snapshot_reason: Option<String>,
    current_encoding: Option<String>,
    max_depth: usize,
    max_entries: usize,
    max_listings: usize,
    incomplete: bool,
    reasons: Vec<&'static str>,
    archive_integrity: &'static str,
    deep_metadata_integrity: &'static str,
    views: Vec<View>,
    top_overlaps_complete: bool,
    top_overlaps: Vec<Overlap>,
}

/// Fingerprints allow correlating metadata paths without disclosing their names.
fn path_id(path: &str) -> String {
    format!("path-blake3:{}", blake3::hash(path.as_bytes()).to_hex())
}
/// Only application-generated UUID/hex archive ids are emitted verbatim.
fn archive_id(name: &str) -> String {
    let suffix = name.strip_prefix("virtual-pack-").or_else(|| name.strip_prefix("virtual-"));
    if suffix.is_some_and(|s| !s.is_empty() && s.len() <= 80 && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')) {
        name.into()
    } else { path_id(name) }
}

/// Inspect the selected crypt, preserving each view's failure and boundedness.
fn collect(executable: &str, args: &EncodingInventoryArgs) -> Report {
    let mut report = Report {
        format: "rpool-encoding-inventory", version: 1,
        scope: "RPool top-level virtual-* ids and depth-bounded .rpool-sync metadata names only",
        snapshot_same_key_root: false, snapshot_reason: None, current_encoding: None,
        max_depth: args.max_depth, max_entries: args.max_entries, max_listings: args.max_listings,
        incomplete: false, reasons: Vec::new(),
        archive_integrity: "not_checked: directories are not completed archives; manifests/shards/content not read",
        deep_metadata_integrity: "not_checked: metadata names only; contents, references and migration state not verified",
        views: Vec::new(), top_overlaps_complete: false, top_overlaps: Vec::new(),
    };
    let probe = Probe::new(Path::new(executable), &args.remote);
    if let Err(error) = &probe {
        // Probe errors are deliberately fixed diagnostics without values or child stderr.
        report.snapshot_reason = Some(error.to_string());
    }
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (view_index, encoding) in args.encodings.iter().enumerate() {
        // Reserve a fair share for every view: a large first namespace must not
        // prevent the other encoding's top-level observation.
        let mut remaining = args.max_entries / args.encodings.len() + usize::from(view_index < args.max_entries % args.encodings.len());
        let mut listings = args.max_listings / args.encodings.len() + usize::from(view_index < args.max_listings % args.encodings.len());
        let mut view = View { encoding: encoding.clone(), top_complete: false,
            top_entries_observed: None, virtual_archive_directory_count: None, virtual_archive_ids: Vec::new(), sync_present: None,
            metadata_complete: true, metadata: Vec::new(), reasons: Vec::new() };
        let Ok(probe) = &probe else {
            view.metadata_complete = false;
            view.reasons.push("snapshot_rejected_or_unavailable: only direct standard crypt SFTP/SMB/local is supported");
            report.views.push(view); continue;
        };
        report.snapshot_same_key_root = true;
        report.current_encoding = Some(probe.current_encoding.clone());
        let mut queue = VecDeque::from([(String::new(), 0usize)]);
        while let Some((path, depth)) = queue.pop_front() {
            if listings == 0 || remaining == 0 {
                view.metadata_complete = false;
                view.reasons.push("entry_or_listing_limit: unvisited_directories_remain");
                for (pending, _) in std::iter::once((path, depth)).chain(queue.into_iter()) {
                    if !pending.is_empty() {
                        view.metadata.push(Directory { path_id: path_id(&pending), complete: false,
                            reason: Some("entry_or_listing_limit: not_requested"), entries_observed: None, items: Vec::new() });
                    }
                }
                break;
            }
            listings -= 1;
            let found = match probe.list(encoding, &path, args.max_listing_bytes) {
                Ok(found) => found,
                Err(_) => {
                    view.metadata_complete = false;
                    view.reasons.push("listing_failed_or_output_limit_or_timeout");
                    if !path.is_empty() {
                        view.metadata.push(Directory { path_id: path_id(&path), complete: false,
                            reason: Some("listing_failed_or_output_limit_or_timeout"), entries_observed: None, items: Vec::new() });
                    }
                    continue;
                }
            };
            let count = found.len();
            let complete = count <= remaining;
            let take = count.min(remaining);
            remaining -= take;
            if !complete {
                view.metadata_complete = false;
                view.reasons.push("entry_limit: listing_not_fully_processed");
            }
            if path.is_empty() {
                view.top_complete = complete;
                view.top_entries_observed = Some(count);
                view.sync_present = if complete { Some(false) } else { None };
                let mut archive_dirs = 0usize;
                let mut seen = BTreeSet::new();
                for entry in found.into_iter().take(take) {
                    if !seen.insert(entry.path.clone()) {
                        view.top_complete = false;
                        view.metadata_complete = false;
                        view.reasons.push("duplicate_top_listing_name");
                        continue;
                    }
                    if entry.path.is_empty() || entry.path.contains('/') || entry.path.contains('\\')
                        || entry.path == "." || entry.path == ".." || entry.path.chars().any(char::is_control) {
                        view.top_complete = false;
                        view.metadata_complete = false;
                        view.reasons.push("rejected_top_listing_name");
                        if view.sync_present == Some(false) { view.sync_present = None; }
                        continue;
                    }
                    if entry.path == ".rpool-sync" {
                        names.entry(".rpool-sync".into()).or_default().insert(encoding.clone());
                        if !entry.is_dir {
                            view.metadata_complete = false;
                            view.reasons.push("sync_path_is_not_directory");
                        } else {
                            view.sync_present = Some(true);
                            queue.push_back((entry.path, 1));
                        }
                    } else if entry.path.starts_with("virtual-") {
                        if entry.is_dir { archive_dirs += 1; }
                        if !entry.is_dir {
                            view.metadata_complete = false;
                            view.reasons.push("virtual_archive_path_is_not_directory");
                        }
                        let id = archive_id(&entry.path);
                        names.entry(id.clone()).or_default().insert(encoding.clone());
                        view.virtual_archive_ids.push(id);
                    }
                }
                view.virtual_archive_ids.sort();
                if view.top_complete { view.virtual_archive_directory_count = Some(archive_dirs); }
            } else {
                let mut directory = Directory { path_id: path_id(&path), complete,
                    reason: if complete { None } else { Some("entry_limit") },
                    entries_observed: Some(count), items: Vec::new() };
                let mut seen = BTreeSet::new();
                for entry in found.into_iter().take(take) {
                    if !seen.insert(entry.path.clone()) {
                        directory.complete = false;
                        directory.reason = Some("duplicate_metadata_listing_name");
                        view.metadata_complete = false;
                        continue;
                    }
                    // rclone nonrecursive lsjson must return immediate names, never traversal.
                    if entry.path.is_empty() || entry.path.contains('/') || entry.path.contains('\\')
                        || entry.path == "." || entry.path == ".." || entry.path.chars().any(char::is_control) {
                        directory.complete = false;
                        directory.reason = Some("rejected_listing_name");
                        view.metadata_complete = false;
                        continue;
                    }
                    let child = format!("{path}/{}", entry.path);
                    directory.items.push(Item { path_id: path_id(&child), is_dir: entry.is_dir,
                        size: u64::try_from(entry.size).ok().filter(|_| !entry.is_dir) });
                    if entry.is_dir {
                        if depth < args.max_depth {
                            queue.push_back((child, depth + 1));
                        } else {
                            directory.complete = false;
                            directory.reason = Some("depth_limit: descendant_names_not_inventoried");
                            view.metadata_complete = false;
                        }
                    }
                }
                view.metadata.push(directory);
            }
        }
        report.views.push(view);
    }
    for view in &mut report.views {
        // A hostile listing cannot amplify fixed diagnostic strings into an
        // unbounded report envelope once detail arrays have been omitted.
        view.reasons.sort_unstable();
        view.reasons.dedup();
    }
    report.top_overlaps = names.into_iter().filter(|(_, encodings)| encodings.len() > 1)
        .map(|(logical_id, encodings)| Overlap { logical_id, encodings: encodings.into_iter().collect() }).collect();
    report.top_overlaps_complete = report.views.iter().all(|v| v.top_complete);
    report.incomplete = report.views.iter().any(|v| !v.top_complete || !v.metadata_complete);
    report
}

/// Run without history/identity/lease side effects. Only --output creates an explicit report.
pub(crate) fn run(executable: &str, args: EncodingInventoryArgs) -> Result<()> {
    if args.encodings.len() < 2 || args.encodings.len() > 3
        || args.encodings.iter().collect::<BTreeSet<_>>().len() != args.encodings.len() {
        bail!("choose two or three distinct encodings");
    }
    for encoding in &args.encodings { crate::config_sync::provision::validate_filename_encoding(encoding)?; }
    if !(1..=8).contains(&args.max_depth) || !(1..=100_000).contains(&args.max_entries)
        || !(2..=1024).contains(&args.max_listings)
        || !(4096..=8 * 1024 * 1024).contains(&args.max_listing_bytes)
        || !(4096..=8 * 1024 * 1024).contains(&args.max_output_bytes) {
        bail!("inventory limits out of range: depth 1..8, entries 1..100000, listings 2..1024, byte limits 4096..8388608");
    }
    let mut report = collect(executable, &args);
    let mut raw = serde_json::to_vec(&report)?;
    if raw.len() + 1 > args.max_output_bytes {
        report.incomplete = true;
        report.reasons.push("report_output_limit: detail_arrays_omitted");
        report.top_overlaps.clear();
        report.top_overlaps_complete = false;
        for view in &mut report.views {
            view.virtual_archive_ids.clear();
            view.metadata.clear();
            view.reasons.push("report_output_limit: detail_arrays_omitted");
        }
        raw = serde_json::to_vec(&report)?;
    }
    if raw.len() + 1 > args.max_output_bytes { bail!("report envelope exceeds output limit"); }
    raw.push(b'\n');
    if let Some(output) = &args.output {
        // No overwrite, no parent creation, no error paths or names in diagnostics.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(output).map_err(|_| anyhow::anyhow!("cannot create report: output must not already exist"))?;
        file.write_all(&raw).map_err(|_| anyhow::anyhow!("report write failed; explicit output may be partial"))?;
        file.sync_all().map_err(|_| anyhow::anyhow!("report flush failed; explicit output may be partial"))?;
    }
    if args.json {
        // The already bounded serialization is emitted without pretty-print expansion.
        print!("{}", std::str::from_utf8(&raw)?);
    } else {
        println!("Encoding inventory: {} view(s), incomplete={}. Content/integrity NOT checked; overlaps are NOT proof of identical files or completed archives.", report.views.len(), report.incomplete);
        println!("Use --json and/or --output <new-report.json> for per-view provenance, rejections and bounds.");
    }
    if report.incomplete { bail!("encoding inventory incomplete; inspect the bounded report before any recovery decision"); }
    Ok(())
}
