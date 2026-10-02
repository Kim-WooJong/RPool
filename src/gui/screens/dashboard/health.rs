//! Pool readiness on the overview page, from the pool's remotes and the
//! discovered crypt remotes.
use crate::gui::screens::dashboard::DashboardPool;
use crate::remote_root::remote_name;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Readiness of a pool for the overview badges and warnings.
pub(crate) enum PoolHealth {
    /// Every pool remote is a discovered crypt remote.
    Ready,
    /// Provider discovery has not reported crypt remotes yet.
    Checking,
    /// The pool has no remotes.
    Empty,
    /// A pool remote is not among the discovered crypt remotes.
    MissingRemote,
}

/// Classify a pool against the discovered crypt remotes (compared by remote name).
pub(crate) fn pool_health(pool: &DashboardPool, crypt_remotes: &[String]) -> PoolHealth {
    if pool.remotes.is_empty() {
        return PoolHealth::Empty;
    }
    if crypt_remotes.is_empty() {
        return PoolHealth::Checking;
    }

    let configured: BTreeSet<String> = crypt_remotes
        .iter()
        .filter_map(|remote| remote_name(remote).ok().map(ToOwned::to_owned))
        .collect();

    let missing = pool
        .remotes
        .iter()
        .filter_map(|remote| remote_name(remote).ok())
        .any(|name| !configured.contains(name));

    if missing {
        PoolHealth::MissingRemote
    } else {
        PoolHealth::Ready
    }
}

/// Pools that are `Empty` or `MissingRemote`; shown in the overview summary.
pub(crate) fn pools_needing_attention(pools: &[DashboardPool], crypt_remotes: &[String]) -> usize {
    pools
        .iter()
        .filter(|pool| {
            matches!(
                pool_health(pool, crypt_remotes),
                PoolHealth::Empty | PoolHealth::MissingRemote
            )
        })
        .count()
}
