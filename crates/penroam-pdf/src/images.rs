//! Embeds a notebook's PNG and JPEG assets in an annotated PDF as image XObjects.
//!
//! The bytes come from the notebook, which may have been imported or synced from anywhere, so
//! every asset is treated as hostile:
//!
//! * Dimensions are read from the header first, and anything over [`MAX_IMAGE_PIXELS`] is
//!   refused before any pixel is decoded or any buffer sized from the header is allocated.
//! * Decoding is limited to [`MAX_DECODED_BYTES`] and buffers are reserved fallibly, so a
//!   decompression bomb costs a warning rather than the process.
//! * A failed image never fails the export. It is left out, with one warning in
//!   [`ExportReport::warnings`](crate::ExportReport::warnings) per distinct image.
//!
//! JPEG bytes pass through unchanged behind `DCTDecode`, so nothing is recompressed. PNGs are
//! decoded to 8-bit RGB behind `FlateDecode`, with any transparency in a separate soft mask.
//! Nothing is ever downsampled. Each distinct asset becomes one XObject however many pages use
//! it; only its resource name is per page. See `docs/formats/portability.md`.

use crate::{f, name};
use flate2::{write::ZlibEncoder, Compression};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use penroam_core::{Element, ImageElement, Page};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

/// Most pixels accepted from one image, the same limit importers apply.
pub const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// Most bytes one PNG may decode to: [`MAX_IMAGE_PIXELS`] of 8-bit RGBA.
const MAX_DECODED_BYTES: usize = MAX_IMAGE_PIXELS as usize * 4;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Why an image was left out. The messages are shown to the user after "Page N: ".
#[derive(Debug, Clone, Copy, thiserror::Error)]
enum ImageProblem {
    #[error("an image is missing from the notebook and was left out.")]
    Missing,
    #[error("an image is damaged and was left out.")]
    Damaged,
    #[error("an image is not a PNG or JPEG and was left out.")]
    NotAnImage,
    #[error("an image is over the 100 megapixel limit and was left out.")]
    TooLarge,
    #[error("an image uses a JPEG variant PDF cannot show and was left out.")]
    UnsupportedJpeg,
}

/// An image XObject in the output document.
#[derive(Clone, Copy)]
struct Embedded {
    id: ObjectId,
    /// EXIF orientation (1 to 8). PNGs are always 1.
    orientation: u8,
}

/// The images already written to a document, keyed by asset hash. A failed image is remembered
/// too, so a page repeated through a notebook warns once and isn't decoded again.
#[derive(Default)]
pub(crate) struct ImageSet {
    images: HashMap<String, Option<Embedded>>,
}

impl ImageSet {
    /// Embeds whatever `page` needs that isn't in `out` yet and names it for the page.
    ///
    /// `taken` lists the XObject names the page's source PDF already uses. A PDF exported by
    /// Penroam and imported again carries our own names, and replacing one would change what
    /// the source page draws. `number` is the one-based output page, used in warnings.
    pub(crate) fn place_on_page<'p>(
        &mut self,
        out: &mut Document,
        page: &'p Page,
        number: usize,
        taken: &BTreeSet<Vec<u8>>,
        asset: &mut impl FnMut(&str) -> Option<Vec<u8>>,
        warnings: &mut Vec<String>,
    ) -> PageImages<'p> {
        let mut placed = PageImages::default();
        let mut names = (0u32..)
            .map(|n| format!("PRim{n}"))
            .filter(|n| !taken.contains(n.as_bytes()));
        for element in &page.elements {
            let Element::Image(image) = element else {
                continue;
            };
            let hash = image.asset_hash.as_str();
            if placed.names.contains_key(hash) {
                continue;
            }
            let embedded = *self.images.entry(hash.to_string()).or_insert_with(|| {
                let result = asset(hash)
                    .ok_or(ImageProblem::Missing)
                    .and_then(|bytes| embed(out, bytes));
                result
                    .map_err(|problem| warnings.push(format!("Page {number}: {problem}")))
                    .ok()
            });
            if let (Some(embedded), Some(resource)) = (embedded, names.next()) {
                placed
                    .xobjects
                    .set(resource.as_bytes(), Object::Reference(embedded.id));
                placed.names.insert(hash, (resource, embedded.orientation));
            }
        }
        placed
    }
}

