//! Data model of a migration cleanup (`retire`): the cloud records, the
//! options, and the report shown by the CLI and the GUI.
use crate::prelude::*;

/// Grace period between quarantine and permanent deletion when not given.
pub(crate) const DEFAULT_GRACE_DAYS: u64 = 7;
/// Upper bound for `--grace-days`.
pub(crate) const MAX_GRACE_DAYS: u64 = 365;
/// Mass-delete guard: refuse above this share of a pool's objects or bytes.
pub(crate) const DEFAULT_MAX_DELETE_PERCENT: u32 = 50;
/// Mass-delete guard: refuse above this many objects in one run.
pub(crate) const DEFAULT_MAX_DELETE_OBJECTS: u64 = 10_000;
/// Objects listed per `Deleted` record.
pub(crate) const DELETE_BATCH: usize = 256;
/// Version of [`RetireRecord`].
pub(crate) const RECORD_VERSION: u32 = 1;

/// What a cleanup item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ItemKind {
    /// An original that a switched, re-verified replacement superseded.
    Original,
    /// A partial copy left by an interrupted or failed attempt.
    Orphan,
}

/// One object to delete: its address and plaintext size.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RetireObject {
    /// Full object address (`remote:path/...`).
    pub address: String,
    /// Plaintext size in bytes (from the listing).
    pub size: u64,
    /// The pool root (account) it lives on.
    pub root: String,
}

/// Kind of a cleanup record. Every record is immutable and content-addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RetireKind {
    /// Quarantine: the exact objects that may be deleted after the grace.
    Fossil,
    /// The user took the item out of quarantine (only before deletion).
    Restore,
    /// A fresh check before deletion found a reason to keep the item.
    Cancelled,
    /// Point of no return: deletion of this fossil started.
    Deleting,
    /// Objects deleted (one batch).
    Deleted,
    /// Every object of the fossil is gone.
    Purged,
}

/// One cloud record of the cleanup, under `<migration>/retire/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RetireRecord {
    /// Record format version ([`RECORD_VERSION`]).
    pub version: u32,
    /// What this record says about the quarantine.
    pub kind: RetireKind,
    /// Archive id of the item (the original or the orphan copy).
    pub item: String,
    /// Whether the item is a superseded original or an orphan copy.
    pub item_kind: ItemKind,
    /// Id of the quarantine this record belongs to (the `Fossil` record's
    /// own `fossil_id`); a restored item gets a new one when quarantined again.
    pub fossil_id: String,
    /// PC that wrote the record.
    pub pc_id: String,
    /// Unix time (s) the record was written.
    pub ts_unix: u64,
    /// `Fossil` only: seconds until deletion is allowed.
    #[serde(default)]
    pub grace_seconds: u64,
    /// `Fossil`: every object that may be deleted; `Deleted`: this batch.
    #[serde(default)]
    pub objects: Vec<RetireObject>,
    /// Original items: the replacement archive id.
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    /// Original file name of the item, for reports.
    pub original_name: String,
    #[serde(default)]
    /// Free-text note, e.g. why a fossil was cancelled or restored.
    pub detail: Option<String>,
}

/// Which part of a confirmed run to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RetireStep {
    /// Delete due fossils (and resume interrupted deletions), then
    /// quarantine new candidates.
    #[default]
    All,
    /// Only quarantine new candidates.
    Quarantine,
    /// Only delete due fossils and resume interrupted deletions.
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Settings of one `pool migrate retire` run (CLI flags or GUI).
pub(crate) struct RetireOptions {
    /// Without it nothing is written or deleted (dry run).
    pub confirm: bool,
    /// Which steps a confirmed run performs.
    pub step: RetireStep,
    /// Grace given to new fossils.
    pub grace_seconds: u64,
    /// Also delete the originals' objects on accounts that left the pool
    /// (when they are still configured and listable).
    pub include_removed: bool,
    /// Re-verify replacements by reading every shard back (default: list
    /// sizes and validate the manifest).
    pub full_verify: bool,
    /// Guard limit: max share (percent) of the pool's objects or bytes per step.
    pub max_delete_percent: u32,
    /// Guard limit: max objects per step.
    pub max_delete_objects: u64,
    /// Skip the mass-delete guard.
    pub force: bool,
    /// Local drive workspaces whose metadata also counts as references.
    pub workspaces: Vec<PathBuf>,
}

impl Default for RetireOptions {
    fn default() -> Self {
        Self {
            confirm: false,
            step: RetireStep::All,
            grace_seconds: DEFAULT_GRACE_DAYS * 86_400,
            include_removed: false,
            full_verify: false,
            max_delete_percent: DEFAULT_MAX_DELETE_PERCENT,
            max_delete_objects: DEFAULT_MAX_DELETE_OBJECTS,
            force: false,
            workspaces: vec![],
        }
    }
}

/// Why an archive (or its copy) is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum KeepReason {
    /// Drive revisions are never removed by retire.
    DriveArchive,
    /// The migration could not rebuild it: the original is all there is.
    Lost,
    /// The replacement did not pass the fresh re-verification.
    ReplacementUnverified,
    /// The original's manifest differs from the one that was migrated.
    OriginalChanged,
    /// The original's manifest could not be read now.
    OriginalUnreadable,
    /// An object is not inside the archive's own folder.
    ObjectOutsideArchive,
    /// Another manifest, the drive or another migration refers to it.
    Referenced,
    /// Some reference source could not be read: nothing is deleted.
    ReferencesUncertain,
    /// An account holding its objects could not be listed.
    Unreachable,
    /// A verified copy (it may be in use on another PC): never an orphan.
    VerifiedCopy,
    /// Not an id this migration generates.
    UnexpectedId,
}

