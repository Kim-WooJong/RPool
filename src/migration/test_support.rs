//! Synthetic manifests and an in-memory cloud for planner tests (no rclone).
use super::enumerate::{Cloud, CopyFeatures, RemoteListing};
use crate::manifest::content_root_v2;
use crate::prelude::*;
use crate::utils::{relative_remote_object, remote_join};

/// A valid v2 manifest of `size` bytes; shard `slot` of every group goes to
/// `remotes[slot % remotes.len()]`. `m == 0` means no coding.
pub(crate) fn manifest(
    id: &str,
    size: u64,
    shard: u64,
    k: usize,
    m: usize,
    remotes: &[&str],
) -> Manifest {
    let data = size.div_ceil(shard).max(1) as usize;
    let k_eff = if m == 0 { data } else { k };
    let groups = data.div_ceil(k_eff);
    let mut shards = Vec::new();
    let hash = |i: usize| blake3::hash(&i.to_le_bytes()).to_hex().to_string();
    for i in 0..data {
        let (group, slot) = if m == 0 { (0, 0) } else { (i / k, i % k) };
        let remote = remotes[if m == 0 { i } else { slot } % remotes.len()];
        let rel = if m == 0 {
            format!("{id}/shards/{i:08}.bin")
        } else {
            format!("{id}/data/{i:08}.bin")
        };
        shards.push(Shard {
            index: i as u32,
            offset: i as u64 * shard,
            size: size.saturating_sub(i as u64 * shard).min(shard),
            remote: remote.into(),
            object: remote_join(remote, &rel),
            blake3: hash(i),
            kind: ShardKind::Data,
            group: group as u32,
            slot: slot as u16,
        });
    }
    if m > 0 {
        for group in 0..groups {
            for p in 0..m {
                let index = data + group * m + p;
                let remote = remotes[(k + p) % remotes.len()];
                shards.push(Shard {
                    index: index as u32,
                    offset: 0,
                    size: shard,
                    remote: remote.into(),
                    object: remote_join(remote, &format!("{id}/parity/g{group:08}-p{p:03}.bin")),
                    blake3: hash(index),
                    kind: ShardKind::Parity,
                    group: group as u32,
                    slot: (k + p) as u16,
                });
            }
        }
    }
    let coding = (m > 0).then(|| Coding {
        algorithm: RS_ALGORITHM.into(),
        data_shards: k,
        parity_shards: m,
        stripe_size: 1048576,
    });
    let mut out = Manifest {
        version: 2,
        archive_id: id.into(),
        original_name: format!("{id}.bin"),
        original_size: size,
        shard_size: shard,
        created_unix: 100,
        content_root_blake3: String::new(),
        coding,
        shards,
    };
    out.content_root_blake3 =
        content_root_v2(out.original_size, out.shard_size, &out.coding, &out.shards);
    out
}

pub(crate) fn policy(remotes: &[&str], shard: u64, k: usize, m: usize) -> PoolDefinition {
    PoolDefinition {
        remotes: remotes.iter().map(|r| r.to_string()).collect(),
        shard_size: crate::models::shard_size::ShardSize::from_mib(shard / 1048576).unwrap(),
        data_shards: k,
        parity_shards: m,
        ..Default::default()
    }
}

pub(crate) fn set(remotes: &[&str]) -> BTreeSet<String> {
    remotes.iter().map(|r| r.to_string()).collect()
}

#[derive(Default)]
pub(crate) struct FakeCloud {
    pub listings: BTreeMap<String, RemoteListing>,
    pub files: BTreeMap<String, Vec<u8>>,
    pub configured: Option<BTreeSet<String>>,
    pub full: BTreeMap<String, Vec<(Shard, Probe)>>,
    pub domains: BTreeMap<String, String>,
    pub quota: Option<bool>,
    pub features: BTreeMap<String, CopyFeatures>,
    pub reads: Mutex<Vec<String>>,
    pub lists: Mutex<Vec<String>>,
}

impl FakeCloud {
    fn files_of(&mut self, remote: &str) -> &mut BTreeMap<String, u64> {
        let listing = self
            .listings
            .entry(remote.to_owned())
            .or_insert_with(|| RemoteListing::Listed(BTreeMap::new()));
        match listing {
            RemoteListing::Listed(files) => files,
            _ => panic!("remote {remote} is not listable"),
        }
    }
    /// Stores every shard of `manifest` and a manifest replica on each of
    /// `replicas`.
    pub(crate) fn store(&mut self, manifest: &Manifest, replicas: &[&str]) {
        for shard in &manifest.shards {
            let rel = relative_remote_object(&shard.remote, &shard.object).unwrap();
            self.files_of(&shard.remote).insert(rel, shard.size);
        }
        let bytes = serde_json::to_vec_pretty(manifest).unwrap();
        for remote in replicas {
            let rel = format!("{}/manifest.json", manifest.archive_id);
            self.files_of(remote)
                .insert(rel.clone(), bytes.len() as u64);
            self.files.insert(remote_join(remote, &rel), bytes.clone());
        }
    }
    /// Deletes a remote's whole directory (lists as empty).
    pub(crate) fn wipe(&mut self, remote: &str) {
        self.listings
            .insert(remote.into(), RemoteListing::Listed(BTreeMap::new()));
    }
}

impl Cloud for FakeCloud {
    fn configured(&self) -> Option<BTreeSet<String>> {
        self.configured.clone()
    }
    fn list(&self, remote: &str) -> RemoteListing {
        self.lists.lock().unwrap().push(remote.to_owned());
        self.listings
            .get(remote)
            .cloned()
            .unwrap_or_else(|| RemoteListing::Listed(BTreeMap::new()))
    }
    fn read(&self, address: &str) -> Result<Vec<u8>> {
        self.reads.lock().unwrap().push(address.to_owned());
        if Path::new(address).exists() {
            return Ok(fs::read(address)?);
        }
        self.files
            .get(address)
            .cloned()
            .ok_or_else(|| anyhow!("not found: {address}"))
    }
    fn probe_full(&self, manifest: &Manifest, _: usize) -> Result<Vec<(Shard, Probe)>> {
        self.full
            .get(&manifest.archive_id)
            .cloned()
            .ok_or_else(|| anyhow!("no full probe"))
    }
    fn failure_domain(&self, remote: &str) -> Option<String> {
        self.domains.get(remote).cloned()
    }
    fn quota_ok(&self, _: &PoolDefinition, _: &[Vec<PhysicalSpec>]) -> Option<bool> {
        self.quota
    }
    fn copy_features(&self, remote: &str) -> Option<CopyFeatures> {
        self.features.get(remote).copied()
    }
}
