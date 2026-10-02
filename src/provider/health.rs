//! Provider health check: probes each remote and joins the result with its
//! quota report. Used by `rpool provider health`.
use crate::prelude::*;
use crate::storage::admin::{collect_quota_reports, unavailable_quota, BackendAdmin, RcloneAdmin};
use std::time::Instant;
/// Checks `remotes` through the real rclone admin backend; see
/// [`check_providers_with_admin`]. Called by `commands::provider::health`.
pub(crate) fn check_providers(
    rclone: &str,
    remotes: &[String],
    workers: usize,
) -> Result<Vec<ProviderHealthReport>> {
    check_providers_with_admin(&RcloneAdmin::inherited(rclone), remotes, workers)
}
/// Probes every remote in parallel (up to `workers` threads), measuring
/// latency in ms, and attaches the quota of the account each remote resolves
/// to (quotas are fetched once per account). Unresolvable capacity yields an
/// unavailable quota instead of failing. Reports are sorted by remote name;
/// `workers` must be non-zero. The admin is injectable for tests.
pub(crate) fn check_providers_with_admin(
    admin: &dyn BackendAdmin,
    remotes: &[String],
    workers: usize,
) -> Result<Vec<ProviderHealthReport>> {
    if workers == 0 {
        bail!("workers must be greater than zero");
    }
    let catalog = admin.catalog().map_err(|e| format!("{e:#}"));
    let bindings = remotes
        .iter()
        .map(|remote| match &catalog {
            Ok(c) => c
                .capacity(remote)
                .map(|b| b.target)
                .map_err(|e| format!("{e:#}")),
            Err(e) => Err(e.clone()),
        })
        .collect::<Vec<_>>();
    let targets = bindings
        .iter()
        .filter_map(|r| r.as_ref().ok().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let quotas = collect_quota_reports(admin, &targets, workers)?
        .into_iter()
        .map(|q| (q.remote.clone(), q))
        .collect::<BTreeMap<_, _>>();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.min(remotes.len().max(1)))
        .build()?;
    let mut reports = pool.install(|| {
        remotes
            .par_iter()
            .zip(bindings.par_iter())
            .map(|(remote, binding)| {
                let started = Instant::now();
                let result = admin.probe(remote);
                let latency_ms = started.elapsed().as_millis();
                let quota = match binding {
                    Ok(target) => quotas[target].clone(),
                    Err(error) => {
                        unavailable_quota(remote, format!("capacity unresolved: {error}"))
                    }
                };
                ProviderHealthReport {
                    remote: remote.clone(),
                    accessible: result.is_ok(),
                    latency_ms,
                    quota,
                    error: result.err().map(|e| format!("{e:#}")),
                }
            })
            .collect::<Vec<_>>()
    });
    reports.sort_by(|a, b| a.remote.cmp(&b.remote));
    Ok(reports)
}
