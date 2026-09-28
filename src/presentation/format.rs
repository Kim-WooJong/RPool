pub(crate) fn usage_bar(percent: Option<f64>) -> String {
    let Some(percent) = percent else {
        return "[??????????]".to_string();
    };
    let filled = ((percent.clamp(0.0, 100.0) / 10.0).round() as usize).min(10);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(10 - filled))
}

pub(crate) fn format_optional_bytes(value: Option<u64>) -> String {
    value.map(format_bytes).unwrap_or_else(|| "n/a".to_string())
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
