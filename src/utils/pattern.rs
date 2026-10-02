//! Wildcard matching for user filters.
/// ASCII case-insensitive glob match of the whole `text`: `*` matches any run,
/// `?` any single byte. Used by `inventory::query`.
pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut checkpoint = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            checkpoint = ti;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            checkpoint += 1;
            ti = checkpoint;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}
