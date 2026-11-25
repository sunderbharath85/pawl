//! Pictures in chat (v0.2 item 4): one screen capture the user approved for one message.
//!
//! [`check_image`] validates a [`ChatImage`] before anything else touches it: PNG or JPEG only, the bytes
//! must be what the media type says, at most [`MAX_IMAGE_BYTES`] decoded and [`MAX_IMAGE_SIDE`] px on the
//! long side. The picture is used once, for one answer: the brain never stores it, never logs it
//! ([`ChatImage`]'s `Debug` shows its length only) and never puts it into a context pack.
//!
//! Which configured model can see it ([`Brain::look`](super::Brain::look)): Anthropic (Claude 3 and later),
//! OpenAI (not the text-only gpt-3.5 / o1-mini / o3-mini / audio models), Gemini, Ollama when
//! `/api/show` lists the `vision` capability, and the hub's agent session when its agent takes images.
//! Request shapes:
//! - Anthropic: a user `content` array, `{type: "image", source: {type: "base64", media_type, data}}` then
//!   the text block;
//! - OpenAI Chat Completions: `{type: "text"}` then `{type: "image_url", image_url: {url: "data:<type>;base64,<data>"}}`;
//! - Gemini: `parts` = `{inline_data: {mime_type, data}}` then `{text}`;
//! - Ollama `/api/chat`: the user message's `images: [<base64>]`.

use pawl_protocol::ChatImage;

/// Largest picture, decoded.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// Longest side, in pixels (the desktop scales captures to this).
pub const MAX_IMAGE_SIDE: u32 = 2048;
/// The media types a picture may have.
pub const MEDIA_TYPES: &[&str] = &["image/png", "image/jpeg"];
/// Pictures per message.
pub const MAX_IMAGES: usize = 1;
/// The reply when no configured model can see pictures.
pub const NO_VISION: &str = "I can't see pictures with the current model.";

/// What [`check_image`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// Decoded size.
    pub bytes: usize,
}

/// Validate one picture: type, base64, size, that the bytes are that type, and its pixel size.
pub fn check_image(img: &ChatImage) -> Result<ImageInfo, String> {
    let mt = img.media_type.trim();
    if !MEDIA_TYPES.contains(&mt) || mt != img.media_type {
        return Err(format!("a picture must be image/png or image/jpeg, not {:?}", crate::util::truncate(&img.media_type, 40)));
    }
    // 4 base64 characters carry 3 bytes: refuse an oversized one before decoding it.
    let max_chars = MAX_IMAGE_BYTES.div_ceil(3) * 4;
    if img.data.len() > max_chars {
        return Err(format!("a picture may be at most {} MiB", MAX_IMAGE_BYTES / (1024 * 1024)));
    }
    let bytes = decode_base64(&img.data).ok_or("the picture is not valid base64")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("a picture may be at most {} MiB", MAX_IMAGE_BYTES / (1024 * 1024)));
    }
    let (width, height) = match mt {
        "image/png" => png_size(&bytes).ok_or("the picture is not a PNG image")?,
        _ => jpeg_size(&bytes).ok_or("the picture is not a JPEG image")?,
    };
    if width == 0 || height == 0 {
        return Err("the picture has no pixels".into());
    }
    if width.max(height) > MAX_IMAGE_SIDE {
        return Err(format!("a picture may be at most {MAX_IMAGE_SIDE} px on its long side (this one is {width}x{height})"));
    }
    Ok(ImageInfo { width, height, bytes: bytes.len() })
}

/// Standard base64 (RFC 4648, `+/`, `=` padding, length a multiple of 4, no whitespace).
pub(crate) fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return None;
    }
    let pad = b.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let body = &b[..b.len() - pad];
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in body {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Leftover bits must be zero (a canonical encoding).
    (acc == 0).then_some(out)
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if b.len() < 24 || &b[..8] != SIG || &b[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Some((be(16), be(20)))
}

/// Width and height from a JPEG's first start-of-frame segment.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 || b[2] != 0xFF {
        return None;
    }
    let mut i = 2;
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        // Fill bytes.
        while i < b.len() && b[i] == 0xFF {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;
        match marker {
            // Standalone markers carry no length.
            0x01 | 0xD0..=0xD7 => continue,
            // Start of scan or end of image before any frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]));
        if len < 2 {
            return None;
        }
        // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([*b.get(i + 3)?, *b.get(i + 4)?]);
            let w = u16::from_be_bytes([*b.get(i + 5)?, *b.get(i + 6)?]);
            return Some((u32::from(w), u32::from(h)));
        }
        i += len;
    }
    None
}

/// Can this Anthropic model see pictures? Every Claude model since Claude 3 can.
pub(crate) fn anthropic_sees(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    !(m.starts_with("claude-2") || m.starts_with("claude-instant") || m.starts_with("claude-1"))
}

