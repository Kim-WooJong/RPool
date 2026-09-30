//! Pool change migration: when a pool's accounts or coding change, move every
//! affected archive onto the new policy. Plan first (what moves, bytes, ETA,
//! what is already unrecoverable), then run with a journal in the cloud so any
//! PC can resume. Phase 1 covers uploaded archives; the drive follows later
//! (see docs/POOL_MIGRATION_DESIGN.md).
//!
//! CONTRACT between the work packages: `model` types and the signatures in
//! `plan`, `journal`, `relocate`, `speed`, `execute` and `status` are shared; keep them stable.
mod classify;
mod enumerate;
mod estimate;
pub(crate) mod execute;
pub(crate) mod journal;
pub(crate) mod model;
pub(crate) mod plan;
mod probe;
pub(crate) mod relocate;
pub(crate) mod speed;
pub(crate) mod status;
#[cfg(test)]
mod test_support;
