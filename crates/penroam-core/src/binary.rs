//! Penroam binary page encoding (`.prp`), file format version 1. Normative
//! spec: `docs/formats/penroam-binary-v1.md`.
//!
//! The same idea as a custom container (magic + version + payload), but
//! lossless for everything a page holds:
//!
//! ```text
//! 0      4  magic "PRPG"
//! 4      2  version u16 LE (= 1)
//! 6      2  flags u16 LE. Bits 0-7 = required features (unknown -> refuse):
//!           bit 0 = meta is zlib-compressed (RFC 1950). Bits 8-15 = optional
//!           (unknown -> ignore): bit 8 = an extension area follows the
//!           point blocks.
//! 8      4  meta length M (stored bytes), u32 LE
//! 12     M  meta: the Page as JSON (UTF-8) with every ink stroke's "points" omitted
//! 12+M   4  ink stroke count S, u32 LE (= ink elements in meta order)
//! ..        S point blocks (below)
//! ..        [flag bit 8] u32 count, then count x (u32 tag, u32 len, len bytes)
//! end-4  4  CRC-32 (IEEE) of every preceding byte, u32 LE
//! ```
//!
//! Point block: `varint n`, then columns x, y, pressure, tiltX, tiltY (float
//! columns), timeUs (int column), then the optional twist (float) and flags
//! (int) columns, each preceded by a presence byte (0 = no point has it,
//! 1 = every point has it, 2 = bitmap of ⌈n/8⌉ bytes follows, LSB first; the
//! column then covers only the points that have it).
//!
//! Float column: tag 0 CONST (one f64 LE: all values bit-identical);
//! tag 1 DECIMAL (u8 k ≤ 6, then n zig-zag varint deltas of q = v·10^k,
//! first delta from 0; used only when q / 10^k reproduces every value
//! bit-for-bit); tag 2 RAW (n × f64 LE).
//! Int column: tag 0 CONST (one zig-zag varint); tag 1 DELTA (n zig-zag
//! varint deltas, first from 0).
//!
//! Captured samples are rounded to 0.001 pt / 1e-4 pressure and timestamps
//! advance in small steps, so typical points cost 5–8 bytes instead of ~90 in
//! JSON, with no loss: anything that is not an exact short decimal is stored
//! as raw f64.

use crate::model::{BinaryExtension, Element, InkPoint, InkStroke, Page};
use crate::validate::limits;

pub const MAGIC: &[u8; 4] = b"PRPG";
/// `.prp` file format version. Independent of the page model schema
/// version carried inside the metadata ([`crate::PAGE_SCHEMA_VERSION`]).
pub const FILE_FORMAT_VERSION: u16 = 1;
const MAX_DECIMALS: u32 = 6;
/// Required feature: metadata is zlib-compressed.
pub const FLAG_META_ZLIB: u16 = 1;
/// Bits 0-7: a reader that does not know a set bit must refuse the file.
pub const REQUIRED_FLAGS: u16 = 0x00ff;
/// Required features this build implements.
const KNOWN_REQUIRED: u16 = FLAG_META_ZLIB;
/// Optional: an extension area follows the point blocks.
pub const FLAG_EXTENSIONS: u16 = 1 << 8;
/// Decompressed metadata limit (hostile-input guard).
pub const MAX_META: u64 = 256 << 20;
/// Extension area limits (hostile-input guard).
pub const MAX_EXTENSIONS: usize = 1024;
pub const MAX_EXTENSION_BYTES: usize = 64 << 20;
/// |q| must stay below 2^53 so the f64 <-> i64 conversion is exact.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BinaryError {
    #[error("not a Penroam page file")]
    BadMagic,
    #[error("page file version {0} is newer than this build supports")]
    TooNew(u16),
    #[error("page file needs feature {0} that this build does not support; please update Penroam")]
    UnsupportedFeature(String),
    #[error("page file is truncated or malformed: {0}")]
    Malformed(&'static str),
    #[error("page file checksum mismatch")]
    Checksum,
    #[error("page metadata: {0}")]
    Meta(String),
}

pub fn is_binary_page(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

// ------------------------------------------------------------- encode --

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn pow10(k: u32) -> f64 {
    10f64.powi(k as i32)
}

/// Smallest k for which every value is exactly q / 10^k with integer q.
fn decimal_scale(values: &[f64]) -> Option<(u32, Vec<i64>)> {
    'k: for k in 0..=MAX_DECIMALS {
        let s = pow10(k);
        let mut qs = Vec::with_capacity(values.len());
        for &v in values {
            let q = (v * s).round();
            if !q.is_finite() || q.abs() >= MAX_EXACT {
                continue 'k;
            }
            // verify through the exact decode path (this also rejects -0.0)
            let qi = q as i64;
            if ((qi as f64) / s).to_bits() != v.to_bits() {
                continue 'k;
            }
            qs.push(qi);
        }
        return Some((k, qs));
    }
    None
}