/// Can this OpenAI model see pictures? Not the text-only ones (gpt-3.5, o1-mini, o3-mini) or the audio,
/// speech and transcription models.
pub(crate) fn openai_sees(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    let text_only = ["gpt-3.5", "o1-mini", "o1-preview", "o3-mini", "davinci", "babbage"];
    let not_chat = ["audio", "realtime", "tts", "transcribe", "whisper", "embedding", "search"];
    !(text_only.iter().any(|p| m.starts_with(p)) || not_chat.iter().any(|w| m.contains(w)))
}

/// Ollama: does `/api/show`'s `capabilities` list `vision`?
pub(crate) fn ollama_shows_vision(show: &serde_json::Value) -> bool {
    show.get("capabilities").and_then(serde_json::Value::as_array).is_some_and(|c| c.iter().any(|v| v == "vision"))
}

/// The `/api/show` URL next to an Ollama `/api/chat` URL.
pub(crate) fn ollama_show_url(chat_url: &str) -> String {
    match chat_url.strip_suffix("/api/chat") {
        Some(base) => format!("{base}/api/show"),
        None => format!("{}/api/show", chat_url.trim_end_matches('/')),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Standard base64 of `bytes` (tests only).
    pub(crate) fn b64(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
            for k in 0..4 {
                if k <= c.len() {
                    s.push(A[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    }

    /// A tiny PNG header of `w` x `h` (enough for [`check_image`]).
    pub(crate) fn png(w: u32, h: u32) -> ChatImage {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        ChatImage { media_type: "image/png".into(), data: b64(&b) }
    }

    /// A tiny JPEG header (SOI, an APP0 segment, SOF0 of `w` x `h`).
    pub(crate) fn jpeg(w: u16, h: u16) -> ChatImage {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        b.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xFF, 0xD9]);
        ChatImage { media_type: "image/jpeg".into(), data: b64(&b) }
    }

    #[test]
    fn base64_round_trips_and_rejects_bad_input() {
        for v in [&b"a"[..], &b"ab"[..], &b"abc"[..], &b"abcd"[..], &[0u8, 255, 128, 7, 9][..]] {
            assert_eq!(decode_base64(&b64(v)).as_deref(), Some(v), "{v:?}");
        }
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        // "TWF=" leaves non-zero bits over: not a canonical encoding.
        for bad in ["", "TWF", "TW=u", "TWFu\n", "TW-u", "T===", "TWF="] {
            assert!(decode_base64(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn pictures_are_checked_for_type_size_and_bytes() {
        assert_eq!(check_image(&png(1280, 800)).unwrap(), ImageInfo { width: 1280, height: 800, bytes: 33 });
        assert_eq!((check_image(&jpeg(1920, 1080)).unwrap().width, check_image(&jpeg(1920, 1080)).unwrap().height), (1920, 1080));
        // Type and bytes must agree; only PNG and JPEG.
        let mut lie = png(10, 10);
        lie.media_type = "image/jpeg".into();
        assert!(check_image(&lie).unwrap_err().contains("not a JPEG"));
        let mut gif = png(10, 10);
        gif.media_type = "image/gif".into();
        assert!(check_image(&gif).unwrap_err().contains("image/png or image/jpeg"));
        gif.media_type = " image/png".into();
        assert!(check_image(&gif).is_err(), "exact media type");
        assert!(check_image(&ChatImage { media_type: "image/png".into(), data: "not base64!".into() }).unwrap_err().contains("base64"));
        assert!(check_image(&ChatImage { media_type: "image/png".into(), data: b64(b"plain text, not a picture") }).is_err());
        // Pixel and byte limits.
        assert!(check_image(&png(2048, 1536)).is_ok());
        assert!(check_image(&png(4096, 100)).unwrap_err().contains("2048 px"));
        assert!(check_image(&jpeg(100, 3000)).is_err());
        assert!(check_image(&png(0, 10)).is_err());
        let big = ChatImage { media_type: "image/png".into(), data: "A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4) };
        assert!(check_image(&big).unwrap_err().contains("4 MiB"));
        // Debug never shows the data.
        let p = png(1, 1);
        assert!(!format!("{p:?}").contains(&p.data));
    }

    #[test]
    fn which_models_see() {
        assert!(anthropic_sees("claude-haiku-5-5") && anthropic_sees("claude-3-haiku-20240307") && !anthropic_sees("claude-2.1"));
        assert!(openai_sees("gpt-6-luna") && openai_sees("gpt-4o-mini") && openai_sees("o3"));
        assert!(!openai_sees("gpt-3.5-turbo") && !openai_sees("o3-mini") && !openai_sees("gpt-4o-audio-preview"));
        assert!(ollama_shows_vision(&serde_json::json!({ "capabilities": ["completion", "vision"] })));
        assert!(!ollama_shows_vision(&serde_json::json!({ "capabilities": ["completion"] })) && !ollama_shows_vision(&serde_json::json!({})));
        assert_eq!(ollama_show_url("http://127.0.0.1:11434/api/chat"), "http://127.0.0.1:11434/api/show");
    }
}
