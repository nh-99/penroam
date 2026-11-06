//! Checks on embedded images: the bytes must really be a PNG or JPEG of a
//! size the webview can safely decode. Dimensions are read from the header,
//! without decoding anything.

use crate::limits;

/// The MIME type of an acceptable image, or why it isn't one.
pub(crate) fn check_image(b: &[u8]) -> Result<&'static str, &'static str> {
    if b.len() > limits::MAX_IMAGE_BYTES {
        return Err("too large");
    }
    let (media, dims) = sniff(b).ok_or("unknown image type")?;
    let (w, h) = dims.ok_or("damaged header")?;
    if w == 0 || h == 0 || w * h > limits::MAX_IMAGE_PIXELS {
        return Err("unsupported dimensions");
    }
    Ok(media)
}

/// Width and height in pixels, for an image [`check_image`] accepted.
pub(crate) fn pixel_size(b: &[u8]) -> Option<(u64, u64)> {
    sniff(b)?.1
}

/// The type a signature promises, and the dimensions in the header if it has any.
fn sniff(b: &[u8]) -> Option<(&'static str, Option<(u64, u64)>)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", png_size(b)))
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", jpeg_size(b)))
    } else {
        None
    }
}

fn png_size(b: &[u8]) -> Option<(u64, u64)> {
    if b.len() < 24 || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    Some((w as u64, h as u64))
}

fn jpeg_size(b: &[u8]) -> Option<(u64, u64)> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xff {
            i += 1; // fill byte
            continue;
        }
        // SOF0..SOF15, except DHT (c4), JPG (c8) and DAC (cc)
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return Some((w as u64, h as u64));
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}
