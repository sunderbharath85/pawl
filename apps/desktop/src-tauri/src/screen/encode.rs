//! Turning a capture into a chat picture: long side at most 2048 px, JPEG at quality 85 unless PNG is smaller, at
//! most 4 MiB (docs/CONTRACTS.md "v0.2 Screen and Browser": `ChatImage`). Also the picker's small PNG thumbnails.
//! Everything stays in memory.

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, RgbImage, RgbaImage};

/// The long side of a picture sent to a model.
pub const MAX_SIDE: u32 = 2048;
/// The most a picture may take once decoded from base64 (the protocol's limit).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub const JPEG_QUALITY: u8 = 85;
/// The long side of a picker thumbnail.
pub const THUMB_SIDE: u32 = 240;
/// Lower JPEG qualities to try when a picture is over the byte limit (before making it smaller).
const FALLBACK_QUALITIES: [u8; 3] = [75, 65, 50];
/// Each further try is this much smaller (3/4 per side).
const SHRINK_NUM: u32 = 3;
const SHRINK_DEN: u32 = 4;
/// Below this long side a picture is not shrunk any further.
const MIN_SIDE: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaType {
    Png,
    Jpeg,
}

impl MediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Png => "image/png",
            MediaType::Jpeg => "image/jpeg",
        }
    }
}

pub struct Encoded {
    pub media_type: MediaType,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// How a capture's alpha channel is to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alpha {
    /// Not meaningful (X11 and GDI captures leave it 0 or 255 at random): the picture is opaque.
    Ignore,
    /// Premultiplied, as from Windows Graphics Capture and macOS (rounded window corners, transparent windows,
    /// macOS's shadow): laid over white, so clear parts do not turn black.
    PremultipliedOverWhite,
}

/// How this platform's captures carry alpha.
pub const PLATFORM_ALPHA: Alpha =
    if cfg!(any(windows, target_os = "macos")) { Alpha::PremultipliedOverWhite } else { Alpha::Ignore };

/// `(w, h)` scaled down so the long side is at most `max` (never up), keeping the aspect ratio; at least 1 px.
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max {
        return (w.max(1), h.max(1));
    }
    let scale = |v: u32| {
        let v = (u64::from(v) * u64::from(max) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(v).unwrap_or(max).max(1)
    };
    (scale(w), scale(h))
}

/// An opaque RGB copy of a capture.
pub fn to_rgb(img: &RgbaImage, alpha: Alpha) -> Result<RgbImage, String> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity(img.as_raw().len() / 4 * 3);
    for p in img.as_raw().chunks_exact(4) {
        match alpha {
            Alpha::Ignore => out.extend_from_slice(&p[..3]),
            Alpha::PremultipliedOverWhite => {
                // over white: c + (1 - a) * 255, with c already multiplied by a.
                let clear = 255 - p[3];
                out.extend(p[..3].iter().map(|c| c.saturating_add(clear)));
            }
        }
    }
    RgbImage::from_raw(w, h, out).ok_or_else(|| "The capture could not be read.".to_string())
}

fn scaled(img: RgbImage, max: u32) -> RgbImage {
    let (w, h) = fit(img.width(), img.height(), max);
    if (w, h) == img.dimensions() {
        img
    } else {
        // Triangle (bilinear) keeps text readable at a fraction of Lanczos's cost.
        imageops::resize(&img, w, h, FilterType::Triangle)
    }
}

fn jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgb8)
        .map_err(|e| format!("Could not encode the picture: {e}"))?;
    Ok(out)
}

fn png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, PngFilter::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgb8)
        .map_err(|e| format!("Could not encode the picture: {e}"))?;
    Ok(out)
}

/// A capture as a chat picture, within `MAX_SIDE` and `MAX_BYTES`.
pub fn for_chat(img: &RgbaImage, alpha: Alpha) -> Result<Encoded, String> {
    encode_within(to_rgb(img, alpha)?, MAX_SIDE, MAX_BYTES)
}

