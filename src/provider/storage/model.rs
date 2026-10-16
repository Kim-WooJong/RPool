//! Reports of the provider storage manager: the grouped view (`provider
//! storage list --json`, read by the GUI) and the result of a cleanup run
//! (`provider storage clean --json`).
use crate::prelude::*;

/// Report format version.
pub(crate) const VERSION: u32 = 1;

/// Who owns a group of stored data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroupKind {
    /// Needed by a configured pool (current drive generation, manifests,
    /// inventory, migration journals, migrations in progress). Never deleted.
    InUse,
    /// Data only an older drive generation of a configured pool names.
    OldGeneration,
    /// Metadata (and the data only it names) of a pool that is not
    /// configured on this provider.
    ForeignPool,
    /// Data only finished or abandoned migrations name; `rpool pool migrate
    /// retire` cleans those up. Never deleted here.
    MigrationLeftovers,
    /// An archive with a manifest that nothing on this PC names (an upload
    /// of another PC, or of a removed pool). Never deleted.
    UnknownPutArchive,
    /// Shards without a manifest or a drive upload no metadata names yet.
    /// Never deleted.
    IncompleteUpload,
    /// Files of interrupted speed tests.
    SpeedTestLeftovers,
    /// This manager's own journal. Never deleted.
    Journal,
    /// Not written by RPool in its layout. Never deleted.
    NotRpool,
}

/// An explicit acknowledgement a group needs before it may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ack {
    /// `--include-unconfirmed-generations`: no recorded migration proves that
    /// no PC still uses the generation.
    UnconfirmedGenerations,
    /// `--include-foreign-pools`: the pool is not configured on this PC.
    ForeignPools,
}

impl Ack {
    /// The CLI flag that gives this acknowledgement.
    pub(crate) fn flag(self) -> &'static str {
        match self {
            Ack::UnconfirmedGenerations => "--include-unconfirmed-generations",
            Ack::ForeignPools => "--include-foreign-pools",
        }
    }
}

/// Pending marks of a group's units (from the journal).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pending {
    /// Units marked for deletion (not deleted yet).
    pub marked: u64,
    /// Bytes of the marked units, as recorded.
    pub bytes: u64,
    /// Earliest time a marked unit may be deleted (unix seconds).
    pub first_due_unix: Option<u64>,
    /// Time every marked unit may be deleted (unix seconds).
    pub last_due_unix: Option<u64>,
    /// Units whose deletion started and did not finish.
    pub deleting: u64,
}

/// One group of the view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Group {
    /// Stable id, used by `clean --group`.
    pub id: String,
    /// Owner category.
    pub kind: GroupKind,
    /// Configured pool the group belongs to, if any.
    pub pool: Option<String>,
    /// Pool-sync scope of a foreign pool or generation (64 hex).
    pub scope: Option<String>,
    /// Drive generations involved, labelled (`first`, `epoch 1a2b…`).
    #[serde(default)]
    pub generations: Vec<String>,
    /// Units (archive folders, metadata generations, runs…).
    pub units: u64,
    /// Archive folders among the units.
    pub archives: u64,
    /// Stored objects.
    pub objects: u64,
    /// Stored bytes.
    pub bytes: u64,
    /// The group may be deleted (possibly after the acknowledgements in `needs`).
    pub deletable: bool,
    /// Acknowledgements required before deleting.
    #[serde(default)]
    pub needs: Vec<Ack>,
    /// Deleted without a grace period (speed-test leftovers).
    #[serde(default)]
    pub immediate: bool,
    /// Why the group is kept or may go.
    pub reason: String,
    /// Marks from earlier cleanup runs.
    #[serde(default)]
    pub pending: Pending,
}

/// The grouped content of one provider (`provider storage list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageView {
    /// Always [`VERSION`].
    pub version: u32,
    /// Provider root that was listed (`remote:path`).
    pub remote: String,
    /// Configured pools that use this provider.
    pub pools: Vec<String>,
    /// Groups, kept ones first.
    pub groups: Vec<Group>,
    /// Objects stored on the provider (listed).
    pub objects: u64,
    /// Bytes stored on the provider (listed).
    pub bytes: u64,
    /// Sources that could not be read: while any is listed, nothing is deletable.
    pub uncertain: Vec<String>,
    /// Names at the provider root that do not decrypt with this crypt
    /// remote (never touched); `None` when it could not be counted.
    pub undecryptable: Option<u64>,
    /// Days between marking and deleting.
    pub grace_days: u32,
    /// Extra explanations.
    pub notes: Vec<String>,
}

/// Units, objects and bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Totals {
    /// Units.
    pub units: u64,
    /// Stored objects.
    pub objects: u64,
    /// Stored bytes.
    pub bytes: u64,
}

/// What a cleanup run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanMode {
    /// Nothing written (no `--confirm`).
    Preview,
    /// Marks published and/or due data deleted.
    Applied,
    /// Pending marks dropped (`--cancel`).
    Cancelled,
    /// A source could not be read: nothing was marked or deleted.
    Postponed,
}

/// Result of `provider storage clean`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CleanReport {
    /// Always [`VERSION`].
    pub version: u32,
    /// Provider root (`remote:path`).
    pub remote: String,
    /// What the run did.
    pub mode: CleanMode,
    /// Selected group ids.
    pub groups: Vec<String>,
    /// Not marked yet: marked by a confirmed run.
    pub new: Totals,
    /// Marked, grace period not over.
    pub waiting: Totals,
    /// Grace period over (or deletion interrupted): deleted by a confirmed run.
    pub due: Totals,
    /// Deleted by this run.
    pub deleted: Totals,
    /// Marks released because the data is needed again (or by `--cancel`).
    pub released: u64,
    /// Earliest time marked data may be deleted.
    pub next_deletion_unix: Option<u64>,
    /// Why due data exceeds the mass-delete guard, if it does.
    pub guard: Option<String>,
    /// Unreadable sources that postponed the run.
    pub postponed: Vec<String>,
    /// Days between marking and deleting.
    pub grace_days: u32,
    /// Extra explanations.
    pub notes: Vec<String>,
}

impl CleanReport {
    /// An empty report.
    pub(crate) fn new(remote: &str, mode: CleanMode, groups: &[String], grace_days: u32) -> Self {
        Self {
            version: VERSION,
            remote: remote.into(),
            mode,
            groups: groups.to_vec(),
            new: Totals::default(),
            waiting: Totals::default(),
            due: Totals::default(),
            deleted: Totals::default(),
            released: 0,
            next_deletion_unix: None,
            guard: None,
            postponed: Vec::new(),
            grace_days,
            notes: Vec::new(),
        }
    }
}
