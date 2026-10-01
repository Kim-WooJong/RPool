//! Pool estimate and bottleneck selection from per-remote speeds.
//!
//! A pool upload sends every shard of a coding group to its remote at the
//! same time, so the remote that needs longest for its share decides:
//! with `s_i` the fraction of all shards on remote `i` and `up_i` its upload
//! rate, writing `B` plaintext bytes (B·(k+m)/k shard bytes) takes
//! `B·(k+m)/k · max_i(s_i/up_i)`, i.e.
//!
//! ```text
//! upload   = (k/(k+m)) / max_i(s_i / up_i)
//! download = 1 / max_i(d_i / down_i)        d_i = fraction of DATA shards on i
//! ```
//!
//! (a healthy read fetches data shards only). The bottleneck is the remote
//! with the largest term. Without a pool the bottleneck is simply the slowest
//! remote. Shares come from the pool's placement (see [`placement_shares`]).
use super::model::PoolEstimate;
use crate::prelude::*;

/// Coding groups simulated to derive shares; enough for placement to cycle.
const SIMULATED_GROUPS_PER_REMOTE: usize = 4;

/// Per-remote fractions (each vector sums to 1 when non-empty).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shares {
    /// Fraction of all shards (data + parity).
    pub all: Vec<f64>,
    /// Fraction of data shards.
    pub data: Vec<f64>,
}

impl Shares {
    /// Fractions from a shard→remote assignment; `is_data[j]` tells whether
    /// shard `j` is a data shard.
    pub(crate) fn from_assignment(assignment: &[usize], is_data: &[bool], remotes: usize) -> Self {
        let mut all = vec![0.0; remotes];
        let mut data = vec![0.0; remotes];
        let total = assignment.len().max(1) as f64;
        let data_total = is_data.iter().filter(|d| **d).count().max(1) as f64;
        for (&remote, &is_data) in assignment.iter().zip(is_data) {
            if remote < remotes {
                all[remote] += 1.0 / total;
                if is_data {
                    data[remote] += 1.0 / data_total;
                }
            }
        }
        Self { all, data }
    }
}

/// Shares under the pool's placement: simulates a few coding groups with the
/// pool's shard size and asks the placement module where they would go.
/// Round-robin needs no network (it is `balanced_assign`); quota-based placements query the remotes'
/// free space and fall back to the round-robin result when that fails.
pub(crate) fn placement_shares(
    rclone: &str,
    remotes: &[String],
    pool: &PoolDefinition,
) -> Result<Shares> {
    if remotes.is_empty() {
        bail!("pool has no remotes");
    }
    let shard = pool.shard_bytes()?.get();
    let k = pool.data_shards.max(1);
    let coding = (pool.parity_shards > 0).then(|| Coding {
        algorithm: RS_ALGORITHM.into(),
        data_shards: k,
        parity_shards: pool.parity_shards,
        stripe_size: 1048576,
    });
    let groups = (remotes.len() * SIMULATED_GROUPS_PER_REMOTE) as u64;
    let size = shard.saturating_mul(k as u64).saturating_mul(groups);
    let specs = crate::planning::physical_specs(size, shard, coding.as_ref())?;
    let is_data = data_mask(&specs, coding.as_ref().map(|_| k));
    let parity = coding.as_ref().map(|c| c.parity_shards);
    let assign =
        |placement| crate::placement::assign_remotes(rclone, remotes, &specs, placement, parity);
    let assignment = assign(pool.placement).or_else(|error| {
        if pool.placement == Placement::RoundRobin {
            return Err(error);
        }
        let _ = writeln!(
            std::io::stderr(),
            "speed test: {} placement unavailable for the estimate ({}); assuming even shares",
            pool.placement.cli_value(),
            first_line(&format!("{error:#}"))
        );
        assign(Placement::RoundRobin)
    })?;
    Ok(Shares::from_assignment(
        &assignment,
        &is_data,
        remotes.len(),
    ))
}

