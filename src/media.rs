//! Image helpers (same sizes/qualities as the Swift MediaHelper usage).
use image::{codecs::jpeg::JpegEncoder, DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use std::io::Cursor;

pub struct PreparedImage {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub thumbnail: Option<Vec<u8>>,
}

pub fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation { img.apply_orientation(o); }
    Ok(img)
}

pub fn resize_max(img: &DynamicImage, max: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max || w == 0 || h == 0 { return img.clone(); }
    let scale = max as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).floor() as u32).max(1);
    let nh = ((h as f64 * scale).floor() as u32).max(1);
    img.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
}

/// Flatten alpha onto white (JPEG has no alpha; screenshots often carry one).
fn to_rgb(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() { return img.to_rgb8(); }
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])]));
    }
    out
}

pub fn jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let rgb = to_rgb(img);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Photo for sending: max 2048 px, JPEG 0.75, thumbnail 120 px JPEG 0.5 (inline).
pub fn prepare_photo(img: &DynamicImage) -> Result<PreparedImage, String> {
    let resized = resize_max(img, 2048);
    let jpeg_bytes = jpeg(&resized, 75)?;
    let thumb = jpeg(&resize_max(&resized, 120), 50).ok();
    Ok(PreparedImage { jpeg: jpeg_bytes, width: resized.width(), height: resized.height(), thumbnail: thumb })
}

pub fn prepare_photo_bytes(bytes: &[u8]) -> Result<PreparedImage, String> {
    prepare_photo(&decode(bytes)?)
}

pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<DynamicImage> {
    RgbaImage::from_raw(width, height, rgba).map(DynamicImage::ImageRgba8)
}

/// Avatar: 256 px JPEG 0.75.
pub fn prepare_avatar(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = decode(bytes)?;
    // centre-crop to a square first so round avatars look right everywhere
    let side = img.width().min(img.height());
    let x = (img.width() - side) / 2;
    let y = (img.height() - side) / 2;
    let sq = img.crop_imm(x, y, side, side);
    jpeg(&resize_max(&sq, 256), 75)
}

pub fn mime_for(name: &str) -> String {
    mime_guess::from_path(name).first().map(|m| m.essence_str().to_string()).unwrap_or_else(|| "application/octet-stream".into())
}

pub fn is_image_name(name: &str) -> bool {
    let l = name.to_lowercase();
    [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".tif", ".tiff", ".heic_unsupported"].iter().any(|e| l.ends_with(e))
}
