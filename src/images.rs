//! Bounded raster attachments for tools and user-selected files.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

pub const SOURCE_BYTES: usize = 20 * 1024 * 1024;
pub const INLINE_BYTES: usize = 128 * 1024;
pub const MAX_IMAGES: usize = 4;

pub fn jailed_file(path: &str) -> Result<Option<Image>, String> {
    let head = |limit: usize| {
        crate::running::binary(
            "head",
            &["-c".into(), limit.to_string(), "--".into(), path.into()],
            limit,
        )
    };
    let header = head(32)?;
    if image::guess_format(&header).is_err() {
        return Ok(None);
    }
    let bytes = head(SOURCE_BYTES + 1)?;
    process(&bytes).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub data: String,
    pub media_type: String,
}

pub fn encoded(data: &str, media_type: &str) -> Result<Image, String> {
    if data.len() > SOURCE_BYTES.div_ceil(3) * 4 {
        return Err("image exceeds the 20 MiB source limit".into());
    }
    if !matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp"
    ) {
        return Err(format!("unsupported image media type: {media_type}"));
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|e| format!("invalid image base64: {e}"))?;
    process(&bytes)
}

fn process(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() > SOURCE_BYTES {
        return Err("image exceeds the 20 MiB source limit".into());
    }
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    if format == ImageFormat::Bmp {
        return Err("BMP images are not read: convert to PNG, JPEG, GIF or WebP".into());
    }
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err("unsupported raster image format".into());
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let original = reader
        .decode()
        .map_err(|e| format!("invalid or oversized image: {e}"))?;
    let alpha = original.color().has_alpha();
    let mut edge = 1536;
    loop {
        let resized = if original.width() <= edge && original.height() <= edge {
            original.clone()
        } else {
            original.thumbnail(edge, edge)
        };
        let mut out = Cursor::new(Vec::new());
        if alpha {
            resized
                .write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
                .map_err(|e| e.to_string())?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
                .encode_image(&resized)
                .map_err(|e| e.to_string())?;
        }
        let bytes = out.into_inner();
        if bytes.len() <= INLINE_BYTES {
            return Ok(Image {
                data: STANDARD.encode(bytes),
                media_type: if alpha { "image/png" } else { "image/jpeg" }.into(),
            });
        }
        if edge <= 64 {
            return Err("image cannot fit the inline byte limit".into());
        }
        edge /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_images_are_not_upscaled() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(16, 8)
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("png");
        let inline = process(bytes.get_ref()).expect("inline");
        let decoded = image::load_from_memory(&STANDARD.decode(inline.data).expect("base64"))
            .expect("pixels");
        assert_eq!((decoded.width(), decoded.height()), (16, 8));
    }

    #[test]
    fn raster_is_resized_and_validated() {
        let raster = image::DynamicImage::new_rgba8(2048, 1024);
        let mut bytes = Cursor::new(Vec::new());
        raster.write_to(&mut bytes, ImageFormat::Png).expect("png");
        let result = process(bytes.get_ref()).expect("image");
        let decoded = STANDARD.decode(&result.data).expect("base64");
        assert!(decoded.len() <= INLINE_BYTES);
        let decoded = image::load_from_memory(&decoded).expect("raster");
        assert_eq!((decoded.width(), decoded.height()), (1536, 768));
        assert!(encoded("not base64!", "image/png").is_err());
        assert!(encoded("", "image/svg+xml").is_err());
        assert!(process(b"not an image").is_err());
    }

    #[test]
    fn a_bmp_is_refused_by_name() {
        let header = b"BM\x3a\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0\x01\0\0\0\x01\0\0\0\x01\0\x18\0";
        let refused = process(header).expect_err("a BMP");
        assert!(refused.contains("BMP images are not read"), "{refused}");
        let data = STANDARD.encode(header);
        let named = encoded(&data, "image/bmp").expect_err("a BMP media type");
        assert!(named.contains("image/bmp"), "{named}");
        assert!(
            encoded(&data, "image/png")
                .expect_err("BMP bytes")
                .contains("BMP")
        );
    }
}
