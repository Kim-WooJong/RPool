//! Which list columns fit, and how wide each one is.

/// Below this list width the Type column is hidden.
pub(crate) const TYPE_MIN_WIDTH: f32 = 700.0;
const SIZE_WIDTH: f32 = 96.0;
const TYPE_WIDTH: f32 = 84.0;
const MIN_NAME_WIDTH: f32 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Columns {
    pub(crate) name: f32,
    /// The containing folder of drive-wide search results.
    pub(crate) folder: Option<f32>,
    pub(crate) size: f32,
    pub(crate) kind: Option<f32>,
}

impl Columns {
    pub(crate) fn for_width(width: f32, with_folder: bool) -> Self {
        let kind = (width >= TYPE_MIN_WIDTH).then_some(TYPE_WIDTH);
        let rest = (width - SIZE_WIDTH - kind.unwrap_or(0.0)).max(MIN_NAME_WIDTH);
        let (name, folder) = if with_folder {
            let folder = (rest * 0.4).floor();
            (rest - folder, Some(folder))
        } else {
            (rest, None)
        };
        Self {
            name,
            folder,
            size: SIZE_WIDTH,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_column_collapses_on_narrow_lists() {
        let wide = Columns::for_width(1000.0, false);
        assert!(wide.kind.is_some() && wide.folder.is_none());
        assert_eq!(wide.name + wide.size + wide.kind.unwrap(), 1000.0);
        let narrow = Columns::for_width(TYPE_MIN_WIDTH - 1.0, false);
        assert!(narrow.kind.is_none());
        assert_eq!(narrow.name + narrow.size, TYPE_MIN_WIDTH - 1.0);
        let search = Columns::for_width(900.0, true);
        let folder = search.folder.unwrap();
        assert!(folder > 0.0 && search.name > folder);
        assert_eq!(
            search.name + folder + search.size + search.kind.unwrap(),
            900.0
        );
        // Never narrower than a usable name column.
        assert!(Columns::for_width(100.0, false).name >= MIN_NAME_WIDTH);
    }
}
