//! Thumbnail generation: decode, resize, and encode.

use std::io::Cursor;
use std::path::Path;

use image::{imageops, DynamicImage, ImageFormat};

use crate::cache::{CachedThumb, ThumbFormat};
use crate::decode::decode_to_rgba;
use crate::error::{Result, ThumbError};

/// Generates a thumbnail for the image at `path`.
///
/// Decodes the source image, resizes it so the longer side equals `target_dim`
/// without upscaling, and encodes it in the requested `format`.
pub fn generate_thumbnail(
    path: &Path,
    target_dim: u32,
    format: ThumbFormat,
) -> Result<CachedThumb> {
    let img = decode_to_rgba(path)?;
    let (orig_w, orig_h) = img.dimensions();

    let longer = orig_w.max(orig_h) as f32;
    let scale = (target_dim as f32 / longer).min(1.0);
    let new_w = ((orig_w as f32 * scale).round() as u32).max(1);
    let new_h = ((orig_h as f32 * scale).round() as u32).max(1);

    let resized = imageops::resize(&img, new_w, new_h, imageops::FilterType::Triangle);
    let image_format = match format {
        ThumbFormat::Webp => ImageFormat::WebP,
        ThumbFormat::Png => ImageFormat::Png,
    };

    let mut buf = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(resized)
        .write_to(&mut buf, image_format)
        .map_err(|error| ThumbError::Encode(error.to_string()))?;

    Ok(CachedThumb {
        format,
        width: new_w,
        height: new_h,
        bytes: buf.into_inner().into(),
    })
}
