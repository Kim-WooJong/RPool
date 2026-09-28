#[derive(Debug, Clone)]
pub(crate) enum Probe {
    Ok,
    Missing,
    BadSize { found: u64, expected: u64 },
    Corrupt { found: String, expected: String },
    Error(String),
}

impl Probe {
    pub(crate) fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}