/// Scale to `max_side`, then JPEG at `JPEG_QUALITY` or PNG, whichever is smaller. Over `max_bytes`: lower JPEG
/// qualities, then smaller sizes, until it fits.
pub fn encode_within(img: RgbImage, max_side: u32, max_bytes: usize) -> Result<Encoded, String> {
    let mut cur = scaled(img, max_side);
    loop {
        let (width, height) = cur.dimensions();
        let j = jpeg(&cur, JPEG_QUALITY)?;
        let p = png(&cur)?;
        let (media_type, bytes) = if p.len() < j.len() { (MediaType::Png, p) } else { (MediaType::Jpeg, j) };
        if bytes.len() <= max_bytes {
            return Ok(Encoded { media_type, bytes, width, height });
        }
        for q in FALLBACK_QUALITIES {
            let j = jpeg(&cur, q)?;
            if j.len() <= max_bytes {
                return Ok(Encoded { media_type: MediaType::Jpeg, bytes: j, width, height });
            }
        }
        let long = width.max(height);
        if long <= MIN_SIDE {
            return Err("The picture is too detailed to send.".into());
        }
        cur = scaled(cur, (long * SHRINK_NUM / SHRINK_DEN).max(MIN_SIDE));
    }
}

/// A picker thumbnail: PNG, long side at most `THUMB_SIDE`.
pub fn thumbnail(img: &RgbaImage, alpha: Alpha) -> Result<Vec<u8>, String> {
    let rgb = to_rgb(img, alpha)?;
    let (w, h) = fit(rgb.width(), rgb.height(), THUMB_SIDE);
    let small = if (w, h) == rgb.dimensions() { rgb } else { imageops::thumbnail(&rgb, w, h) };
    png(&small)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (RFC 4648 section 4).
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(B64[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, Rgba};

    fn decode(e: &Encoded) -> image::DynamicImage {
        let format = match e.media_type {
            MediaType::Png => ImageFormat::Png,
            MediaType::Jpeg => ImageFormat::Jpeg,
        };
        image::load_from_memory_with_format(&e.bytes, format).unwrap()
    }

    /// Deterministic noise (xorshift): the worst case for both encoders.
    fn noise(w: u32, h: u32) -> RgbImage {
        let mut s = 0x2545_f491_u32;
        RgbImage::from_fn(w, h, |_, _| {
            let mut px = [0u8; 3];
            for c in &mut px {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                *c = s as u8;
            }
            Rgb(px)
        })
    }

    /// Something like a window: flat areas, a few colours, sharp edges (PNG's strength).
    fn ui_like(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| match (x / 40 + y / 25) % 3 {
            0 => Rgb([250, 250, 250]),
            1 => Rgb([30, 30, 30]),
            _ => Rgb([0, 120, 215]),
        })
    }

    /// Something like a photo: smooth gradients (JPEG's strength).
    fn photo_like(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            Rgb([(255.0 * fx) as u8, (255.0 * fy) as u8, (128.0 + 100.0 * (fx * 9.0).sin() * (fy * 7.0).cos()) as u8])
        })
    }

    #[test]
    fn fit_keeps_the_aspect_ratio_and_never_upscales() {
        assert_eq!(fit(3840, 2160, 2048), (2048, 1152));
        assert_eq!(fit(2160, 3840, 2048), (1152, 2048));
        assert_eq!(fit(5120, 1440, 2048), (2048, 576));
        assert_eq!(fit(2048, 1000, 2048), (2048, 1000));
        assert_eq!(fit(800, 600, 2048), (800, 600));
        assert_eq!(fit(1920, 1080, 240), (240, 135));
        assert_eq!(fit(10_000, 1, 2048), (2048, 1), "never 0 px");
        assert_eq!(fit(0, 0, 2048), (1, 1));
        assert_eq!(fit(u32::MAX, u32::MAX / 2, 2048), (2048, 1024), "no overflow");
    }

    #[test]
    fn alpha_is_ignored_or_laid_over_white() {
        let img = RgbaImage::from_fn(3, 1, |x, _| match x {
            0 => Rgba([10, 20, 30, 0]),  // X11 / GDI: alpha 0 on an opaque pixel
            1 => Rgba([0, 0, 0, 0]),     // a transparent (rounded) window corner
            _ => Rgba([64, 0, 32, 128]), // macOS: half-transparent shadow, premultiplied
        });
        let opaque = to_rgb(&img, Alpha::Ignore).unwrap();
        assert_eq!(opaque.get_pixel(0, 0), &Rgb([10, 20, 30]));
        let over_white = to_rgb(&img, Alpha::PremultipliedOverWhite).unwrap();
        assert_eq!(over_white.get_pixel(1, 0), &Rgb([255, 255, 255]), "transparent -> white, not black");
        assert_eq!(over_white.get_pixel(2, 0), &Rgb([191, 127, 159]));
        assert_eq!(PLATFORM_ALPHA == Alpha::PremultipliedOverWhite, cfg!(any(windows, target_os = "macos")));
    }

    #[test]
    fn large_captures_are_scaled_to_2048() {
        let rgba = RgbaImage::from_pixel(3000, 1500, Rgba([200, 100, 50, 255]));
        let e = for_chat(&rgba, Alpha::Ignore).unwrap();
        assert_eq!((e.width, e.height), (2048, 1024));
        let d = decode(&e);
        assert_eq!((d.width(), d.height()), (2048, 1024));
        assert!(e.bytes.len() <= MAX_BYTES);
        // Small ones keep their size.
        let e = for_chat(&RgbaImage::from_pixel(640, 480, Rgba([1, 2, 3, 255])), Alpha::Ignore).unwrap();
        assert_eq!((e.width, e.height), (640, 480));
    }

    #[test]
    fn png_when_smaller_jpeg_otherwise() {
        let ui = encode_within(ui_like(800, 600), MAX_SIDE, MAX_BYTES).unwrap();
        assert_eq!(ui.media_type, MediaType::Png, "flat UI compresses better as PNG ({} bytes)", ui.bytes.len());
        assert!(ui.bytes.starts_with(b"\x89PNG"));
        let photo = encode_within(photo_like(800, 600), MAX_SIDE, MAX_BYTES).unwrap();
        assert_eq!(photo.media_type, MediaType::Jpeg, "{} bytes", photo.bytes.len());
        assert!(photo.bytes.starts_with(&[0xFF, 0xD8]));
        assert_eq!(decode(&photo).width(), 800);
        // Whatever won, it is the smaller of the two.
        let img = photo_like(800, 600);
        assert!(photo.bytes.len() <= png(&img).unwrap().len());
        assert_eq!(photo.bytes.len(), jpeg(&img, JPEG_QUALITY).unwrap().len());
    }

    #[test]
    fn over_the_limit_lowers_quality_then_size() {
        // Noise at 512 px is far over 64 KiB: lower qualities do not suffice, so it shrinks until it fits.
        let limit = 64 * 1024;
        let e = encode_within(noise(512, 512), MAX_SIDE, limit).unwrap();
        assert!(e.bytes.len() <= limit, "{}", e.bytes.len());
        assert_eq!(e.media_type, MediaType::Jpeg);
        assert!(e.width < 512 && e.width == e.height, "{}x{}", e.width, e.height);
        assert!(e.width >= MIN_SIDE);
        let d = decode(&e);
        assert_eq!((d.width(), d.height()), (e.width, e.height));

        // A limit a lower quality meets: same size, lower quality.
        let q85 = jpeg(&noise(300, 200), JPEG_QUALITY).unwrap().len();
        let q50 = jpeg(&noise(300, 200), 50).unwrap().len();
        let e = encode_within(noise(300, 200), MAX_SIDE, (q85 + q50) / 2).unwrap();
        assert_eq!((e.width, e.height), (300, 200));
        assert!(e.bytes.len() < q85);

        // Impossible: an error, not an endless loop.
        assert!(encode_within(noise(400, 400), MAX_SIDE, 100).is_err());
    }

    #[test]
    fn thumbnails_are_small_pngs() {
        let rgba = RgbaImage::from_pixel(1920, 1080, Rgba([9, 99, 199, 255]));
        let t = thumbnail(&rgba, Alpha::Ignore).unwrap();
        let d = image::load_from_memory_with_format(&t, ImageFormat::Png).unwrap();
        assert_eq!((d.width(), d.height()), (240, 135));
        let tiny = thumbnail(&RgbaImage::from_pixel(100, 50, Rgba([0, 0, 0, 255])), Alpha::Ignore).unwrap();
        let d = image::load_from_memory_with_format(&tiny, ImageFormat::Png).unwrap();
        assert_eq!((d.width(), d.height()), (100, 50));
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, b64) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), b64, "{raw:?}");
        }
        assert_eq!(base64(&[0xFB, 0xFF, 0xBF]), "+/+/");
        assert_eq!(base64(&[0u8; 5]).len(), 8);
    }
}
