//! Immutable empty-folder records, deliberately outside the v6 file event
//! directory so older clients can continue syncing files.
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FolderRecord {
    pub version: u32,
    pub device: String,
    pub path: String,
    pub parents: Vec<String>,
    pub present: bool,
    /// Distinguishes a later recreation from an identical earlier create.
    pub nonce: String,
}

impl FolderRecord {
    pub(crate) fn new(
        device: &str,
        path: &str,
        parents: Vec<String>,
        present: bool,
    ) -> Result<Self> {
        let record = Self {
            version: 1,
            device: device.into(),
            path: path.into(),
            parents,
            present,
            nonce: super::namespace::random_id()?,
        };
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn id(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1 || self.nonce.len() != 64 || self.device.is_empty() {
            bail!("invalid folder record version or identity");
        }
        super::namespace::valid_path(&self.path)?;
        if self.parents.windows(2).any(|w| w[0] >= w[1])
            || self
                .parents
                .iter()
                .any(|p| p.len() != 64 || !p.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            bail!("invalid folder record ancestry");
        }
        Ok(())
    }
}

pub(crate) type Records = BTreeMap<String, FolderRecord>;

/// Heads of one folder's causal history, including concurrent creates.
pub(crate) fn heads(records: &Records, path: &str) -> Vec<String> {
    let referenced: BTreeSet<&str> = records
        .values()
        .filter(|r| r.path == path)
        .flat_map(|r| r.parents.iter().map(String::as_str))
        .collect();
    records
        .iter()
        .filter(|(id, r)| r.path == path && !referenced.contains(id.as_str()))
        .map(|(id, _)| id.clone())
        .collect()
}

/// An add wins against a concurrent removal. A file always wins against a
/// colliding folder, and file-implied spelling wins against case variants.
pub(crate) fn projected(records: &Records, files: &BTreeSet<String>) -> BTreeSet<String> {
    let mut candidates = BTreeSet::new();
    let referenced: BTreeSet<(&str, &str)> = records
        .values()
        .flat_map(|r| {
            r.parents
                .iter()
                .map(move |parent| (r.path.as_str(), parent.as_str()))
        })
        .collect();
    for (id, record) in records {
        if record.present && !referenced.contains(&(record.path.as_str(), id.as_str())) {
            candidates.insert(record.path.clone());
            for (index, _) in record.path.match_indices('/') {
                candidates.insert(record.path[..index].to_owned());
            }
        }
    }
    let mut file_names = BTreeSet::new();
    let mut file_directories = BTreeMap::new();
    for file in files {
        file_names.insert(file.to_lowercase());
        for (index, _) in file.match_indices('/') {
            let prefix = &file[..index];
            file_directories.insert(prefix.to_lowercase(), prefix);
        }
    }
    let mut result = BTreeSet::new();
    let mut chosen = BTreeMap::new();
    // Parents before children. Otherwise a lower-case child could select a
    // different spelling from an already selected ancestor.
    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_by_key(|p| (p.matches('/').count(), p.clone()));
    for path in candidates {
        let mut prefixes: Vec<&str> = path.match_indices('/').map(|(i, _)| &path[..i]).collect();
        prefixes.push(&path);
        let collides = prefixes.iter().any(|prefix| {
            let folded = prefix.to_lowercase();
            file_names.contains(&folded)
                || file_directories
                    .get(&folded)
                    .is_some_and(|actual| *actual != *prefix)
                || chosen
                    .get(&folded)
                    .is_some_and(|actual: &String| actual.as_str() != *prefix)
        });
        if !collides {
            chosen.insert(path.to_lowercase(), path.clone());
            result.insert(path);
        }
    }
    result
}

pub(crate) fn parse(id: &str, bytes: &[u8]) -> Result<FolderRecord> {
    let record: FolderRecord = serde_json::from_slice(bytes)?;
    record.validate()?;
    if record.id()? != id {
        bail!("folder record identity mismatch");
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::namespace::Namespace;
    use crate::mount::shared_model::Event;
    #[test]
    fn concurrent_create_survives_remove_and_recreate_has_new_id() {
        let mut records = Records::new();
        let a = FolderRecord::new("a", "X", vec![], true).unwrap();
        let b = FolderRecord::new("b", "X", vec![], true).unwrap();
        records.insert(a.id().unwrap(), a.clone());
        records.insert(b.id().unwrap(), b);
        let remove = FolderRecord::new("a", "X", vec![a.id().unwrap()], false).unwrap();
        records.insert(remove.id().unwrap(), remove.clone());
        assert!(projected(&records, &BTreeSet::new()).contains("X"));
        let both = heads(&records, "X");
        let remove_both = FolderRecord::new("a", "X", both, false).unwrap();
        records.insert(remove_both.id().unwrap(), remove_both.clone());
        assert!(!projected(&records, &BTreeSet::new()).contains("X"));
        let recreated = FolderRecord::new("a", "X", vec![remove_both.id().unwrap()], true).unwrap();
        assert_ne!(recreated.id().unwrap(), a.id().unwrap());
        records.insert(recreated.id().unwrap(), recreated);
        assert!(projected(&records, &BTreeSet::new()).contains("X"));
    }
    #[test]
    fn files_win_collisions_without_failing() {
        let mut records = Records::new();
        for p in ["Report", "a/b", "report/sub"] {
            let r = FolderRecord::new("a", p, vec![], true).unwrap();
            records.insert(r.id().unwrap(), r);
        }
        let files = BTreeSet::from(["report/x".to_owned(), "a".to_owned()]);
        let visible = projected(&records, &files);
        assert!(!visible.contains("Report"));
        assert!(!visible.contains("a/b"));
        assert!(visible.contains("report/sub"));
        assert!(projected(&records, &BTreeSet::new()).contains("Report"));
        assert!(
            !projected(&records, &BTreeSet::from(["Report/x".to_owned()])).contains("report/sub")
        );
    }
    #[test]
    fn peer_file_collision_does_not_wedge_namespace_or_reload() {
        for path in ["Report", "report/b.txt", "Report/b.txt"] {
            let temp = tempfile::tempdir().unwrap();
            let mut state = Namespace::create("pc").unwrap();
            state.version = 6;
            state.seed_folders().unwrap();
            state.add_folder("Report").unwrap();
            state.save(temp.path()).unwrap();
            let file = Event {
                version: 1,
                worker: "peer".into(),
                device: "peer".into(),
                path: path.into(),
                parents: vec![],
                content: Some(crate::mount::virtual_tests::content(b"data")),
            };
            let id = file.id().unwrap();
            state.ingest(BTreeMap::from([(id, file)])).unwrap();
            state.save(temp.path()).unwrap();
            let loaded = Namespace::load(temp.path(), "pc").unwrap();
            assert_eq!(
                loaded.directories.contains("Report"),
                path == "Report/b.txt",
                "{path}"
            );
        }
    }
    #[test]
    fn folder_records_survive_checkpoint_and_journal() {
        let temp = tempfile::tempdir().unwrap();
        let mut state = Namespace::create("pc").unwrap();
        state.seed_folders().unwrap();
        state.save(temp.path()).unwrap();
        state.add_folder("empty/nested").unwrap();
        state.save(temp.path()).unwrap();
        let mut loaded = Namespace::load(temp.path(), "pc").unwrap();
        assert!(loaded.directories.contains("empty"));
        assert!(loaded.directories.contains("empty/nested"));
        loaded.remove_folder("empty/nested").unwrap();
        loaded.save(temp.path()).unwrap();
        let reloaded = Namespace::load(temp.path(), "pc").unwrap();
        assert!(!reloaded.directories.contains("empty/nested"));
    }

    #[test]
    fn deleting_visible_case_variant_does_not_reveal_another() {
        let mut state = Namespace::create("pc").unwrap();
        state.seed_folders().unwrap();
        state.add_folder("Docs").unwrap();
        state.add_folder("docs").unwrap();
        assert_eq!(state.directories.len(), 1);
        state.remove_folder("Docs").unwrap();
        assert!(state.directories.is_empty());
    }
}
