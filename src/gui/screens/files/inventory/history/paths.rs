//! Explorer paths (`Docs/a.txt`, root `""`) ↔ drive paths of the history
//! commands (`/Docs/a.txt`, root `/`).

/// `Docs/a.txt` → `/Docs/a.txt`; `""` → `/`.
pub(crate) fn to_drive(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

/// `/Docs/a.txt` → `Docs/a.txt`; `/` → `""`.
pub(crate) fn from_drive(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// `(folder, name)` of a drive path: `/Docs/a.txt` → `("/Docs", "a.txt")`,
/// `/a.txt` → `("/", "a.txt")`.
pub(crate) fn split(path: &str) -> (String, String) {
    let inner = from_drive(path);
    match inner.rsplit_once('/') {
        Some((folder, name)) => (to_drive(folder), name.to_string()),
        None => ("/".to_string(), inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_and_splits() {
        assert_eq!(to_drive(""), "/");
        assert_eq!(to_drive("Docs/a.txt"), "/Docs/a.txt");
        assert_eq!(from_drive("/"), "");
        assert_eq!(from_drive("//Docs//a.txt"), "Docs/a.txt");
        assert_eq!(split("/Docs/x/a.txt"), ("/Docs/x".into(), "a.txt".into()));
        assert_eq!(split("/a.txt"), ("/".into(), "a.txt".into()));
    }
}
