//! Image probing and JPEG thumbnails for outgoing photos, MIME sniffing.

use std::io::Cursor;
use std::path::Path;

use crate::error::{Error, Result};
use crate::net::types::{OutgoingKind, OutgoingMedia};

/// Longest side of the inline thumbnail sent with a photo.
pub const THUMB_SIDE: u32 = 96;

fn media_err(e: impl std::fmt::Display) -> Error {
    Error::Media(e.to_string())
}

/// Reads only the header and rejects images whose decoded size would exceed
/// [`crate::media::MAX_IMAGE_PIXELS`] (decompression bombs).
pub fn check_image_bounds(bytes: &[u8]) -> Result<(u32, u32)> {
    check_image_bounds_max(bytes, crate::media::MAX_IMAGE_PIXELS)
}

/// [`check_image_bounds`] with an explicit pixel cap.
pub fn check_image_bounds_max(bytes: &[u8], max_pixels: u64) -> Result<(u32, u32)> {
    let (w, h) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(media_err)?
        .into_dimensions()
        .map_err(media_err)?;
    if w as u64 * h as u64 > max_pixels {
        return Err(Error::Media(format!("image too large ({w}x{h})")));
    }
    Ok((w, h))
}

/// Decodes with allocation limits.
pub fn decode_limited(bytes: &[u8]) -> Result<image::DynamicImage> {
    check_image_bounds(bytes)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(media_err)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(crate::media::MAX_IMAGE_PIXELS * 4 + 16 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(media_err)
}

/// Largest sticker decoded (pixels). Real stickers are 512x512.
pub const MAX_STICKER_PIXELS: u64 = 1_000_000;

/// Decodes a WebP sticker to RGBA. Animated stickers yield their first
/// frame. Never panics; oversize or corrupt input is an error.
pub fn decode_sticker(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    check_image_bounds_max(bytes, MAX_STICKER_PIXELS)?;
    let img = decode_limited(bytes)?;
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Decodes `bytes`, returns `(width, height, jpeg_thumbnail)`.
pub fn probe_and_thumbnail(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = decode_limited(bytes)?;
    let (w, h) = (img.width(), img.height());
    let thumb = img.thumbnail(THUMB_SIDE, THUMB_SIDE).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), 70)
        .encode_image(&thumb)
        .map_err(|e| Error::Media(e.to_string()))?;
    Ok((w, h, out))
}

/// Longest side of a video poster frame kept in the cache.
pub const POSTER_SIDE: u32 = 480;

/// A decoded video frame as the bubble's poster: scaled to fit
/// [`POSTER_SIDE`] and JPEG-encoded (a few tens of kB, stored like an
/// inline thumbnail so it is extracted once).
pub fn poster_jpeg(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| Error::Media("frame size does not match its pixels".into()))?;
    let img = image::DynamicImage::ImageRgba8(img)
        .thumbnail(POSTER_SIDE, POSTER_SIDE)
        .to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), 80)
        .encode_image(&img)
        .map_err(|e| Error::Media(e.to_string()))?;
    Ok(out)
}

/// MIME type from the file extension.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "ogg" | "opus" => "audio/ogg; codecs=opus",
        "mp3" => "audio/mpeg",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        _ => "application/octet-stream",
    }
}

/// Turns a picked file into an [`OutgoingMedia`]: photos (JPEG/PNG/WebP that
/// decode) become images, everything else a document.
pub fn outgoing_from_file(path: &Path, bytes: Vec<u8>, caption: Option<String>) -> OutgoingMedia {
    let mimetype = mime_for_path(path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    // GIFs are sent as videos (see `app::paste`); a still copy would lose
    // the animation.
    if mimetype.starts_with("image/")
        && mimetype != "image/gif"
        && let Ok((width, height, thumbnail)) = probe_and_thumbnail(&bytes)
    {
        // WhatsApp only renders JPEG photos reliably; others go as-is.
        return OutgoingMedia {
            kind: OutgoingKind::Image {
                width,
                height,
                thumbnail,
            },
            bytes,
            file_name,
            mimetype: mimetype.into(),
            caption,
        };
    }
    OutgoingMedia {
        kind: OutgoingKind::Document,
        bytes,
        file_name,
        mimetype: mimetype.into(),
        caption,
    }
}

/// The inline JPEG thumbnail (as sent with photos) of raw RGBA pixels:
/// used for the first frame of a video.
pub fn thumbnail_from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| Error::Media("frame size does not match its pixels".into()))?;
    let thumb = image::DynamicImage::ImageRgba8(img)
        .thumbnail(THUMB_SIDE, THUMB_SIDE)
        .to_rgb8();
    encode_jpeg(&thumb, 70)
}

