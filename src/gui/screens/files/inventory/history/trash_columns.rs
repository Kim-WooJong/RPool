//! Which trash list columns fit, and how wide each one is.

pub(crate) const CHECK: f32 = 28.0;
const SIZE: f32 = 100.0;
const EXPIRES: f32 = 128.0;
const DELETED: f32 = 170.0;
const MIN_NAME: f32 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TrashColumns {
    pub(crate) name: f32,
    /// Original folder (wide lists only).
    pub(crate) folder: Option<f32>,
    pub(crate) size: f32,
    pub(crate) deleted: Option<f32>,
    pub(crate) expires: Option<f32>,
}

impl TrashColumns {
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