/// Data/parity flags in the planner's order: per group `k` data, then parity.
fn data_mask(specs: &[PhysicalSpec], k: Option<usize>) -> Vec<bool> {
    let Some(k) = k else {
        return vec![true; specs.len()];
    };
    let mut position = 0usize;
    let mut group = None;
    specs
        .iter()
        .map(|spec| {
            if group != Some(spec.group) {
                group = Some(spec.group);
                position = 0;
            }
            position += 1;
            position <= k
        })
        .collect()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn usable(speed: Option<f64>) -> Option<f64> {
    speed.filter(|s| s.is_finite() && *s > 0.0)
}

/// Index with the largest `share / speed` among remotes with a usable speed
/// (ties: the first). `None` when no remote has one.
pub(crate) fn weighted_bottleneck(shares: &[f64], speeds: &[Option<f64>]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, speed) in speeds.iter().enumerate() {
        let Some(speed) = usable(*speed) else {
            continue;
        };
        let term = shares.get(i).copied().unwrap_or(0.0) / speed;
        if best.is_none_or(|(_, b)| term > b) {
            best = Some((i, term));
        }
    }
    best.map(|(i, _)| i)
}

/// Slowest remote with a usable speed (ties: the first).
pub(crate) fn raw_bottleneck(speeds: &[Option<f64>]) -> Option<usize> {
    weighted_bottleneck(&vec![1.0; speeds.len()], speeds)
}