/// The images one page can draw, and the resource entries that make their names resolve.
#[derive(Default)]
pub(crate) struct PageImages<'p> {
    /// Asset hash to resource name and EXIF orientation.
    names: HashMap<&'p str, (String, u8)>,
    /// `/XObject` entries for the page's resources.
    pub(crate) xobjects: Dictionary,
}

impl PageImages<'_> {
    /// Appends the operators that draw `image` in y-down display coordinates. Returns false,
    /// writing nothing, if the image was left out.
    pub(crate) fn draw(&self, s: &mut String, image: &ImageElement) -> bool {
        let Some((resource, orientation)) = self.names.get(image.asset_hash.as_str()) else {
            return false;
        };
        let [a, b, c, d, e, g] = placement(image, *orientation);
        let _ = writeln!(
            s,
            "q {} {} {} {} {} {} cm /{resource} Do Q",
            f(a),
            f(b),
            f(c),
            f(d),
            f(e),
            f(g)
        );
        true
    }
}

/// The matrix taking PDF's unit image square to the element's box in y-down coordinates.
///
/// An image's unit square has its top edge at v = 1, so with y down it must be flipped to come
/// out upright. The editor decodes with `createImageBitmap`, which applies EXIF orientation,
/// and the element's box is the shape it shows. Doing the same turn here keeps the JPEG's
/// bytes untouched while it lands the same way up.
fn placement(i: &ImageElement, orientation: u8) -> [f64; 6] {
    // Each row gives the fractions across and down the box as (u, v, constant) pairs.
    let ([su, sv, s0], [tu, tv, t0]) = match orientation {
        2 => ([-1.0, 0.0, 1.0], [0.0, -1.0, 1.0]),
        3 => ([-1.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        4 => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        5 => ([0.0, -1.0, 1.0], [1.0, 0.0, 0.0]),
        6 => ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        7 => ([0.0, 1.0, 0.0], [-1.0, 0.0, 1.0]),
        8 => ([0.0, -1.0, 1.0], [-1.0, 0.0, 1.0]),
        _ => ([1.0, 0.0, 0.0], [0.0, -1.0, 1.0]),
    };
    [
        i.width * su,
        i.height * tu,
        i.width * sv,
        i.height * tv,
        i.x + i.width * s0,
        i.y + i.height * t0,
    ]
}

fn embed(out: &mut Document, bytes: Vec<u8>) -> Result<Embedded, ImageProblem> {
    if bytes.starts_with(PNG_SIGNATURE) {
        let id = png_xobject(out, &bytes)?;
        Ok(Embedded { id, orientation: 1 })
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        jpeg_xobject(out, bytes)
    } else {
        Err(ImageProblem::NotAnImage)
    }
}

fn check_size(width: u32, height: u32) -> Result<(), ImageProblem> {
    if width == 0 || height == 0 {
        return Err(ImageProblem::Damaged);
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(ImageProblem::TooLarge);
    }
    Ok(())
}

/// The dictionary shared by every image XObject.
fn image_dict(width: u32, height: u32, colour: &str, filter: &str) -> Dictionary {
    Dictionary::from_iter(vec![
        ("Type", name("XObject")),
        ("Subtype", name("Image")),
        ("Width", Object::Integer(i64::from(width))),
        ("Height", Object::Integer(i64::from(height))),
        ("ColorSpace", name(colour)),
        ("BitsPerComponent", Object::Integer(8)),
        ("Filter", name(filter)),
    ])
}

fn png_failure(e: png::DecodingError) -> ImageProblem {
    match e {
        png::DecodingError::LimitsExceeded => ImageProblem::TooLarge,
        _ => ImageProblem::Damaged,
    }
}

/// Decodes a PNG into an RGB XObject, with a soft mask if any pixel isn't fully opaque.
fn png_xobject(out: &mut Document, bytes: &[u8]) -> Result<ObjectId, ImageProblem> {
    let limits = png::Limits {
        bytes: MAX_DECODED_BYTES,
    };
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    let (width, height) = decoder.read_header_info().map_err(png_failure)?.size();
    check_size(width, height)?;
    // Palette, tRNS, low bit depths and 16-bit samples all arrive as 8-bit gray or RGB, plus alpha.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(png_failure)?;
    let (colour, _) = reader.output_color_type();
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= MAX_DECODED_BYTES)
        .ok_or(ImageProblem::TooLarge)?;
    let mut pixels = Vec::new();
    // A plain allocation would abort the app on a device short of memory.
    pixels
        .try_reserve_exact(size)
        .map_err(|_| ImageProblem::TooLarge)?;
    pixels.resize(size, 0);
    let frame = reader.next_frame(&mut pixels).map_err(png_failure)?;
    if (frame.width, frame.height) != (width, height) {
        return Err(ImageProblem::Damaged);
    }

    use png::ColorType::{Grayscale, GrayscaleAlpha, Rgb, Rgba};
    let (channels, gray, has_alpha) = match colour {
        Grayscale => (1, true, false),
        GrayscaleAlpha => (2, true, true),
        Rgb => (3, false, false),
        Rgba => (4, false, true),
        png::ColorType::Indexed => return Err(ImageProblem::Damaged),
    };
    let translucent = has_alpha
        && pixels
            .chunks_exact(channels)
            .any(|p| p[channels - 1] != u8::MAX);

    // Rows go straight into the compressors so the raw image exists only once in memory.
    let mut rgb = ZlibEncoder::new(Vec::new(), Compression::default());
    let mut alpha = translucent.then(|| ZlibEncoder::new(Vec::new(), Compression::default()));
    let mut row = Vec::new();
    for line in pixels.chunks_exact(frame.line_size) {
        row.clear();
        for pixel in line.chunks_exact(channels) {
            if gray {
                row.extend_from_slice(&[pixel[0]; 3]);
            } else {
                row.extend_from_slice(&pixel[..3]);
            }
        }
        rgb.write_all(&row).map_err(|_| ImageProblem::Damaged)?;
        if let Some(alpha) = &mut alpha {
            row.clear();
            row.extend(line.chunks_exact(channels).map(|p| p[channels - 1]));
            alpha.write_all(&row).map_err(|_| ImageProblem::Damaged)?;
        }
    }
    drop(pixels);

    let mut dict = image_dict(width, height, "DeviceRGB", "FlateDecode");
    if let Some(alpha) = alpha {
        let mask = alpha.finish().map_err(|_| ImageProblem::Damaged)?;
        let mask_dict = image_dict(width, height, "DeviceGray", "FlateDecode");
        let mask = out.add_object(Stream::new(mask_dict, mask).with_compression(false));
        dict.set("SMask", Object::Reference(mask));
    }
    let rgb = rgb.finish().map_err(|_| ImageProblem::Damaged)?;
    Ok(out.add_object(Stream::new(dict, rgb).with_compression(false)))
}

/// What a JPEG's headers say, up to its first scan.
struct JpegHeader {
    width: u32,
    height: u32,
    components: u8,
    /// An Adobe APP14 segment is present.
    adobe: bool,
    orientation: u8,
}

/// Walks the marker segments before the first scan. Every read is bounds-checked and each step
/// moves forward, so a hostile file can only end the walk early.
fn jpeg_header(b: &[u8]) -> Result<JpegHeader, ImageProblem> {
    let mut size = None;
    let mut adobe = false;
    let mut orientation = 1;
    let mut i = 2;
    loop {
        if *b.get(i).ok_or(ImageProblem::Damaged)? != 0xff {
            return Err(ImageProblem::Damaged);
        }
        // Any number of 0xff fill bytes may come before the marker code.
        while b.get(i) == Some(&0xff) {
            i += 1;
        }
        let marker = *b.get(i).ok_or(ImageProblem::Damaged)?;
        i += 1;
        match marker {
            // start of scan: every header that matters has been seen
            0xda => break,
            // a second SOI, or EOI, before any scan
            0xd8 | 0xd9 => return Err(ImageProblem::Damaged),
            // standalone markers have no length
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }
        let length = b.get(i..i + 2).ok_or(ImageProblem::Damaged)?;
        let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        let segment = length
            .checked_sub(2)
            .and_then(|body| b.get(i + 2..i + 2 + body))
            .ok_or(ImageProblem::Damaged)?;
        i += length;
        match marker {
            // baseline, extended sequential and progressive: what DCTDecode reads
            0xc0..=0xc2 => {
                if size.is_some() {
                    return Err(ImageProblem::Damaged);
                }
                size = Some(jpeg_frame(segment)?);
            }
            // lossless, hierarchical and arithmetic-coded frames; c4, c8 and cc aren't frames
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(ImageProblem::UnsupportedJpeg)
            }
            0xee => adobe |= segment.starts_with(b"Adobe"),
            0xe1 => {
                if let Some(tiff) = segment.strip_prefix(b"Exif\0\0") {
                    orientation = exif_orientation(tiff).unwrap_or(orientation);
                }
            }
            _ => {}
        }
    }
    let (width, height, components) = size.ok_or(ImageProblem::Damaged)?;
    Ok(JpegHeader {
        width,
        height,
        components,
        adobe,
        orientation,
    })
}

