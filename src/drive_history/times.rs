//! When records reached the cloud. v6 events carry no timestamp (adding a field would change their content-addressed ids, so
//! older RPool would reject them). The object ModTime of a record in the
//! cloud listing is used instead: the upload time, identical on every PC.
//! Records only present in a metadata checkpoint (listing deleted after the
//! compaction grace period) have no time; callers treat them as old.
use crate::prelude::*;

#[derive(Deserialize)]
/// One entry of an `rclone lsjson -R` listing of a generation root.
pub(crate) struct Listed {
    #[serde(rename = "Path")]
    /// Path relative to the listed root.
    pub path: String,
    #[serde(rename = "ModTime", default)]
    /// Object ModTime (RFC 3339); empty when absent.
    pub mod_time: String,
    #[serde(rename = "IsDir", default)]
    /// Directory entries are skipped.
    pub is_dir: bool,
}

/// Record id of a listed path `…/events/<64 hex>.json` outside checkpoints
/// and history marks, relative to one generation root.
fn record_id(path: &str) -> Option<&str> {
    if path.starts_with("checkpoints/")
        || path.starts_with("history/")
        || path.starts_with("epochs/")
    {
        return None;
    }
    let name = path.strip_suffix(".json")?;
    let (dir, id) = name.rsplit_once('/')?;
    let valid = id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (valid && dir == "events").then_some(id)
}

/// Earliest time per record over the given replica listings.
pub(crate) fn from_listings(listings: &[Vec<Listed>]) -> BTreeMap<String, u64> {
    let mut times = BTreeMap::new();
    for listing in listings {
        for item in listing.iter().filter(|i| !i.is_dir) {
            let Some(id) = record_id(&item.path) else {
                continue;
            };
            let Some(nanos) = crate::pool::browse_generations::parse_rfc3339(&item.mod_time) else {
                continue;
            };
            let seconds = u64::try_from(nanos.div_euclid(1_000_000_000)).unwrap_or(0);
            times
                .entry(id.to_owned())
                .and_modify(|t: &mut u64| *t = (*t).min(seconds))
                .or_insert(seconds);
        }
    }
    times
}

/// Lists every generation root (one recursive listing per replica). A
/// replica that cannot be listed only loses times (`Err` text returned as a
/// note); history itself was already read through the normal record paths.
pub(crate) fn list(rclone: &str, roots: &[String]) -> (BTreeMap<String, u64>, Vec<String>) {
    use crate::storage::traits::OperationContext;
    let context = crate::storage::rclone::RcloneContext::inherited(rclone);
    let mut listings = Vec::new();
    let mut notes = Vec::new();
    for root in roots {
        match context
            .list_recursive(&OperationContext::none(), root)
            .map_err(|e| anyhow!("{e}"))
            .and_then(|bytes| Ok(serde_json::from_slice::<Vec<Listed>>(&bytes)?))
        {
            Ok(listing) => listings.push(listing),
            Err(error) => notes.push(format!("record times unavailable on {root}: {error:#}")),
        }
    }
    (from_listings(&listings), notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(path: &str, time: &str) -> Listed {
        Listed {
            path: path.into(),
            mod_time: time.into(),
            is_dir: false,
        }
    }
    #[test]
    fn earliest_listed_time_per_record_and_other_objects_ignored() {
        let id = "a".repeat(64);
        let other = "b".repeat(64);
        let times = from_listings(&[
            vec![
                item(&format!("events/{id}.json"), "2026-09-30T00:00:10Z"),
                item(&format!("events/{other}.json"), "2026-09-30T00:00:05.5Z"),
                item(&format!("nested/events/{id}.json"), "2020-01-01T00:00:00Z"),
                item(
                    &format!("checkpoints/heads/{id}.json"),
                    "2020-01-01T00:00:00Z",
                ),
                item(&format!("history/events/{id}.json"), "2020-01-01T00:00:00Z"),
                item("events/short.json", "2020-01-01T00:00:00Z"),
            ],
            vec![item(&format!("events/{id}.json"), "2026-09-30T00:00:01Z")],
        ]);
        assert_eq!(times.len(), 2);
        let base = 1_790_726_400; // 2026-09-30T00:00:00Z
        assert_eq!(times[&id], base + 1);
        assert_eq!(times[&other], base + 5);
    }
}
