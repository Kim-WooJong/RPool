//! References that keep an object alive: every manifest this PC can see
//! (local inventory and the manifest replicas in the cloud), the drive's
//! metadata, and other migrations still in progress. Built fresh right before
//! a quarantine or a deletion; an archive is referenced when anything other
//! than the archive itself names it or one of its objects.
use crate::prelude::*;

/// Who refers to an archive id or an object, for the report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct References {
    /// Token (archive id or path component) -> (owner archive id, source).
    tokens: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Object address -> (owner archive id, source).
    objects: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Sources that could not be read. While any is listed, nothing is
    /// deleted: an unread manifest might refer to anything.
    pub uncertain: Vec<String>,
}

/// Owner used for references that do not belong to an archive.
const NO_OWNER: &str = "";

/// Path components and names of a string: archive ids appear as one
/// component of an object address (`remote:root/<id>/data/…`).
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(['/', ':', '\\']).filter(|t| !t.is_empty())
}

impl References {
    /// A manifest of `manifest.archive_id` read from `source`.
    pub(crate) fn add_manifest(&mut self, source: &str, manifest: &Manifest) {
        let owner = manifest.archive_id.as_str();
        for shard in &manifest.shards {
            self.objects
                .entry(shard.object.clone())
                .or_default()
                .insert((owner.into(), source.into()));
            for token in tokens(&shard.object) {
                if token != owner {
                    self.add_token_owned(token, owner, source);
                }
            }
        }
        self.add_token_owned(owner, owner, source);
    }

    fn add_token_owned(&mut self, token: &str, owner: &str, source: &str) {
        self.tokens
            .entry(token.into())
            .or_default()
            .insert((owner.into(), source.into()));
    }

    /// Every string of some metadata (drive events, checkpoints, other
    /// migrations): each path component counts as a reference.
    pub(crate) fn add_text(&mut self, source: &str, text: &str) {
        self.add_token_owned(text, NO_OWNER, source);
        for token in tokens(text) {
            self.add_token_owned(token, NO_OWNER, source);
        }
        self.objects
            .entry(text.into())
            .or_default()
            .insert((NO_OWNER.into(), source.into()));
    }

    /// Every string value and key inside `value`.
    pub(crate) fn add_json(&mut self, source: &str, value: &Value) {
        match value {
            Value::String(text) => self.add_text(source, text),
            Value::Array(items) => items.iter().for_each(|v| self.add_json(source, v)),
            Value::Object(map) => {
                for (key, v) in map {
                    self.add_text(source, key);
                    self.add_json(source, v);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn uncertain(&mut self, what: String) {
        self.uncertain.push(what);
    }

    /// Sources other than `archive_id` itself that name it or any of
    /// `objects`; empty when unreferenced.
    pub(crate) fn referrers<'a>(
        &self,
        archive_id: &str,
        objects: impl IntoIterator<Item = &'a str>,
    ) -> BTreeSet<String> {
        let foreign = |set: &BTreeSet<(String, String)>| -> Vec<String> {
            set.iter()
                .filter(|(owner, _)| owner != archive_id)
                .map(|(_, source)| source.clone())
                .collect()
        };
        let mut out: BTreeSet<String> = BTreeSet::new();
        if let Some(set) = self.tokens.get(archive_id) {
            out.extend(foreign(set));
        }
        for object in objects {
            if let Some(set) = self.objects.get(object) {
                out.extend(foreign(set));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::test_support::manifest;

    #[test]
    fn own_manifest_is_not_a_reference_but_others_are() {
        let mut refs = References::default();
        let own = manifest("old", 10, 4, 2, 1, &["a:", "b:", "c:"]);
        refs.add_manifest("a:old/manifest.json", &own);
        let objects: Vec<&str> = own.shards.iter().map(|s| s.object.as_str()).collect();
        assert!(refs.referrers("old", objects.clone()).is_empty());
        // A different archive borrowing one object of "old".
        let mut other = manifest("new", 10, 4, 2, 1, &["a:", "b:", "c:"]);
        other.shards[0].object = own.shards[0].object.clone();
        refs.add_manifest("inventory new", &other);
        assert_eq!(
            refs.referrers("old", objects.clone()),
            BTreeSet::from(["inventory new".to_string()])
        );
        // The drive naming the archive id anywhere keeps it.
        let mut drive = References::default();
        drive.add_json(
            "drive v6",
            &serde_json::json!({"content": {"manifest": {"archive_id": "old"}}}),
        );
        assert_eq!(drive.referrers("old", []).len(), 1);
        assert!(drive.referrers("unrelated", []).is_empty());
        drive.add_text("drive", "x:root/zzz/data/0.bin");
        assert_eq!(drive.referrers("zzz", []).len(), 1);
    }
}
