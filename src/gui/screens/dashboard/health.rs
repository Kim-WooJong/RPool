use crate::gui::screens::dashboard::DashboardPool;
use crate::remote_root::remote_name;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoolHealth {
    Ready,
    Checking,
    Empty,
    MissingRemote,
}

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

pub(crate) fn pools_needing_attention(
    pools: &[DashboardPool],
    crypt_remotes: &[String],
) -> usize {
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
