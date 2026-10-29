//! Signatures and initials as signers submit them: a drawn image (a PNG data URL, as a browser
//! canvas produces) or typed text. Drawn ones are decoded completely when submitted, so a
//! malformed, oversized or empty image is refused then, not when the document is sealed.

use crate::pdf::Image;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::io::Cursor;

/// Largest PNG accepted (a canvas signature is typically 5–60 KB).
pub const MAX_PNG_BYTES: usize = 300 * 1024;
const MIN_SIZE: (u32, u32) = (20, 10);
const MAX_SIZE: (u32, u32) = (3000, 1500);
const PREFIX: &str = "data:image/png;base64,";
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const MAX_TYPED: usize = 100;

/// The PNG inside a `data:image/png;base64,...` URL.
pub fn png_bytes(data_url: &str) -> Result<Vec<u8>, String> {
    let b64 = data_url.strip_prefix(PREFIX).ok_or("a drawn signature must be a PNG data URL (data:image/png;base64,...)")?;
    if b64.len() > MAX_PNG_BYTES.div_ceil(3) * 4 {
        return Err(format!("the signature image is larger than {} KB", MAX_PNG_BYTES / 1024));
    }
    STANDARD.decode(b64).map_err(|_| "the signature image is not valid base64".to_string())
}

/// Decodes a drawn signature: a PNG between 20×10 and 3000×1500 pixels that shows some ink.
pub fn decode(png: &[u8]) -> Result<Image, String> {
    if png.len() > MAX_PNG_BYTES {
        return Err(format!("the signature image is larger than {} KB", MAX_PNG_BYTES / 1024));
    }
    if !png.starts_with(PNG_MAGIC) {
        return Err("the signature image is not a PNG".into());
    }
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(png), png::Limits { bytes: 64 << 20 });
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let bad = |e: png::DecodingError| format!("the signature image cannot be decoded ({e})");
    let mut reader = decoder.read_info().map_err(bad)?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 || w > MAX_SIZE.0 || h > MAX_SIZE.1 {
        return Err(format!("the signature image is {w}×{h} pixels; use between {}×{} and {}×{}", MIN_SIZE.0, MIN_SIZE.1, MAX_SIZE.0, MAX_SIZE.1));
    }
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("the signature image is too large")?];
    let frame = reader.next_frame(&mut buf).map_err(bad)?;
    buf.truncate(frame.buffer_size());
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("the signature image uses an unsupported color type".into()),
    };
    let px = (w * h) as usize;
    if frame.bit_depth != png::BitDepth::Eight || buf.len() != px * channels {
        return Err("the signature image uses an unsupported pixel format".into());
    }
    let mut rgb = Vec::with_capacity(px * 3);
    let mut alpha = Vec::with_capacity(if channels % 2 == 0 { px } else { 0 });
    for p in buf.chunks_exact(channels) {
        match channels {
            1 | 2 => rgb.extend_from_slice(&[p[0], p[0], p[0]]),
            _ => rgb.extend_from_slice(&p[..3]),
        }
        if channels % 2 == 0 {
            alpha.push(p[channels - 1]);
        }
    }
    let alpha = (!alpha.is_empty()).then_some(alpha);
    // Ink: a visible pixel clearly darker than paper. A blank canvas has none.
    let inked =
        rgb.chunks_exact(3).enumerate().filter(|(i, c)| alpha.as_ref().is_none_or(|a| a[*i] >= 64) && (c[0] as u32 + c[1] as u32 + c[2] as u32) < 600).count();
    if inked < 20 {
        return Err("the signature image is empty: draw your signature".into());
    }
    Ok(Image { width: w, height: h, rgb, alpha })
}

/// A typed signature or initials: 1–100 printable characters with at least one letter.
pub fn typed(text: &str) -> Result<String, String> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > MAX_TYPED || t.chars().any(char::is_control) || !t.chars().any(char::is_alphabetic) {
        return Err(format!("type 1 to {MAX_TYPED} characters, including a letter"));
    }
    Ok(t.to_string())
}

/// Initials from a name: the first letter of each word, at most four (`"Jane van Doe"` → `JVD`).
pub fn initials_of(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphabetic())).flat_map(char::to_uppercase).take(4).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `w`×`h` pixels with a dark stroke across the middle, as a data URL.
    pub(crate) fn drawn(w: u32, h: u32, color: png::ColorType) -> String {
        let channels = match color {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            _ => 4,
        };
        let mut data = vec![0u8; (w * h) as usize * channels];
        for (i, p) in data.chunks_exact_mut(channels).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let ink = y.abs_diff(h / 2) <= 1 && x > 2 && x < w - 2;
            for (c, v) in p.iter_mut().enumerate() {
                let is_alpha = channels % 2 == 0 && c == channels - 1;
                *v = match (is_alpha, ink) {
                    (true, true) => 255,
                    (true, false) => 0,
                    (false, true) => 20,
                    (false, false) => 255,
                };
            }
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&data).unwrap();
        }
        format!("{PREFIX}{}", STANDARD.encode(out))
    }

    #[test]
    fn drawn_signatures_are_validated_and_decoded() {
        let img = decode(&png_bytes(&drawn(300, 100, png::ColorType::Rgba)).unwrap()).unwrap();
        assert_eq!((img.width, img.height, img.rgb.len()), (300, 100, 90_000));
        assert_eq!(img.alpha.as_ref().map(Vec::len), Some(30_000));
        let gray = decode(&png_bytes(&drawn(60, 20, png::ColorType::Grayscale)).unwrap()).unwrap();
        assert!(gray.alpha.is_none() && gray.rgb.len() == 60 * 20 * 3);

        assert!(png_bytes("data:image/jpeg;base64,/9j/4AAQ").unwrap_err().contains("PNG data URL"));
        assert!(png_bytes("data:image/png;base64,***").unwrap_err().contains("base64"));
        assert!(png_bytes(&format!("{PREFIX}{}", "A".repeat(500_000))).unwrap_err().contains("larger than"));
        assert!(decode(b"GIF89a....").unwrap_err().contains("not a PNG"));
        assert!(decode(&png_bytes(&drawn(10, 5, png::ColorType::Rgba)).unwrap()).unwrap_err().contains("10×5 pixels"));
        assert!(decode(&png_bytes(&drawn(3001, 20, png::ColorType::Rgba)).unwrap()).unwrap_err().contains("pixels"));
        let mut cut = png_bytes(&drawn(300, 100, png::ColorType::Rgb)).unwrap();
        cut.truncate(cut.len() - 30);
        assert!(decode(&cut).is_err(), "a truncated PNG is refused");

        // A blank canvas (fully transparent) is not a signature.
        let mut blank = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut blank, 200, 80);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&vec![0u8; 200 * 80 * 4]).unwrap();
        }
        assert!(decode(&blank).unwrap_err().contains("empty"));
    }

    #[test]
    fn typed_signatures_and_initials() {
        assert_eq!(typed("  Jane Doe ").unwrap(), "Jane Doe");
        assert!(typed("").is_err() && typed("12345").is_err() && typed("a\u{7}b").is_err() && typed(&"x".repeat(101)).is_err());
        assert_eq!(initials_of("jane van doe"), "JVD");
        assert_eq!(initials_of("Ångström Øre"), "ÅØ");
        assert_eq!(initials_of(""), "");
    }
}
