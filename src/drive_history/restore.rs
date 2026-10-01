//! Planned drive changes. Every change publishes a NEW revision: `Put` makes
//! an old revision's bytes the content of a path, `Delete` deletes a visible
//! file (it goes to the trash). Nothing here destroys history.
use super::graph::collides;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub(crate) enum Action {
    Put {
        /// Namespace path written.
        path: String,
        /// Revision whose bytes become the new content.
        rev: String,
        /// Where those bytes were (trash restore), for reporting.
        from: Option<String>,
    },
    Delete {
        path: String,
        /// Revision visible at `path` when planned (must still be current).
        rev: String,
    },
}
impl Action {
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Put { path, .. } | Self::Delete { path, .. } => path,
        }
    }
    pub(crate) fn rev(&self) -> &str {
        match self {
            Self::Put { rev, .. } | Self::Delete { rev, .. } => rev,
        }
    }
}

/// Deletions first (deepest path first) so a file can replace a folder of
/// the same name and the reverse.
pub(crate) fn order(actions: &mut [Action]) {
    actions.sort_by_key(|a| match a {
        Action::Delete { path, .. } => (0, usize::MAX - path.matches('/').count(), path.clone()),
        Action::Put { path, .. } => (1, path.matches('/').count(), path.clone()),
    });
}

/// `dir/name (restored).ext`, then `(restored 2)`, … free among `taken`.
pub(crate) fn unique_name(path: &str, taken: &[String]) -> String {
    let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
    let (stem, ext) = name
        .rsplit_once('.')
        .filter(|(s, _)| !s.is_empty())
        .map_or((name.to_owned(), String::new()), |(s, e)| {
            (s.to_owned(), format!(".{e}"))
        });
    for n in 1.. {
        let label = if n == 1 {
            " (restored)".to_owned()
        } else {
            format!(" (restored {n})")
        };
        let candidate = format!("{stem}{label}{ext}");
        let candidate = if dir.is_empty() {
            candidate
        } else {
            format!("{dir}/{candidate}")
        };
        if !taken.iter().any(|p| collides(p, &candidate)) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_names_skip_taken_paths_and_keep_extensions() {
        let taken = vec!["Docs/a.txt".to_string(), "Docs/a (restored).txt".into()];
        assert_eq!(unique_name("Docs/a.txt", &taken), "Docs/a (restored 2).txt");
        assert_eq!(unique_name("README", &[]), "README (restored)");
        assert_eq!(unique_name(".env", &[]), ".env (restored)");
        let mut actions = vec![
            Action::Put {
                path: "x".into(),
                rev: "1".into(),
                from: None,
            },
            Action::Delete {
                path: "a".into(),
                rev: "2".into(),
            },
            Action::Delete {
                path: "x/y/z".into(),
                rev: "3".into(),
            },
        ];
        order(&mut actions);
        assert_eq!(
            actions.iter().map(Action::path).collect::<Vec<_>>(),
            ["x/y/z", "a", "x"]
        );
    }
}