/// An item that will not be cleaned up, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Kept {
    /// Archive id of the kept item.
    pub archive_id: String,
    /// Original or orphan.
    pub kind: ItemKind,
    /// Original file name, for reports.
    pub original_name: String,
    /// Main reason it is kept.
    pub reason: KeepReason,
    /// Human-readable detail behind the reason.
    pub detail: String,
}

/// Something that can be quarantined now, or is in quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Item {
    /// Archive id of the item to clean up.
    pub archive_id: String,
    /// Original or orphan.
    pub kind: ItemKind,
    /// Original file name, for reports.
    pub original_name: String,
    /// Replacement archive id (originals only).
    pub replacement: Option<String>,
    /// Exact objects that would be deleted.
    pub objects: Vec<RetireObject>,
}

impl Item {
    /// Sum of the objects' sizes in bytes.
    pub(crate) fn bytes(&self) -> u64 {
        self.objects.iter().map(|o| o.size).sum()
    }
}

/// Quarantine state of one item (from the folded records).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FossilState {
    /// In quarantine; `due_unix` not reached.
    Waiting,
    /// Grace elapsed: a confirmed run deletes it after a fresh check.
    Due,
    /// Deletion started (resumable); restore is no longer possible.
    Deleting,
    /// Every object deleted; the quarantine is over.
    Purged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Report view of one quarantined item, built by `ItemFossil::view`.
pub(crate) struct FossilView {
    /// The item and its quarantined objects.
    pub item: Item,
    /// Quarantine id (of the `Fossil` record).
    pub fossil_id: String,
    /// Current state at the report time.
    pub state: FossilState,
    /// Unix time (s) it was quarantined.
    pub since_unix: u64,
    /// Unix time (s) from which deletion is allowed.
    pub due_unix: u64,
    /// PC that quarantined it.
    pub by_pc: String,
    /// Objects not yet deleted (all of them unless `Deleting`).
    pub remaining: usize,
    /// A restore arrived after deletion started (it could not be honoured).
    #[serde(default)]
    pub restore_refused: bool,
    /// Result of the fresh check for an item still waiting or due: why it
    /// would be kept (deletion cancelled) or postponed; `None` when deletion
    /// may go ahead.
    #[serde(default)]
    pub blocked: Option<String>,
}

/// Objects and bytes per account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AccountBytes {
    /// Account root.
    pub root: String,
    /// Object count on that root.
    pub objects: u64,
    /// Bytes on that root.
    pub bytes: u64,
}

/// Mass-delete guard numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuardView {
    /// Objects currently on the pool's listed roots.
    pub pool_objects: u64,
    /// Bytes currently on the pool's listed roots.
    pub pool_bytes: u64,
    /// Configured percent limit (see `RetireOptions::max_delete_percent`).
    pub max_percent: u32,
    /// Configured object limit (see `RetireOptions::max_delete_objects`).
    pub max_objects: u64,
    /// Why quarantining the candidates now would be refused without --force.
    pub quarantine_refusal: Option<String>,
    /// Why deleting the due fossils now would be refused without --force.
    pub delete_refusal: Option<String>,
}

/// Everything `retire` found and (with --confirm) did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RetireReport {
    /// Migration being cleaned up.
    pub migration_id: String,
    /// Pool name.
    pub pool: String,
    /// Report time (Unix seconds).
    pub now_unix: u64,
    /// True unless `--confirm` was given.
    pub dry_run: bool,
    /// Not yet quarantined; `--confirm` moves them into quarantine.
    pub candidates: Vec<Item>,
    /// In quarantine (waiting, due or being deleted).
    pub quarantine: Vec<FossilView>,
    /// Fossils already fully purged (not listed in `quarantine`).
    pub purged: usize,
    /// Items that are not cleaned up, with reasons.
    pub kept: Vec<Kept>,
    /// Candidates' objects/bytes per account (what quarantine would free).
    pub candidate_accounts: Vec<AccountBytes>,
    /// Quarantined objects/bytes per account.
    pub quarantine_accounts: Vec<AccountBytes>,
    /// Originals' objects left on accounts that left the pool (not deleted).
    pub left_on_removed: Vec<AccountBytes>,
    /// Mass-delete guard numbers and refusals.
    pub guard: GuardView,
    /// Reference sources that could not be read (nothing is deleted then).
    pub uncertain: Vec<String>,
    /// What this run did.
    pub actions: Vec<String>,
}

/// Groups objects by account root.
pub(crate) fn per_account<'a>(
    objects: impl IntoIterator<Item = &'a RetireObject>,
) -> Vec<AccountBytes> {
    let mut by: BTreeMap<&str, AccountBytes> = BTreeMap::new();
    for object in objects {
        let slot = by
            .entry(object.root.as_str())
            .or_insert_with(|| AccountBytes {
                root: object.root.clone(),
                ..Default::default()
            });
        slot.objects += 1;
        slot.bytes += object.size;
    }
    by.into_values().collect()
}
