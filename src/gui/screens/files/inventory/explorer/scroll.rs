//! Keeping a moved selection inside the list's scroll viewport.

use super::state::ScrollMemo;

/// The scroll offset that shows row `index` (rows `row` high), moving as
/// little as possible from the last offset.
pub(crate) fn reveal_offset(index: usize, row: f32, last: ScrollMemo) -> f32 {
    let top = index as f32 * row;
    let bottom = top + row;
    if top < last.offset {
        top
    } else if bottom > last.offset + last.view && last.view > 0.0 {
        bottom - last.view
    } else {
        last.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolls_only_when_the_row_is_outside_the_view() {
        let last = ScrollMemo {
            offset: 100.0,
            view: 200.0,
        };
        assert_eq!(reveal_offset(5, 20.0, last), 100.0);
        assert_eq!(reveal_offset(2, 20.0, last), 40.0);
        assert_eq!(reveal_offset(20, 20.0, last), 220.0);
    }
}
