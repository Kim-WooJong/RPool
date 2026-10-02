//! Which trash list columns fit, and how wide each one is.

/// Width of the check box column, in points.
pub(crate) const CHECK: f32 = 28.0;
/// Width of the Size column, in points.
const SIZE: f32 = 100.0;
/// Width of the "Leaves the trash" column (shown from 520 pt).
const EXPIRES: f32 = 128.0;
/// Width of the Deleted column (shown from 720 pt).
const DELETED: f32 = 170.0;
/// Lower bound of the Name column, in points.
const MIN_NAME: f32 = 120.0;

/// Column widths (points) of the trash list for one frame, from `for_width`.
/// Used by `trash_list` for the header and rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TrashColumns {
    /// Name column: the width left after the other columns.
    pub(crate) name: f32,
    /// Original folder (wide lists only).
    pub(crate) folder: Option<f32>,
    /// Size column (always shown).
    pub(crate) size: f32,
    /// Deleted-when/by column; `None` below 720 pt.
    pub(crate) deleted: Option<f32>,
    /// Expiry column; `None` below 520 pt.
    pub(crate) expires: Option<f32>,
}

impl TrashColumns {
    /// Splits `width` into columns, dropping Expires, Deleted and Folder as the
    /// list gets narrower (Folder only from 900 pt, taking 38% of the free width).
    pub(crate) fn for_width(width: f32) -> Self {
        let expires = (width >= 520.0).then_some(EXPIRES);
        let deleted = (width >= 720.0).then_some(DELETED);
        let fixed = CHECK + SIZE + expires.unwrap_or(0.0) + deleted.unwrap_or(0.0);
        let rest = (width - fixed).max(MIN_NAME);
        let (name, folder) = if width >= 900.0 {
            let folder = (rest * 0.38).floor();
            (rest - folder, Some(folder))
        } else {
            (rest, None)
        };
        Self {
            name,
            folder,
            size: SIZE,
            deleted,
            expires,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_collapse_and_fill_the_width() {
        let wide = TrashColumns::for_width(1200.0);
        assert!(wide.folder.is_some() && wide.deleted.is_some() && wide.expires.is_some());
        let sum = CHECK
            + wide.name
            + wide.folder.unwrap()
            + wide.size
            + wide.deleted.unwrap()
            + wide.expires.unwrap();
        assert_eq!(sum, 1200.0);
        let medium = TrashColumns::for_width(800.0);
        assert!(medium.folder.is_none() && medium.deleted.is_some());
        let narrow = TrashColumns::for_width(500.0);
        assert!(narrow.deleted.is_none() && narrow.expires.is_none());
        assert_eq!(CHECK + narrow.name + narrow.size, 500.0);
        assert!(TrashColumns::for_width(100.0).name >= MIN_NAME);
    }
}
