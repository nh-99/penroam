use penroam_core::binary::{decode_page, encode_page, BinaryError};
use penroam_core::migrate::stroke_doc_to_elements;
use penroam_core::*;
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

fn fixture_path(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(p)
}

/// Deterministic page from a canonical stroke document (fixed ids/timestamps).
fn page_from_stroke_doc(file: &str) -> Page {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path(file)).unwrap()).unwrap();
    let mut els = stroke_doc_to_elements(&v, 1).unwrap();
    for (i, e) in els.iter_mut().enumerate() {
        if let Element::Ink(s) = e {
            s.id = Uuid::from_u128(i as u128 + 1);
        }
    }
    let mut p = Page::new(
        Uuid::from_u128(0xabc),
        Paper::Custom,
        Size {
            width: 1600.0,
            height: 1200.0,
        },
        Template::default(),
        1_793_600_000_000,
    );
    p.elements = els;
    p
}

fn text(id: u128, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::from_u128(id),
        z: 1000,
        x: 10.5,
        y: 20.25,
        width: 200.0,
        height: 40.0,
        text: s.into(),
        font_size: 14.0,
        color: "#202020".into(),
        align: TextAlign::Center,
        row_pitch: None,
        extra: Default::default(),
    })
}

#[test]
fn round_trip_is_exact_for_fixtures_and_scenes() {
    for f in [
        "sample-strokes.json",
        "benchmark-scenes/scene-100.json",
        "benchmark-scenes/scene-1000.json",
    ] {
        let mut p = page_from_stroke_doc(f);
        p.elements.push(text(9999, "typed ✓\nsecond line"));
        let bytes = encode_page(&p);
        let back = decode_page(&bytes).unwrap();
        assert_eq!(back, p, "{f}");
        assert_eq!(encode_page(&back), bytes, "{f}: encoding is deterministic");
    }
}

#[test]
fn awkward_values_round_trip_bit_for_bit() {
    let mut p = page_from_stroke_doc("sample-strokes.json");
    let Element::Ink(s) = &mut p.elements[3] else {
        panic!()
    };
    // float32-derived values (native SDKs), negative zero, huge/tiny numbers,
    // mixed optional twist/flags, timestamps going backwards
    let vals = [
        123.456_001_281_738_28,
        -0.0,
        1e-300,
        999_999.999_999,
        0.1 + 0.2,
        7.0,
        -3.125,
    ];
    s.points = vals
        .iter()
        .enumerate()
        .map(|(i, v)| InkPoint {
            x: *v,
            y: -*v,
            pressure: (i as f64) / 7.0,
            tilt_x: if i % 2 == 0 { 12.5 } else { -0.0 },
            tilt_y: 45.0,
            time_us: [0, 4000, 3999, 1 << 40, -5, 7, 8][i],
            twist: if i % 3 == 0 {
                Some(i as f64 * 1.1)
            } else {
                None
            },
            flags: if i == 2 {
                Some(u32::MAX)
            } else if i == 4 {
                Some(2)
            } else {
                None
            },
        })
        .collect();
    let orig = s.points.clone();
    let back = decode_page(&encode_page(&p)).unwrap();
    let Element::Ink(b) = &back.elements[3] else {
        panic!()
    };
    for (a, c) in orig.iter().zip(&b.points) {
        for (u, v) in [
            (a.x, c.x),
            (a.y, c.y),
            (a.pressure, c.pressure),
            (a.tilt_x, c.tilt_x),
            (a.tilt_y, c.tilt_y),
        ] {
            assert_eq!(u.to_bits(), v.to_bits());
        }
        assert_eq!(a.time_us, c.time_us);
        assert_eq!(a.twist.map(f64::to_bits), c.twist.map(f64::to_bits));
        assert_eq!(a.flags, c.flags);
    }
    assert_eq!(back, p);
}

#[test]
fn unknown_fields_and_metadata_survive() {
    let mut p = page_from_stroke_doc("sample-strokes.json");
    p.extra
        .insert("futurePageField".into(), serde_json::json!({"a": [1, 2]}));
    if let Element::Ink(s) = &mut p.elements[0] {
        s.extra
            .insert("futureStrokeField".into(), Value::Bool(true));
        s.input
            .insert("maxTouchPressure".into(), serde_json::json!(4095));
    }
    assert_eq!(decode_page(&encode_page(&p)).unwrap(), p);
}

#[test]
fn compact_compared_with_json() {
    let p = page_from_stroke_doc("benchmark-scenes/scene-1000.json");
    let json = serde_json::to_vec(&p).unwrap();
    let bin = encode_page(&p);
    let points: usize = p
        .elements
        .iter()
        .map(|e| {
            if let Element::Ink(s) = e {
                s.points.len()
            } else {
                0
            }
        })
        .sum();
    eprintln!(
        "scene-1000: json {} B, prp {} B ({:.1} B/point)",
        json.len(),
        bin.len(),
        bin.len() as f64 / points as f64
    );
    assert!(
        bin.len() * 6 < json.len(),
        "binary should be far smaller than JSON"
    );
}

#[test]
fn corruption_is_detected_never_panics() {
    let p = page_from_stroke_doc("sample-strokes.json");
    let bytes = encode_page(&p);
    // every single-byte change is caught (checksum or structure)
    for i in 0..bytes.len() {
        let mut b = bytes.clone();
        b[i] ^= 0x5a;
        assert!(decode_page(&b).is_err(), "byte {i}");
    }
    // every truncation is caught
    for n in 0..bytes.len() {
        assert!(decode_page(&bytes[..n]).is_err(), "truncated to {n}");
    }
    assert_eq!(
        decode_page(b"{\"json\":true}").unwrap_err(),
        BinaryError::BadMagic
    );
    // a newer version is refused, not misread
    let mut b = bytes.clone();
    b[4] = 9;
    let body = b.len() - 4;
    let crc = crc32fast::hash(&b[..body]);
    b[body..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(decode_page(&b).unwrap_err(), BinaryError::TooNew(9));
}

#[test]
fn hostile_counts_are_rejected_without_huge_allocations() {
    // valid header + meta for one stroke, then a point count of 2^40 with a valid checksum
    let mut p = page_from_stroke_doc("sample-strokes.json");
    p.elements.truncate(1);
    let good = encode_page(&p);
    let meta_len = u32::from_le_bytes(good[8..12].try_into().unwrap()) as usize;
    let mut b = good[..12 + meta_len + 4].to_vec();
    b.extend_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x20]); // varint 2^40
    let crc = crc32fast::hash(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    assert!(matches!(decode_page(&b), Err(BinaryError::Malformed(_))));
}

/// Freezes the v1 byte layout: if this fails, the format changed — bump VERSION
/// and add a migration instead of editing the golden file.
#[test]
fn golden_file_is_stable() {
    let p = page_from_stroke_doc("sample-strokes.json");
    let bytes = encode_page(&p);
    let path = fixture_path("format/sample-page-v1.prp");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
    }
    let golden = std::fs::read(&path).unwrap();
    assert_eq!(
        decode_page(&golden).unwrap(),
        p,
        "golden decodes to the same page"
    );
    assert_eq!(golden, bytes, "encoder output changed");
}
