//! Saved pools (`pools.json`): loading, saving, validation, add/remove,
//! option and target resolution, capacity/estimates, browsing of a pool's
//! drive metadata, and reprocessing (re-encoding archives to a new layout).

pub(crate) mod browse;
pub(crate) mod browse_cache;
pub(crate) mod browse_generations;
mod browse_local;
pub(crate) mod capacity;
pub(crate) mod estimate;
/// Loading the pool store.
mod load;
/// Adding, replacing and removing pools.
mod manage;
/// Effective put options from a pool or explicit remotes.
mod resolve;
/// Atomic saving of the pool store.
mod save;
/// Target remotes of pool-wide commands.
mod targets;
/// Pool name and definition validation.
mod validate;

pub(crate) use load::load_pool_store;
pub(crate) use manage::{remove_pool, upsert_pool};
pub(crate) use resolve::resolve_put_options;
pub(crate) use save::save_pool_store;
pub(crate) use targets::resolve_target_remotes;
pub(crate) use validate::{validate_pool, validate_pool_name};

mod reprocess;
pub(crate) use reprocess::{
    build_plan, completed_reprocess_replacements, execute_plan, load_plan, reencode_manifest,
    ReprocessPlan,
};
