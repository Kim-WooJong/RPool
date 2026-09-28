use crate::utils::now_unix;

pub(crate) fn relative_age(timestamp: u64) -> String {
    let elapsed = now_unix().saturating_sub(timestamp);
    match elapsed {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}
