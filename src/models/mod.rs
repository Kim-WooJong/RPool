//! Plain data types shared across RPool: manifests, pool definitions,
//! inventory, journals, scrub/health reports, placement, portable config and
//! secrets. Mostly serde structs; behavior lives in the modules that use them.
/// Erasure-coding parameters and shard kind.
mod coding;
/// Command history records.
mod history;
/// Saved integrity check summary and group health.
mod integrity_snapshot;
/// Local archive inventory.
mod inventory;
/// Upload resume journal.
mod journal;
/// Scrub report and per-shard health.
mod maintenance;
/// Archive manifest and shard entries.
mod manifest;
/// Shard placement policy.
mod placement;
/// Pool definitions and the pool store.
mod pool;
/// Portable configuration bundle.
mod portable_config;
/// Shard probe result.
mod probe;
/// Provider health report.
mod provider;
/// Resolved `put` settings.
mod put_options;
/// Account quota report.
mod quota;
/// Remote root store.
mod remote_root;
/// `get` resume state.
mod resume;
/// Crypt secret bundle for the encrypted vault.
pub(crate) mod secrets;
pub(crate) mod sensitive;
pub(crate) mod shard_size;
/// Upload plan, physical shard specs and generated parity.
mod upload;
pub(crate) mod volume;

pub(crate) use coding::{Coding, ShardKind};
pub(crate) use history::TaskRecord;
pub(crate) use integrity_snapshot::{GroupHealth, IntegritySnapshot, INTEGRITY_SNAPSHOT_VERSION};
pub(crate) use inventory::{InventoryEntry, InventoryStore};
pub(crate) use journal::UploadJournal;
pub(crate) use maintenance::{ScrubReport, ShardHealth};
pub(crate) use manifest::{Manifest, Shard};
pub(crate) use placement::Placement;
pub(crate) use pool::{PoolDefinition, PoolStore};
pub(crate) use portable_config::{
    PortableConfig, PortableCryptRemote, PortableGuiSettings, PortableSecretVault,
    CONFIG_SYNC_FORMAT, CONFIG_SYNC_VERSION, SECRET_VAULT_PATH,
};
pub(crate) use probe::Probe;
pub(crate) use provider::ProviderHealthReport;
pub(crate) use put_options::ResolvedPutOptions;
pub(crate) use quota::QuotaReport;
pub(crate) use remote_root::RemoteRootStore;
pub(crate) use resume::ResumeState;
pub(crate) use upload::{GeneratedParity, PhysicalSpec, PlanShard, UploadPlan};
