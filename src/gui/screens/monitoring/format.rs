//! Text for rates, byte counts and durations on the Monitoring page.
use crate::gui::i18n::trf;

/// Bytes per second in decimal units with one decimal ("820 B/s",
/// "1.5 KB/s", "12.3 MB/s", "1.1 GB/s").
pub(crate) fn rate(bytes_per_second: f64) -> String {
    let value = if bytes_per_second.is_finite() {
        bytes_per_second.max(0.0)
    } else {
        0.0
    };
    if value < 1_000.0 {
        format!("{value:.0} B/s")
    } else if value < 1_000_000.0 {
        format!("{:.1} KB/s", value / 1_000.0)
    } else if value < 1_000_000_000.0 {
        format!("{:.1} MB/s", value / 1_000_000.0)
    } else {
        format!("{:.1} GB/s", value / 1_000_000_000.0)
    }
}

/// Byte counts in binary units (KiB, MiB, GiB).
pub(crate) fn bytes(value: u64) -> String {
    crate::presentation::format_bytes(value)
}

/// A running time such as "45s", "12m 5s", "3h 20m" or "2d 4h".
pub(crate) fn duration(seconds: u64) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
        seconds % 60,
    );
    if d > 0 {
        trf("{d}d {h}h", &[("d", &d), ("h", &h)])
    } else if h > 0 {
        trf("{h}h {m}m", &[("h", &h), ("m", &m)])
    } else if m > 0 {
        trf("{m}m {s}s", &[("m", &m), ("s", &s)])
    } else {
        trf("{s}s", &[("s", &s)])
    }
}
