//! Column definitions and defaults for the Details view.

use atlas_fs::SortOrder;

/// The kind of data a Details column displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// File / directory name with icon.
    Name,
    /// File size in human-readable binary units.
    Size,
    /// Last-modified timestamp, relative.
    Modified,
    /// Entry kind (File / Directory / Symlink).
    Kind,
    /// File extension.
    Extension,
}

impl ColumnKind {
    /// The wire string that matches the Slint `ColumnSpec.kind` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Kind => "kind",
            Self::Extension => "extension",
        }
    }
}

/// A Rust-side column descriptor that mirrors the Slint `ColumnSpec` struct.
#[derive(Debug, Clone)]
pub struct ColumnSpec {
    /// Column kind — determines what data is rendered.
    pub kind: ColumnKind,
    /// Display title in the header.
    pub title: String,
    /// Column width in logical pixels.
    pub width_px: f32,
    /// Active sort order, if this column is the sort key.
    pub sort: Option<SortOrder>,
    /// Whether the cell content is right-aligned (e.g., file size).
    pub align_right: bool,
}

impl ColumnSpec {
    /// Convert to the Slint-generated `ColumnSpec` struct.
    #[must_use]
    pub fn to_slint(&self) -> crate::ColumnSpec {
        let sort_dir = match self.sort {
            None => 0,
            Some(SortOrder::Asc) => 1,
            Some(SortOrder::Desc) => -1,
        };
        crate::ColumnSpec {
            title: self.title.as_str().into(),
            width: self.width_px,
            kind: self.kind.as_str().into(),
            sort_dir,
            align_right: self.align_right,
        }
    }
}

/// Return the default column layout for the Details view.
#[must_use]
pub fn default_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec {
            kind: ColumnKind::Name,
            title: "Name".to_owned(),
            width_px: 280.0,
            sort: Some(SortOrder::Asc),
            align_right: false,
        },
        ColumnSpec {
            kind: ColumnKind::Size,
            title: "Size".to_owned(),
            width_px: 90.0,
            sort: None,
            align_right: true,
        },
        ColumnSpec {
            kind: ColumnKind::Modified,
            title: "Modified".to_owned(),
            width_px: 150.0,
            sort: None,
            align_right: false,
        },
        ColumnSpec {
            kind: ColumnKind::Kind,
            title: "Kind".to_owned(),
            width_px: 90.0,
            sort: None,
            align_right: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_columns_has_name_first() {
        let cols = default_columns();
        assert!(!cols.is_empty());
        assert_eq!(cols[0].kind, ColumnKind::Name);
    }

    #[test]
    fn kind_as_str_roundtrip() {
        assert_eq!(ColumnKind::Name.as_str(), "name");
        assert_eq!(ColumnKind::Size.as_str(), "size");
        assert_eq!(ColumnKind::Modified.as_str(), "modified");
        assert_eq!(ColumnKind::Kind.as_str(), "kind");
        assert_eq!(ColumnKind::Extension.as_str(), "extension");
    }
}
