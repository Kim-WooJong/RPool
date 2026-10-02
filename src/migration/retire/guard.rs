//! Mass-delete guard (like rclone bisync `--max-delete`): refuse a cleanup
//! step that would remove more than a share of a pool's objects or bytes, or
//! more than a fixed number of objects at once, unless forced.
use super::model::RetireObject;
use crate::migration::enumerate::RemoteListing;
use crate::prelude::*;

/// Objects and bytes currently stored on the listed roots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    /// Object count.
    pub objects: u64,
    /// Sum of object sizes in bytes.
    pub bytes: u64,
}

/// Totals over every successfully listed root in `roots`.
pub(crate) fn totals<'a>(
    listings: &BTreeMap<String, RemoteListing>,
    roots: impl IntoIterator<Item = &'a String>,
) -> Totals {
    let mut out = Totals::default();
    for root in roots {
        if let Some(files) = listings.get(root).and_then(RemoteListing::files) {
            out.objects += files.len() as u64;
            out.bytes += files.values().sum::<u64>();
        }
    }
    out
}

/// Why deleting `objects` would be refused, or `None` when it is allowed.
pub(crate) fn check<'a>(
    objects: impl IntoIterator<Item = &'a RetireObject>,
    pool: Totals,
    max_percent: u32,
    max_objects: u64,
) -> Option<String> {
    let (mut count, mut bytes) = (0u64, 0u64);
    for object in objects {
        count += 1;
        bytes += object.size;
    }
    if count == 0 {
        return None;
    }
    if count > max_objects {
        return Some(format!(
            "{count} objects exceed the limit of {max_objects} per run"
        ));
    }
    let over = |part: u64, whole: u64| {
        u128::from(part) * 100 > u128::from(max_percent) * u128::from(whole)
    };
    if over(count, pool.objects) {
        return Some(format!(
            "{count} of the pool's {} objects is more than {max_percent}%",
            pool.objects
        ));
    }
    if over(bytes, pool.bytes) {
        return Some(format!(
            "{} of the pool's {} is more than {max_percent}%",
            crate::presentation::format_bytes(bytes),
            crate::presentation::format_bytes(pool.bytes)
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objects(n: usize, size: u64) -> Vec<RetireObject> {
        (0..n)
            .map(|i| RetireObject {
                address: format!("r:x/{i}"),
                size,
                root: "r:".into(),
            })
            .collect()
    }

    #[test]
    fn thresholds() {
        let pool = Totals {
            objects: 10,
            bytes: 1000,
        };
        assert_eq!(check(&objects(0, 1), pool, 50, 100), None);
        assert_eq!(
            check(&objects(5, 100), pool, 50, 100),
            None,
            "exactly 50% passes"
        );
        assert!(check(&objects(6, 1), pool, 50, 100)
            .unwrap()
            .contains("objects is more than 50%"));
        assert!(check(&objects(2, 400), pool, 50, 100)
            .unwrap()
            .contains("more than 50%"));
        assert!(check(&objects(3, 1), pool, 50, 2)
            .unwrap()
            .contains("limit of 2"));
        assert_eq!(check(&objects(10, 100), pool, 100, 100), None);
        // Unknown pool size: anything is too much.
        assert!(check(&objects(1, 1), Totals::default(), 50, 100).is_some());
        let listings = BTreeMap::from([
            (
                "a:".to_string(),
                RemoteListing::Listed(BTreeMap::from([("x".into(), 3), ("y".into(), 4)])),
            ),
            ("b:".to_string(), RemoteListing::Failed("down".into())),
        ]);
        let roots = ["a:".to_string(), "b:".to_string()];
        assert_eq!(
            totals(&listings, &roots),
            Totals {
                objects: 2,
                bytes: 7
            }
        );
    }
}