/// Longest side of a pasted photo: WhatsApp's standard photo quality.
pub const MAX_PHOTO_SIDE: u32 = 1600;
/// JPEG quality of pasted photos.
const PHOTO_QUALITY: u8 = 82;
/// Longest side of the attachment preview tile.
pub const PREVIEW_SIDE: u32 = 960;

fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), quality)
        .encode_image(img)
        .map_err(media_err)?;
    Ok(out)
}

/// Turns raw RGBA pixels (a copied image) into a JPEG photo the way WhatsApp
/// sends photos: transparency flattened onto white, the longest side capped
/// at [`MAX_PHOTO_SIDE`], plus the inline thumbnail. Oversized or short
/// buffers are errors, never panics.
pub fn outgoing_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<OutgoingMedia> {
    let w = u32::try_from(width).map_err(media_err)?;
    let h = u32::try_from(height).map_err(media_err)?;
    if w == 0 || h == 0 {
        return Err(Error::Media("empty image".into()));
    }
    if u64::from(w) * u64::from(h) > crate::media::MAX_IMAGE_PIXELS {
        return Err(Error::Media(format!("image too large ({w}x{h})")));
    }
    let len = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| Error::Media("image too large".into()))?;
    let Some(px) = rgba.get(..len) else {
        return Err(Error::Media("truncated image data".into()));
    };
    let blend = |c: u8, a: u8| -> u8 {
        let (c, a) = (u16::from(c), u16::from(a));
        ((c * a + 255 * (255 - a) + 127) / 255) as u8
    };
    let rgb: Vec<u8> = px
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [blend(r, a), blend(g, a), blend(b, a)])
        .collect();
    let mut img = image::RgbImage::from_raw(w, h, rgb)
        .ok_or_else(|| Error::Media("bad image buffer".into()))?;
    if w.max(h) > MAX_PHOTO_SIDE {
        img = image::DynamicImage::ImageRgb8(img)
            .resize(
                MAX_PHOTO_SIDE,
                MAX_PHOTO_SIDE,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8();
    }
    let thumb = image::DynamicImage::ImageRgb8(img.clone())
        .thumbnail(THUMB_SIDE, THUMB_SIDE)
        .to_rgb8();
    Ok(OutgoingMedia {
        kind: OutgoingKind::Image {
            width: img.width(),
            height: img.height(),
            thumbnail: encode_jpeg(&thumb, 70)?,
        },
        bytes: encode_jpeg(&img, PHOTO_QUALITY)?,
        file_name: "image.jpg".into(),
        mimetype: "image/jpeg".into(),
        caption: None,
    })
}

