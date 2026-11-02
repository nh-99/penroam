//! `.prp` file format v1 contract: pinned golden, forward-compatibility
//! rules (required vs optional features, extension preservation, read-only
//! mode), version separation and a deterministic fuzz smoke test.
//! Long-running coverage-guided fuzzing lives in `fuzz/` (scheduled CI).

use penroam_core::binary::*;
use penroam_core::migrate::{m00_strokes_to_elements, notebook_from_value, page_from_value};
use penroam_core::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;

fn fixture_path(p: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(p)
}

fn sha256(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn sample_page() -> Page {
    let v: Value =
        serde_json::from_slice(&std::fs::read(fixture_path("sample-strokes.json")).unwrap())
            .unwrap();
    let mut els = m00_strokes_to_elements(&v, 1).unwrap();
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

fn recrc(b: &mut [u8]) {
    let body = b.len() - 4;
    let crc = crc32fast::hash(&b[..body]);
    b[body..].copy_from_slice(&crc.to_le_bytes());
}

/// SHA-256 of `tests/fixtures/format/sample-page-v1.prp`, written by the M01
/// encoder. The M02 encoder must reproduce it byte for byte.
const GOLDEN_SHA256: &str = "05716d7d5cd66973bc8524da9a3055501dfd198087778973afc984a2a7f8da64";

#[test]
fn golden_is_hash_pinned_and_reproduced() {
    let golden = std::fs::read(fixture_path("format/sample-page-v1.prp")).unwrap();
    assert_eq!(sha256(&golden), GOLDEN_SHA256, "golden file was modified");
    assert_eq!(encode_page(&sample_page()), golden);
    assert_eq!(decode_page(&golden).unwrap(), sample_page());
}

#[test]
fn versions_are_independent() {
    assert_eq!(FILE_FORMAT_VERSION, 1);
    assert_eq!(PAGE_SCHEMA_VERSION, 1);
    assert_eq!(NOTEBOOK_SCHEMA_VERSION, 2);
    assert_eq!(operation::OP_SCHEMA_VERSION, 1);
    // a page whose *model* schema is newer is refused even inside a v1 file
    let mut p = sample_page();
    p.schema_version = 2;
    let err = decode_page(&encode_page(&p)).unwrap_err();
    assert!(
        matches!(err, BinaryError::Meta(ref m) if m.contains("newer")),
        "{err}"
    );
}

fn with_extensions(exts: Vec<BinaryExtension>) -> Vec<u8> {
    let mut p = sample_page();
    p.binary_extensions = exts;
    encode_page(&p)
}

#[test]
fn unknown_copy_safe_extension_is_preserved_on_resave() {
    let ext = BinaryExtension {
        tag: BinaryExtension::COPY_SAFE | 0x77,
        data: b"future optional data".to_vec(),
    };
    let bytes = with_extensions(vec![ext.clone()]);
    assert_eq!(
        u16::from_le_bytes([bytes[6], bytes[7]]) & FLAG_EXTENSIONS,
        FLAG_EXTENSIONS
    );
    let mut page = decode_page(&bytes).unwrap();
    assert_eq!(page.binary_extensions, vec![ext.clone()]);
    assert!(page.read_only_reason().is_none());
    // an edit + re-save keeps it verbatim
    page.elements.pop();
    let again = decode_page(&encode_page(&page)).unwrap();
    assert_eq!(again.binary_extensions, vec![ext]);
}

#[test]
fn unknown_non_copy_safe_extension_makes_the_page_read_only() {
    let ext = BinaryExtension {
        tag: 0x1234,
        data: vec![1, 2, 3],
    };
    let bytes = with_extensions(vec![ext.clone()]);
    let page = decode_page(&bytes).unwrap();
    assert!(page.read_only_reason().is_some());
    // unchanged re-save is byte-identical (nothing stripped)
    assert_eq!(encode_page(&page), bytes);
}

#[test]
fn unknown_required_extension_or_flag_is_refused() {
    let bytes = with_extensions(vec![BinaryExtension {
        tag: BinaryExtension::REQUIRED | 5,
        data: vec![],
    }]);
    assert!(matches!(
        decode_page(&bytes),
        Err(BinaryError::UnsupportedFeature(_))
    ));
    // required flag bit 3
    let mut b = encode_page(&sample_page());
    b[6] |= 1 << 3;
    recrc(&mut b);
    assert!(matches!(
        decode_page(&b),
        Err(BinaryError::UnsupportedFeature(_))
    ));
    // optional flag bit 12 is ignored
    let mut b = encode_page(&sample_page());
    b[7] |= 1 << 4;
    recrc(&mut b);
    assert_eq!(decode_page(&b).unwrap(), sample_page());
}

#[test]
fn extension_area_limits() {
    let mut b = encode_page(&sample_page());
    b.truncate(b.len() - 4);
    b[7] |= 1; // FLAG_EXTENSIONS
    b.extend_from_slice(&(MAX_EXTENSIONS as u32 + 1).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    recrc(&mut b);
    assert!(matches!(decode_page(&b), Err(BinaryError::Malformed(_))));
    // a record longer than the file
    let mut b = encode_page(&sample_page());
    b.truncate(b.len() - 4);
    b[7] |= 1;
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&7u32.to_le_bytes());
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    recrc(&mut b);
    assert!(matches!(decode_page(&b), Err(BinaryError::Malformed(_))));
}

#[test]
fn notebook_v1_migrates_to_v2_with_order_keys() {
    let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
    let v1 = json!({
        "schemaVersion": 1, "id": Uuid::from_u128(99), "title": "Physics",
        "createdAt": 1, "updatedAt": 2,
        "pageOrder": [ids[3], ids[0], ids[2]],
        "trashedPages": [{"id": ids[1], "deletedAt": 5, "formerIndex": 1},
                          {"id": ids[4], "deletedAt": 6, "formerIndex": 9}],
        "defaults": {"paper": "letter", "template": {"kind": "ruled", "spacing": 24}},
        "cover": "#3b5b7a", "assets": [], "futureField": {"kept": true}
    });
    let nb = notebook_from_value(v1).unwrap();
    assert_eq!(nb.schema_version, 2);
    assert_eq!(
        nb.page_order,
        vec![ids[3], ids[0], ids[2]],
        "order unchanged"
    );
    assert!(nb.order_is_consistent());
    assert_eq!(nb.order_keys.len(), 5);
    // trashed page 1 sits between its former neighbours
    let k = |i: usize| nb.key_of(ids[i]).unwrap().to_string();
    assert!(k(3) < k(1) && k(1) < k(0));
    assert!(k(2) < k(4), "former index past the end goes last");
    assert_eq!(
        nb.extra["futureField"],
        json!({"kept": true}),
        "unknown fields survive"
    );
    // v2 re-reads identically; v3 is refused
    let v2 = serde_json::to_value(&nb).unwrap();
    assert_eq!(notebook_from_value(v2.clone()).unwrap(), nb);
    let mut v3 = v2;
    v3["schemaVersion"] = json!(3);
    assert!(notebook_from_value(v3).is_err());
}

#[test]
fn page_schema_has_no_migrations_yet_and_refuses_newer() {
    let v = serde_json::to_value(sample_page()).unwrap();
    assert_eq!(page_from_value(v.clone()).unwrap(), sample_page());
    let mut v2 = v;
    v2["schemaVersion"] = json!(2);
    assert!(page_from_value(v2).is_err());
}

// ------------------------------------------------------------------ fuzz --

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn f64(&mut self) -> f64 {
        match self.below(6) {
            0 => f64::from_bits(self.next()), // any bit pattern, incl. NaN/inf/-0
            1 => -0.0,
            2 => (self.below(2_000_000) as f64) / 1000.0,
            3 => (self.below(10_000) as f64) / 10_000.0,
            4 => 1e300,
            _ => self.below(100) as f64,
        }
    }
}

fn bits(p: &Page) -> Vec<u64> {
    p.elements
        .iter()
        .flat_map(|e| match e {
            Element::Ink(s) => s
                .points
                .iter()
                .flat_map(|q| {
                    [
                        q.x.to_bits(),
                        q.y.to_bits(),
                        q.pressure.to_bits(),
                        q.tilt_x.to_bits(),
                        q.tilt_y.to_bits(),
                        q.time_us as u64,
                        q.twist.map_or(1, f64::to_bits),
                        q.flags.map_or(u64::MAX, u64::from),
                    ]
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect()
}

/// Random strokes with arbitrary f64 bit patterns round-trip bit for bit.
#[test]
fn fuzz_roundtrip_is_bit_exact() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for _ in 0..300 {
        let mut page = sample_page();
        page.elements.truncate(rng.below(3));
        for i in 0..rng.below(5) {
            let n = 1 + rng.below(40);
            let mut points = Vec::new();
            for _ in 0..n {
                points.push(InkPoint {
                    x: rng.f64(),
                    y: rng.f64(),
                    pressure: rng.f64(),
                    tilt_x: rng.f64(),
                    tilt_y: rng.f64(),
                    time_us: rng.next() as i64 >> rng.below(64),
                    twist: (rng.below(2) == 0).then(|| rng.f64()),
                    flags: (rng.below(3) == 0).then(|| rng.next() as u32),
                });
            }
            page.elements.push(Element::Ink(InkStroke {
                id: Uuid::from_u128(1000 + i as u128),
                z: i as i64,
                tool: InkTool::Pen,
                color: "#000000".into(),
                opacity: 1.0,
                width: 2.0,
                pressure: true,
                source: "fuzz".into(),
                input: Default::default(),
                points,
                extra: Default::default(),
            }));
        }
        let bytes = encode_page(&page);
        let back = decode_page(&bytes).unwrap();
        assert_eq!(bits(&back), bits(&page));
        assert_eq!(encode_page(&back), bytes, "re-encoding is deterministic");
    }
}

/// Mutated and random inputs never panic, hang or allocate wildly; they
/// either decode or return an error. (Checksums are recomputed after the
/// mutation so the structural parser is exercised, not just the CRC.)
#[test]
fn fuzz_mutations_never_panic() {
    let golden = std::fs::read(fixture_path("format/sample-page-v1.prp")).unwrap();
    let small = {
        let mut p = sample_page();
        p.elements.truncate(2);
        p.binary_extensions.push(BinaryExtension {
            tag: BinaryExtension::COPY_SAFE | 1,
            data: vec![9; 5],
        });
        encode_page(&p)
    };
    let mut rng = Rng(42);
    for iter in 0..20_000 {
        let base = if iter % 2 == 0 { &golden } else { &small };
        let mut b = base.clone();
        match rng.below(5) {
            0 => {
                for _ in 0..1 + rng.below(4) {
                    let i = rng.below(b.len());
                    b[i] = rng.next() as u8;
                }
            }
            1 => b.truncate(rng.below(b.len())),
            2 => {
                let i = rng.below(b.len());
                let n = rng.below(16);
                for _ in 0..n {
                    b.insert(i, rng.next() as u8);
                }
            }
            3 => {
                // huge length / count fields
                let i = 8 + rng.below(b.len().saturating_sub(12).max(1));
                let v = [u32::MAX, 1 << 31, 1 << 24, 0][rng.below(4)];
                if i + 4 <= b.len() {
                    b[i..i + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            _ => {
                b = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
                if rng.below(2) == 0 {
                    b.splice(0..0, MAGIC.iter().copied().chain([1, 0, 1, 0]));
                }
            }
        }
        if b.len() >= 4 && rng.below(4) != 0 {
            recrc(&mut b);
        }
        let _ = decode_page(&b);
    }
}
