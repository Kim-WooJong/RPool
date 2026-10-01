//! Per-account limits and lifetime: daily upload budgets (rolling 24 h,
//! shared by every RPool process on this computer), provider-reported upload
//! limits, bandwidth timetables, request-rate limits and inactivity warnings.
//!
//! An account is the remote at the bottom of a crypt/alias chain (the same
//! resolution as the per-namespace write cap in `rclone::write_base`).
//! Settings are portable (`account_limits.json`); the usage ledger is
//! machine-local (`account_usage.json`).
pub(crate) mod bandwidth;
pub(crate) mod budget;
pub(crate) mod edit;
pub(crate) mod inactivity;
pub(crate) mod ledger;
pub(crate) mod limits;
pub(crate) mod runtime;
pub(crate) mod store;
