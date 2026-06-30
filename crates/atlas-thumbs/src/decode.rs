//! Image decoding for raster and SVG formats.

use std::path::Path;

use image::RgbaImage;

use crate::error::{Result, ThumbError};

/// Returns the file extensions that this crate can thumbnail.
#[must_use]
pub fn decode_thumbnailable_extensions() -> &'static [&'static str] {
    &[
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif", "svg",
    ]
}

/// Returns `true` if atlas-thumbs can generate a thumbnail for `path`.
#[must_use]
pub fn can_thumbnail(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    let lower = ext.to_lowercase();
    decode_thumbnailable_extensions()
        .iter()
        .any(|&supported| supported == lower)
}

/// Decodes the image at `path` into an RGBA8 image buffer.
///
/// Supports JPEG, PNG, GIF, WebP, BMP, TIFF, and SVG.
/// SVG is rendered at its intrinsic size.
pub fn decode_to_rgba(path: &Path) -> Result<RgbaImage> {
    let ext = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_lowercase);

    match ext.as_deref() {
        Some("svg") => decode_svg(path),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif") => {
            decode_raster(path)
        }
        other => Err(ThumbError::UnsupportedFormat(other.map(String::from))),
    }
}

fn decode_raster(path: &Path) -> Result<RgbaImage> {
    let img = image::ImageReader::open(path)?
        .decode()
        .map_err(|error| ThumbError::Decode(error.to_string()))?;
    Ok(img.into_rgba8())
}

fn decode_svg(path: &Path) -> Result<RgbaImage> {
    let data = std::fs::read(path)?;
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &options)
        .map_err(|error| ThumbError::Decode(error.to_string()))?;

    let size = tree.size();
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;

    if width == 0 || height == 0 {
        return Err(ThumbError::Decode("SVG has zero dimensions".into()));
    }

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| ThumbError::Decode("failed to allocate SVG pixmap".into()))?;
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap_mut);

    let raw = pixmap.data();
    let mut straight = Vec::with_capacity(raw.len());
    for chunk in raw.chunks_exact(4) {
        let (red, green, blue, alpha) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        if alpha == 0 {
            straight.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let scale = 255.0 / alpha as f32;
            straight.push((red as f32 * scale).min(255.0) as u8);
            straight.push((green as f32 * scale).min(255.0) as u8);
            straight.push((blue as f32 * scale).min(255.0) as u8);
            straight.push(alpha);
        }
    }

    RgbaImage::from_raw(width, height, straight)
        .ok_or_else(|| ThumbError::Decode("SVG pixmap to RgbaImage failed".into()))
}
