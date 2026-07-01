//! Metadata extraction helpers for the Gallery view.

use std::{fs, path::Path};

use atlas_fs::EntryKind;
use image::ImageReader;

use crate::views::details::{format_relative_time, format_size};

/// Rich metadata shown in the Gallery sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// File name.
    pub name: String,
    /// Full path displayed to the user.
    pub path: String,
    /// Human-readable file size.
    pub size_text: String,
    /// Human-readable modified time.
    pub modified_text: String,
    /// Entry kind label.
    pub kind: String,
    /// Image dimensions when the file can be probed as an image.
    pub dimensions: Option<(u32, u32)>,
}

/// Extract Gallery metadata for `path`.
#[must_use]
pub fn extract(path: &Path, meta: &fs::Metadata, kind: EntryKind) -> Metadata {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let size_text = if meta.is_dir() {
        "—".to_owned()
    } else {
        format_size(meta.len())
    };
    let modified_text = meta
        .modified()
        .ok()
        .map(format_relative_time)
        .unwrap_or_else(|| "—".to_owned());

    Metadata {
        name,
        path: path.to_string_lossy().into_owned(),
        size_text,
        modified_text,
        kind: kind_label(&kind).to_owned(),
        dimensions: probe_dimensions(path, &kind),
    }
}

fn kind_label(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "File",
        EntryKind::Dir => "Directory",
        EntryKind::Symlink { .. } => "Symlink",
        EntryKind::Other => "Other",
    }
}

fn probe_dimensions(path: &Path, kind: &EntryKind) -> Option<(u32, u32)> {
    if matches!(kind, EntryKind::Dir) {
        return None;
    }

    ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;
    use tempfile::TempDir;

    #[test]
    fn extract_png_reports_dimensions() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("image.png");
        RgbaImage::new(32, 24).save(&path).expect("save png");
        let meta = fs::metadata(&path).expect("metadata");

        let extracted = extract(&path, &meta, EntryKind::File);
        assert_eq!(extracted.dimensions, Some((32, 24)));
    }

    #[test]
    fn extract_text_has_no_dimensions() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("notes.txt");
        fs::write(&path, b"hello").expect("write text");
        let meta = fs::metadata(&path).expect("metadata");

        let extracted = extract(&path, &meta, EntryKind::File);
        assert_eq!(extracted.dimensions, None);
    }
}