/// Reads a frame header's size and component count.
fn jpeg_frame(segment: &[u8]) -> Result<(u32, u32, u8), ImageProblem> {
    let &[precision, h1, h0, w1, w0, components, ref specs @ ..] = segment else {
        return Err(ImageProblem::Damaged);
    };
    if specs.len() < usize::from(components) * 3 {
        return Err(ImageProblem::Damaged);
    }
    // PDF only defines 8-bit DCT samples, and its colour spaces stop at four components.
    if precision != 8 || !matches!(components, 1 | 3 | 4) {
        return Err(ImageProblem::UnsupportedJpeg);
    }
    let width = u32::from(u16::from_be_bytes([w1, w0]));
    let height = u32::from(u16::from_be_bytes([h1, h0]));
    check_size(width, height)?;
    Ok((width, height, components))
}

/// The orientation tag from an EXIF TIFF block, if it has a valid one. The tag is in the first
/// directory, which is at most 65535 entries long.
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let little = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |i: usize| {
        let bytes = [*tiff.get(i)?, *tiff.get(i.checked_add(1)?)?];
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let offset: [u8; 4] = tiff.get(4..8)?.try_into().ok()?;
    let directory = usize::try_from(if little {
        u32::from_le_bytes(offset)
    } else {
        u32::from_be_bytes(offset)
    })
    .ok()?;
    let entries = usize::from(u16_at(directory)?);
    (0..entries).find_map(|n| {
        let entry = directory.checked_add(2 + n * 12)?;
        if u16_at(entry)? != 0x0112 {
            return None;
        }
        let value = u16_at(entry.checked_add(8)?)?;
        u8::try_from(value).ok().filter(|v| (1..=8).contains(v))
    })
}

/// Wraps a JPEG's original bytes in an XObject.
fn jpeg_xobject(out: &mut Document, bytes: Vec<u8>) -> Result<Embedded, ImageProblem> {
    let header = jpeg_header(&bytes)?;
    let colour = match header.components {
        1 => "DeviceGray",
        3 => "DeviceRGB",
        _ => "DeviceCMYK",
    };
    let mut dict = image_dict(header.width, header.height, colour, "DCTDecode");
    if header.components == 4 && header.adobe {
        // Adobe writes CMYK JPEGs with inverted samples. Decoders only undo that when told to.
        let inverted = [1, 0, 1, 0, 1, 0, 1, 0].map(Object::Integer).to_vec();
        dict.set("Decode", Object::Array(inverted));
    }
    // already compressed: the document-wide compression pass must leave it alone
    let id = out.add_object(Stream::new(dict, bytes).with_compression(false));
    Ok(Embedded {
        id,
        orientation: header.orientation,
    })
}