/// The pool estimate; `None` unless every remote has usable speeds.
pub(crate) fn pool_estimate(
    data_shards: usize,
    parity_shards: usize,
    shares: &Shares,
    upload: &[Option<f64>],
    download: &[Option<f64>],
) -> Option<PoolEstimate> {
    let n = shares.all.len();
    if n == 0 || upload.len() != n || download.len() != n {
        return None;
    }
    let up: Vec<f64> = upload.iter().map(|s| usable(*s)).collect::<Option<_>>()?;
    let down: Vec<f64> = download.iter().map(|s| usable(*s)).collect::<Option<_>>()?;
    let max_term = |share: &[f64], speed: &[f64]| {
        share
            .iter()
            .zip(speed)
            .map(|(s, v)| s / v)
            .fold(0.0f64, f64::max)
    };
    let k = data_shards.max(1) as f64;
    let efficiency = if parity_shards == 0 {
        1.0
    } else {
        k / (k + parity_shards as f64)
    };
    let up_term = max_term(&shares.all, &up);
    let down_term = max_term(&shares.data, &down);
    if up_term <= 0.0 || down_term <= 0.0 {
        return None;
    }
    Some(PoolEstimate {
        data_shards,
        parity_shards,
        upload_bytes_per_s: efficiency / up_term,
        download_bytes_per_s: 1.0 / down_term,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn even(n: usize) -> Shares {
        Shares {
            all: vec![1.0 / n as f64; n],
            data: vec![1.0 / n as f64; n],
        }
    }

    #[test]
    fn equal_shares_and_speeds_sum_the_remotes() {
        // 3 remotes at 10 B/s each, RS 2+1: shard bytes move at 30 B/s,
        // 2/3 of them are plaintext.
        let speeds = vec![Some(10.0); 3];
        let e = pool_estimate(2, 1, &even(3), &speeds, &speeds).unwrap();
        assert!(close(e.upload_bytes_per_s, 20.0), "{e:?}");
        assert!(close(e.download_bytes_per_s, 30.0), "{e:?}");
        assert_eq!((e.data_shards, e.parity_shards), (2, 1));
        // No parity: no overhead.
        let e = pool_estimate(4, 0, &even(3), &speeds, &speeds).unwrap();
        assert!(close(e.upload_bytes_per_s, 30.0));
    }

    #[test]
    fn one_slow_remote_limits_the_pool() {
        let up = vec![Some(100.0), Some(100.0), Some(10.0)];
        let down = vec![Some(100.0), Some(5.0), Some(100.0)];
        let e = pool_estimate(2, 1, &even(3), &up, &down).unwrap();
        // max(s/up) = (1/3)/10 → upload = (2/3) / (1/30) = 20.
        assert!(close(e.upload_bytes_per_s, 20.0), "{e:?}");
        // max(d/down) = (1/3)/5 → download = 15.
        assert!(close(e.download_bytes_per_s, 15.0), "{e:?}");
        assert_eq!(weighted_bottleneck(&even(3).all, &up), Some(2));
        assert_eq!(weighted_bottleneck(&even(3).data, &down), Some(1));
    }

    #[test]
    fn uneven_placement_moves_the_bottleneck() {
        // Remote 0 holds half of everything but is the fastest; remote 1 is
        // slower but holds little, so remote 0 still decides.
        let shares = Shares {
            all: vec![0.5, 0.1, 0.4],
            data: vec![0.6, 0.0, 0.4],
        };
        let up = vec![Some(40.0), Some(10.0), Some(40.0)];
        let down = vec![Some(40.0), Some(1.0), Some(40.0)];
        assert_eq!(raw_bottleneck(&up), Some(1));
        assert_eq!(weighted_bottleneck(&shares.all, &up), Some(0));
        // Remote 1 holds no data shards: it never limits a read.
        assert_eq!(weighted_bottleneck(&shares.data, &down), Some(0));
        let e = pool_estimate(3, 2, &shares, &up, &down).unwrap();
        assert!(close(e.upload_bytes_per_s, 0.6 / (0.5 / 40.0)));
        assert!(close(e.download_bytes_per_s, 1.0 / (0.6 / 40.0)));
    }

    #[test]
    fn missing_speeds_give_no_estimate_but_a_bottleneck_among_the_rest() {
        let speeds = vec![Some(10.0), None, Some(5.0)];
        assert!(pool_estimate(2, 1, &even(3), &speeds, &speeds).is_none());
        assert_eq!(raw_bottleneck(&speeds), Some(2));
        assert_eq!(raw_bottleneck(&[None, None]), None);
        assert_eq!(raw_bottleneck(&[Some(0.0), Some(f64::NAN)]), None);
        assert_eq!(
            raw_bottleneck(&[Some(3.0), Some(3.0)]),
            Some(0),
            "ties: first"
        );
    }

    #[test]
    fn shares_from_round_robin_placement() {
        let assignment = [0, 1, 2, 0, 1, 2];
        let is_data = [true, true, false, true, true, false];
        let shares = Shares::from_assignment(&assignment, &is_data, 3);
        for s in &shares.all {
            assert!(close(*s, 1.0 / 3.0));
        }
        assert!(close(shares.data[0], 0.5) && close(shares.data[1], 0.5));
        assert!(close(shares.data[2], 0.0));
    }

    #[test]
    fn data_mask_follows_planner_order() {
        let specs = crate::planning::physical_specs(
            8,
            1,
            Some(&Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: 4,
                parity_shards: 2,
                stripe_size: 1,
            }),
        )
        .unwrap();
        assert_eq!(specs.len(), 12);
        let mask = data_mask(&specs, Some(4));
        assert_eq!(mask.iter().filter(|d| **d).count(), 8);
        assert_eq!(&mask[..6], &[true, true, true, true, false, false]);
        assert_eq!(data_mask(&specs, None), vec![true; 12]);
    }

    #[test]
    fn round_robin_pool_shares_are_even() {
        let pool = PoolDefinition {
            remotes: vec!["a:".into(), "b:".into(), "c:".into()],
            placement: Placement::RoundRobin,
            data_shards: 2,
            parity_shards: 1,
            ..PoolDefinition::default()
        };
        let shares = placement_shares("rclone-not-run", &pool.remotes, &pool).unwrap();
        for s in &shares.all {
            assert!(close(*s, 1.0 / 3.0), "{shares:?}");
        }
        // RS 2+1 over 3 remotes: the round-robin cursor puts every group's
        // parity on the third remote, so it serves no healthy reads.
        assert!(close(shares.data[0], 0.5) && close(shares.data[1], 0.5));
        assert!(close(shares.data[2], 0.0), "{shares:?}");
        // With 4 remotes the groups rotate and data spreads evenly.
        let mut four = pool.clone();
        four.remotes.push("d:".into());
        let shares = placement_shares("rclone-not-run", &four.remotes, &four).unwrap();
        for s in shares.all.iter().chain(&shares.data) {
            assert!(close(*s, 0.25), "{shares:?}");
        }
    }
}
