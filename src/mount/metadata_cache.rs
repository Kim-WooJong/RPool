//! Workspace-local memory of ingested checkpoint chunks (chunk id -> record
//! ids) and whether the compaction gate was seen. Only an optimization: a
//! missing or unreadable file means "read every chunk again".
use super::metadata_checkpoint_model::Ids;
use crate::prelude::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct Cache {
    pub chunks: BTreeMap<String, Ids>,
    pub gate: bool,
}
impl Cache {
    pub(crate) fn path(root: &Path, family: &str) -> PathBuf {
        root.join(format!("metadata-checkpoints-{family}.json"))
    }
    pub(crate) fn load(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match crate::utils::read_json::<Self>(path) {
            Ok(cache) => cache,
            Err(error) => {
                eprintln!("Checkpoint cache unreadable ({error:#}); checkpoints are read again");
                Self::default()
            }
        }
    }
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        super::namespace::durable_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_or_corrupt_cache_is_empty_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = Cache::path(dir.path(), "v6");
        assert_eq!(Cache::load(&path), Cache::default());
        let mut cache = Cache {
            gate: true,
            ..Default::default()
        };
        cache
            .chunks
            .insert("a".repeat(64), [("events".into(), BTreeSet::new())].into());
        cache.save(&path).unwrap();
        assert_eq!(Cache::load(&path), cache);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Cache::load(&path), Cache::default());
    }
}
