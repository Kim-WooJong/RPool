//! How many provider cards fit side by side.

/// Narrowest useful provider card.
pub(crate) const CARD_MIN_WIDTH: f32 = 300.0;
/// Upper bound of card columns, even on very wide windows.
pub(crate) const MAX_COLUMNS: usize = 4;
/// Space between cards, horizontally and vertically, in points.
pub(crate) const GAP: f32 = 12.0;

/// Columns for `width`: as many `CARD_MIN_WIDTH` cards (with gaps) as fit,
/// at least one and at most `MAX_COLUMNS`, never more than `cards`.
pub(crate) fn columns(width: f32, cards: usize) -> usize {
    let fit = ((width + GAP) / (CARD_MIN_WIDTH + GAP)).floor() as usize;
    fit.clamp(1, MAX_COLUMNS).min(cards.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_count_follows_the_width() {
        assert_eq!(columns(200.0, 9), 1);
        assert_eq!(columns(611.0, 9), 1);
        assert_eq!(columns(612.0, 9), 2);
        assert_eq!(columns(936.0, 9), 3);
        assert_eq!(columns(1248.0, 9), 4);
        assert_eq!(columns(3000.0, 9), MAX_COLUMNS);
        // Few providers do not leave empty columns.
        assert_eq!(columns(3000.0, 2), 2);
        assert_eq!(columns(3000.0, 0), 1);
    }
}