fn put_float_column(out: &mut Vec<u8>, values: &[f64]) {
    if let Some(first) = values.first() {
        if values.iter().all(|v| v.to_bits() == first.to_bits()) {
            out.push(0);
            out.extend_from_slice(&first.to_le_bytes());
            return;
        }
    }
    if let Some((k, qs)) = decimal_scale(values) {
        out.push(1);
        out.push(k as u8);
        let mut prev = 0i64;
        for q in qs {
            put_varint(out, zigzag(q.wrapping_sub(prev)));
            prev = q;
        }
        return;
    }
    out.push(2);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_int_column(out: &mut Vec<u8>, values: &[i64]) {
    if let Some(first) = values.first() {
        if values.iter().all(|v| v == first) {
            out.push(0);
            put_varint(out, zigzag(*first));
            return;
        }
    }
    out.push(1);
    let mut prev = 0i64;
    for &v in values {
        put_varint(out, zigzag(v.wrapping_sub(prev)));
        prev = v;
    }
}

fn put_presence(out: &mut Vec<u8>, present: &[bool]) {
    if present.iter().all(|p| !p) {
        out.push(0);
    } else if present.iter().all(|p| *p) {
        out.push(1);
    } else {
        out.push(2);
        let mut bits = vec![0u8; present.len().div_ceil(8)];
        for (i, p) in present.iter().enumerate() {
            if *p {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        out.extend_from_slice(&bits);
    }
}

fn put_points(out: &mut Vec<u8>, pts: &[InkPoint]) {
    put_varint(out, pts.len() as u64);
    let col = |f: fn(&InkPoint) -> f64| pts.iter().map(f).collect::<Vec<f64>>();
    put_float_column(out, &col(|p| p.x));
    put_float_column(out, &col(|p| p.y));
    put_float_column(out, &col(|p| p.pressure));
    put_float_column(out, &col(|p| p.tilt_x));
    put_float_column(out, &col(|p| p.tilt_y));
    put_int_column(out, &pts.iter().map(|p| p.time_us).collect::<Vec<_>>());
    let tw: Vec<bool> = pts.iter().map(|p| p.twist.is_some()).collect();
    put_presence(out, &tw);
    if tw.iter().any(|b| *b) {
        put_float_column(out, &pts.iter().filter_map(|p| p.twist).collect::<Vec<_>>());
    }
    let fl: Vec<bool> = pts.iter().map(|p| p.flags.is_some()).collect();
    put_presence(out, &fl);
    if fl.iter().any(|b| *b) {
        put_int_column(
            out,
            &pts.iter()
                .filter_map(|p| p.flags.map(i64::from))
                .collect::<Vec<_>>(),
        );
    }
}

/// Encodes a page. Deterministic: the same page always yields the same bytes.
pub fn encode_page(page: &Page) -> Vec<u8> {
    // metadata = the page with ink points left out (cheap shallow copy)
    let meta_elements: Vec<Element> = page
        .elements
        .iter()
        .map(|e| match e {
            Element::Ink(s) => Element::Ink(InkStroke {
                id: s.id,
                z: s.z,
                tool: s.tool,
                color: s.color.clone(),
                opacity: s.opacity,
                width: s.width,
                pressure: s.pressure,
                source: s.source.clone(),
                input: s.input.clone(),
                points: Vec::new(),
                extra: s.extra.clone(),
            }),
            other => other.clone(),
        })
        .collect();
    let mut meta_page = page.clone_without_elements();
    meta_page.elements = meta_elements;
    let mut meta = serde_json::to_value(&meta_page).expect("page serialises");
    if let Some(els) = meta.get_mut("elements").and_then(|v| v.as_array_mut()) {
        for e in els {
            if let Some(o) = e.as_object_mut() {
                if o.get("type").and_then(|t| t.as_str()) == Some("ink") {
                    o.remove("points");
                }
            }
        }
    }
    let meta_json = serde_json::to_vec(&meta).expect("json");
    let meta = {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&meta_json).expect("in-memory write");
        z.finish().expect("in-memory write")
    };

    let strokes: Vec<&InkStroke> = page
        .elements
        .iter()
        .filter_map(|e| {
            if let Element::Ink(s) = e {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    let n_points: usize = strokes.iter().map(|s| s.points.len()).sum();
    let mut out = Vec::with_capacity(16 + meta.len() + n_points * 8);
    let mut flags = FLAG_META_ZLIB;
    if !page.binary_extensions.is_empty() {
        flags |= FLAG_EXTENSIONS;
    }
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FILE_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(&meta);
    out.extend_from_slice(&(strokes.len() as u32).to_le_bytes());
    for s in strokes {
        put_points(&mut out, &s.points);
    }
    if !page.binary_extensions.is_empty() {
        out.extend_from_slice(&(page.binary_extensions.len() as u32).to_le_bytes());
        for e in &page.binary_extensions {
            out.extend_from_slice(&e.tag.to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&e.data);
        }
    }
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// Appends one point block (the `.prp` column encoding) — also used by the
/// operation journal for ink carried in operations.
pub fn encode_point_block(out: &mut Vec<u8>, points: &[InkPoint]) {
    put_points(out, points);
}

/// Decodes one point block starting at `*pos` and advances `*pos`.
pub fn decode_point_block(bytes: &[u8], pos: &mut usize) -> Result<Vec<InkPoint>, BinaryError> {
    let mut r = Reader { b: bytes, i: *pos };
    let pts = r.points()?;
    *pos = r.i;
    Ok(pts)
}

// ------------------------------------------------------------- decode --

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], BinaryError> {
        let end = self
            .i
            .checked_add(n)
            .ok_or(BinaryError::Malformed("length overflow"))?;
        let s = self
            .b
            .get(self.i..end)
            .ok_or(BinaryError::Malformed("unexpected end of data"))?;
        self.i = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, BinaryError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, BinaryError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, BinaryError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u64, BinaryError> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = self.u8()?;
            if shift == 63 && byte > 1 {
                return Err(BinaryError::Malformed("varint overflow"));
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(BinaryError::Malformed("varint too long"))
    }

    fn float_column(&mut self, n: usize) -> Result<Vec<f64>, BinaryError> {
        match self.u8()? {
            0 => Ok(vec![self.f64()?; n]),
            1 => {
                let k = u32::from(self.u8()?);
                if k > MAX_DECIMALS {
                    return Err(BinaryError::Malformed("decimal scale out of range"));
                }
                let s = pow10(k);
                let mut q = 0i64;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    q = q.wrapping_add(unzigzag(self.varint()?));
                    out.push(q as f64 / s);
                }
                Ok(out)
            }
            2 => {
                // check the length up front so hostile counts can't allocate wildly
                let bytes = self.take(
                    n.checked_mul(8)
                        .ok_or(BinaryError::Malformed("length overflow"))?,
                )?;
                Ok(bytes
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| f64::from_le_bytes(*c))
                    .collect())
            }
            _ => Err(BinaryError::Malformed("unknown float column encoding")),
        }
    }

    fn int_column(&mut self, n: usize) -> Result<Vec<i64>, BinaryError> {
        match self.u8()? {
            0 => Ok(vec![unzigzag(self.varint()?); n]),
            1 => {
                let mut v = 0i64;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    v = v.wrapping_add(unzigzag(self.varint()?));
                    out.push(v);
                }
                Ok(out)
            }
            _ => Err(BinaryError::Malformed("unknown int column encoding")),
        }
    }

    fn presence(&mut self, n: usize) -> Result<Vec<bool>, BinaryError> {
        match self.u8()? {
            0 => Ok(vec![false; n]),
            1 => Ok(vec![true; n]),
            2 => {
                let bits = self.take(n.div_ceil(8))?;
                Ok((0..n).map(|i| bits[i / 8] & (1 << (i % 8)) != 0).collect())
            }
            _ => Err(BinaryError::Malformed("unknown presence encoding")),
        }
    }

    fn extensions(&mut self) -> Result<Vec<BinaryExtension>, BinaryError> {
        let count = self.u32()? as usize;
        if count > MAX_EXTENSIONS {
            return Err(BinaryError::Malformed("too many extension records"));
        }
        let mut out = Vec::new();
        let mut total = 0usize;
        for _ in 0..count {
            let tag = self.u32()?;
            let len = self.u32()? as usize;
            total = total.saturating_add(len);
            if total > MAX_EXTENSION_BYTES {
                return Err(BinaryError::Malformed("extension area too large"));
            }
            let data = self.take(len)?.to_vec();
            let ext = BinaryExtension { tag, data };
            // no extension tags are defined in this build
            if ext.required() {
                return Err(BinaryError::UnsupportedFeature(format!(
                    "extension {tag:#010x}"
                )));
            }
            out.push(ext);
        }
        Ok(out)
    }

    fn points(&mut self) -> Result<Vec<InkPoint>, BinaryError> {
        let n = self.varint()? as usize;
        if n > limits::MAX_POINTS_PER_STROKE {
            return Err(BinaryError::Malformed("too many points in a stroke"));
        }
        // every point needs at least one byte per varint column; reject impossible counts early
        if n > self.b.len().saturating_sub(self.i) {
            return Err(BinaryError::Malformed("point count exceeds data"));
        }
        let x = self.float_column(n)?;
        let y = self.float_column(n)?;
        let p = self.float_column(n)?;
        let tx = self.float_column(n)?;
        let ty = self.float_column(n)?;
        let t = self.int_column(n)?;
        let tw_p = self.presence(n)?;
        let tw_n = tw_p.iter().filter(|b| **b).count();
        let tw = if tw_n > 0 {
            self.float_column(tw_n)?
        } else {
            Vec::new()
        };
        let fl_p = self.presence(n)?;
        let fl_n = fl_p.iter().filter(|b| **b).count();
        let fl = if fl_n > 0 {
            self.int_column(fl_n)?
        } else {
            Vec::new()
        };
        let (mut ti, mut fi) = (0, 0);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let twist = if tw_p[i] {
                ti += 1;
                Some(tw[ti - 1])
            } else {
                None
            };
            let flags = if fl_p[i] {
                fi += 1;
                Some(
                    u32::try_from(fl[fi - 1])
                        .map_err(|_| BinaryError::Malformed("flags out of range"))?,
                )
            } else {
                None
            };
            out.push(InkPoint {
                x: x[i],
                y: y[i],
                pressure: p[i],
                tilt_x: tx[i],
                tilt_y: ty[i],
                time_us: t[i],
                twist,
                flags,
            });
        }
        Ok(out)
    }
}

/// Decodes a page. The metadata section goes through the regular schema
/// migration chain; the result still needs `validate_page`.
pub fn decode_page(bytes: &[u8]) -> Result<Page, BinaryError> {
    if !is_binary_page(bytes) {
        return Err(BinaryError::BadMagic);
    }
    if bytes.len() < 20 {
        return Err(BinaryError::Malformed("too short"));
    }
    let (body, crc) = bytes.split_at(bytes.len() - 4);
    if crc32fast::hash(body) != u32::from_le_bytes(crc.try_into().unwrap()) {
        return Err(BinaryError::Checksum);
    }
    let mut r = Reader { b: body, i: 4 };
    let version = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    if version > FILE_FORMAT_VERSION {
        return Err(BinaryError::TooNew(version));
    }
    if version == 0 {
        return Err(BinaryError::Malformed("version 0"));
    }
    let flags = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    let unknown_required = flags & REQUIRED_FLAGS & !KNOWN_REQUIRED;
    if unknown_required != 0 {
        return Err(BinaryError::UnsupportedFeature(format!(
            "flags {unknown_required:#06x}"
        )));
    }
    let meta_len = r.u32()? as usize;
    let stored = r.take(meta_len)?;
    let inflated;
    let meta: &[u8] = if flags & FLAG_META_ZLIB != 0 {
        use std::io::Read;
        let mut buf = Vec::new();
        flate2::read::ZlibDecoder::new(stored)
            .take(MAX_META + 1)
            .read_to_end(&mut buf)
            .map_err(|_| BinaryError::Malformed("metadata does not decompress"))?;
        if buf.len() as u64 > MAX_META {
            return Err(BinaryError::Malformed("metadata too large"));
        }
        inflated = buf;
        &inflated
    } else {
        stored
    };
    let mut page =
        crate::migrate::page_from_slice(meta).map_err(|e| BinaryError::Meta(e.to_string()))?;
    let count = r.u32()? as usize;
    let inks: Vec<&mut InkStroke> = page
        .elements
        .iter_mut()
        .filter_map(|e| {
            if let Element::Ink(s) = e {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    if inks.len() != count {
        return Err(BinaryError::Malformed(
            "stroke count does not match metadata",
        ));
    }
    for s in inks {
        s.points = r.points()?;
    }
    if flags & FLAG_EXTENSIONS != 0 {
        page.binary_extensions = r.extensions()?;
    }
    if r.i != body.len() {
        return Err(BinaryError::Malformed("trailing bytes"));
    }
    Ok(page)
}