/// Downscaled RGBA pixels `(width, height, rgba)` for the attachment
/// preview of a photo; `None` for documents or undecodable data.
pub fn preview_rgba(media: &OutgoingMedia) -> Option<(u32, u32, Vec<u8>)> {
    if !matches!(media.kind, OutgoingKind::Image { .. }) {
        return None;
    }
    let img = decode_limited(&media.bytes).ok()?;
    let img = if img.width().max(img.height()) > PREVIEW_SIDE {
        img.thumbnail(PREVIEW_SIDE, PREVIEW_SIDE)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Human-readable size (`1.2 MB`, `340 kB`).
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1000.0;
    let b = bytes as f64;
    if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} kB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder as _;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 128]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn thumbnails_keep_aspect() {
        let (w, h, thumb) = probe_and_thumbnail(&png(400, 200)).unwrap();
        assert_eq!((w, h), (400, 200));
        let t = image::load_from_memory(&thumb).unwrap();
        assert_eq!(t.width(), THUMB_SIDE);
        assert_eq!(t.height(), THUMB_SIDE / 2);
    }

    #[test]
    fn picks_kind_from_file() {
        let img = outgoing_from_file(Path::new("/x/Cat.PNG"), png(10, 10), None);
        assert!(matches!(img.kind, OutgoingKind::Image { width: 10, .. }));
        let doc = outgoing_from_file(Path::new("/x/report.pdf"), b"%PDF-1.4".to_vec(), None);
        assert_eq!(doc.kind, OutgoingKind::Document);
        assert_eq!(doc.mimetype, "application/pdf");
        assert_eq!(doc.file_name, "report.pdf");
        // A broken "png" falls back to a document instead of failing.
        let bad = outgoing_from_file(Path::new("/x/b.png"), b"nope".to_vec(), None);
        assert_eq!(bad.kind, OutgoingKind::Document);
        // A GIF that decodes is still not a photo (it is sent as a video).
        let mut gif = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut Cursor::new(&mut gif), image::ImageFormat::Gif)
            .unwrap();
        let g = outgoing_from_file(Path::new("/x/funny.gif"), gif, None);
        assert_eq!(g.kind, OutgoingKind::Document);
        assert_eq!(g.mimetype, "image/gif");
        assert_eq!(mime_for_path(Path::new("/x/clip.MP4")), "video/mp4");
    }

    #[test]
    fn video_frames_get_a_small_jpeg_thumbnail() {
        let rgba = vec![200; 640 * 360 * 4];
        let t = thumbnail_from_rgba(640, 360, rgba).unwrap();
        let img = image::load_from_memory(&t).unwrap();
        assert_eq!(img.width(), THUMB_SIDE);
        assert!(thumbnail_from_rgba(10, 10, vec![0; 3]).is_err());
    }

    #[test]
    fn rejects_decompression_bombs() {
        // A valid PNG header claiming 20000x20000 pixels.
        let mut out = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut out);
        use image::ImageEncoder;
        // Encode a tiny image, then patch the IHDR dimensions.
        enc.write_image(&[0u8; 3], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        out[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        out[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        assert!(check_image_bounds(&out).is_err());
        assert!(check_image_bounds(&png(10, 10)).is_ok());
    }

    #[test]
    fn pasted_pixels_become_a_jpeg_photo() {
        // 2x1: opaque red, fully transparent (flattened to white).
        let px = [255, 0, 0, 255, 0, 0, 0, 0];
        let m = outgoing_from_rgba(2, 1, &px).unwrap();
        assert_eq!(m.mimetype, "image/jpeg");
        assert!(m.bytes.starts_with(&[0xFF, 0xD8]), "JPEG magic");
        let OutgoingKind::Image {
            width,
            height,
            thumbnail,
        } = &m.kind
        else {
            panic!("not an image: {:?}", m.kind);
        };
        assert_eq!((*width, *height), (2, 1));
        assert!(thumbnail.starts_with(&[0xFF, 0xD8]));
        let back = image::load_from_memory(&m.bytes).unwrap().to_rgb8();
        let white = back.get_pixel(1, 0);
        assert!(white.0.iter().all(|&c| c > 230), "transparent -> white");
        let (pw, ph, rgba) = preview_rgba(&m).unwrap();
        assert_eq!((pw, ph, rgba.len()), (2, 1, 8));
    }

    #[test]
    fn pasted_pixels_are_scaled_and_bounded() {
        let (w, h) = (3200usize, 1000usize);
        let px = vec![200u8; w * h * 4];
        let m = outgoing_from_rgba(w, h, &px).unwrap();
        assert!(matches!(
            m.kind,
            OutgoingKind::Image {
                width: MAX_PHOTO_SIDE,
                height: 500,
                ..
            }
        ));
        let (pw, ph, _) = preview_rgba(&m).unwrap();
        assert_eq!((pw, ph), (PREVIEW_SIDE, 300));
        assert!(outgoing_from_rgba(0, 10, &px).is_err(), "empty");
        assert!(outgoing_from_rgba(4, 4, &px[..10]).is_err(), "short buffer");
        assert!(
            outgoing_from_rgba(100_000, 100_000, &px).is_err(),
            "pixel cap"
        );
        assert!(outgoing_from_rgba(usize::MAX, 2, &px).is_err(), "overflow");
        let doc = outgoing_from_file(Path::new("/x/a.pdf"), b"%PDF".to_vec(), None);
        assert!(preview_rgba(&doc).is_none());
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(34_000), "34 kB");
        assert_eq!(human_size(1_260_000), "1.3 MB");
    }

    #[test]
    fn stickers_decode_to_rgba_with_transparency() {
        let (w, h, px) = decode_sticker(&crate::mock::sticker_webp()).unwrap();
        assert_eq!((w, h), (256, 256));
        assert_eq!(px.len(), 256 * 256 * 4);
        assert_eq!(px[3], 0, "outside the circle is transparent");
        assert!(decode_sticker(b"RIFF....WEBPjunk").is_err());
        assert!(decode_sticker(&[]).is_err());
    }

    #[test]
    fn oversized_stickers_are_refused_before_decoding() {
        let img = image::RgbaImage::new(1200, 1200);
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), 1200, 1200, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(decode_sticker(&out).is_err());
    }
}
