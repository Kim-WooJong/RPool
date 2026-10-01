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
    pub address: String,
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
    pub version: u32,
    pub kind: RetireKind,
    /// Archive id of the item (the original or the orphan copy).
    pub item: String,
    pub item_kind: ItemKind,
    /// Id of the quarantine this record belongs to (the `Fossil` record's
    /// own `fossil_id`); a restored item gets a new one when quarantined again.
    pub fossil_id: String,
    pub pc_id: String,
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
    pub original_name: String,
    #[serde(default)]
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
pub(crate) struct RetireOptions {
    /// Without it nothing is written or deleted (dry run).
    pub confirm: bool,
    pub step: RetireStep,
    /// Grace given to new fossils.
    pub grace_seconds: u64,
    /// Also delete the originals' objects on accounts that left the pool
    /// (when they are still configured and listable).
    pub include_removed: bool,
    /// Re-verify replacements by reading every shard back (default: list
    /// sizes and validate the manifest).
    pub full_verify: bool,
    pub max_delete_percent: u32,
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
    pub archive_id: String,
    pub kind: ItemKind,
    pub original_name: String,
    pub reason: KeepReason,
    pub detail: String,
}

/// Something that can be quarantined now, or is in quarantine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Item {
    pub archive_id: String,
    pub kind: ItemKind,
    pub original_name: String,
    pub replacement: Option<String>,
    pub objects: Vec<RetireObject>,
}

impl Item {
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
    Purged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FossilView {
    pub item: Item,
    pub fossil_id: String,
    pub state: FossilState,
    pub since_unix: u64,
    pub due_unix: u64,
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
    pub root: String,
    pub objects: u64,
    pub bytes: u64,
}

/// Mass-delete guard numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GuardView {
    pub pool_objects: u64,
    pub pool_bytes: u64,
    pub max_percent: u32,
    pub max_objects: u64,
    /// Why quarantining the candidates now would be refused without --force.
    pub quarantine_refusal: Option<String>,
    /// Why deleting the due fossils now would be refused without --force.
    pub delete_refusal: Option<String>,
}

/// Everything `retire` found and (with --confirm) did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RetireReport {
    pub migration_id: String,
    pub pool: String,
    pub now_unix: u64,
    pub dry_run: bool,
    /// Not yet quarantined; `--confirm` moves them into quarantine.
    pub candidates: Vec<Item>,
    /// In quarantine (waiting, due or being deleted).
    pub quarantine: Vec<FossilView>,
    pub purged: usize,
    pub kept: Vec<Kept>,
    /// Candidates' objects/bytes per account (what quarantine would free).
    pub candidate_accounts: Vec<AccountBytes>,
    /// Quarantined objects/bytes per account.
    pub quarantine_accounts: Vec<AccountBytes>,
    /// Originals' objects left on accounts that left the pool (not deleted).
    pub left_on_removed: Vec<AccountBytes>,
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
