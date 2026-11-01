//! Writes tests/fixtures/pdf/mixed-sizes-rotations.pdf: Letter, A4 (with a
//! crop box), a landscape page rotated 90, a page rotated 180 and one rotated
//! 270. Each page shows its label and an arrow pointing to the page's
//! *unrotated* top, so rotation handling is visible in renders.
//!
//!     cargo run -p penroam-pdf --example make_fixture_pdf
use lopdf::{Dictionary, Document, Object, Stream};

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn main() {
    let pages: &[(&str, f32, f32, i64, Option<[f32; 4]>)] = &[
        ("1 Letter portrait", 612.0, 792.0, 0, None),
        (
            "2 A4 cropped",
            595.0,
            842.0,
            0,
            Some([30.0, 40.0, 565.0, 800.0]),
        ),
        ("3 Landscape Rotate 90", 792.0, 612.0, 90, None),
        ("4 Rotate 180", 420.0, 595.0, 180, None),
        ("5 Rotate 270", 500.0, 360.0, 270, None),
    ];
    let mut doc = Document::with_version("1.5");
    let root = doc.new_object_id();
    let font = doc.add_object(Dictionary::from_iter(vec![
        ("Type", name("Font")),
        ("Subtype", name("Type1")),
        ("BaseFont", name("Helvetica-Bold")),
    ]));
    let mut kids = vec![];
    for (label, w, h, rot, crop) in pages {
        let (cx, cy) = (w / 2.0, h / 2.0);
        let content = format!(
            "q 0.85 0.9 1 rg 0 0 {w} {h} re f Q \
             q 0.2 0.3 0.6 RG 4 w 12 12 {} {} re S Q \
             q 0.8 0.1 0.1 rg {cx} {} m {} {} l {} {} l f Q \
             BT /F1 28 Tf 40 {} Td ({label}) Tj ET \
             BT /F1 14 Tf 40 {} Td (Arrow points to the unrotated top) Tj ET",
            w - 24.0,
            h - 24.0,
            h - 30.0,
            cx - 30.0,
            h - 90.0,
            cx + 30.0,
            h - 90.0,
            cy,
            cy - 30.0
        );
        let c = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let mut d = Dictionary::from_iter(vec![
            ("Type", name("Page")),
            ("Parent", Object::Reference(root)),
            ("Contents", Object::Reference(c)),
            (
                "MediaBox",
                Object::Array(vec![0.into(), 0.into(), Object::Real(*w), Object::Real(*h)]),
            ),
            ("Rotate", Object::Integer(*rot)),
            (
                "Resources",
                Object::Dictionary(Dictionary::from_iter(vec![(
                    "Font",
                    Object::Dictionary(Dictionary::from_iter(vec![(
                        "F1",
                        Object::Reference(font),
                    )])),
                )])),
            ),
        ]);
        if let Some(cb) = crop {
            d.set(
                "CropBox",
                Object::Array(cb.iter().map(|v| Object::Real(*v)).collect()),
            );
        }
        kids.push(Object::Reference(doc.add_object(d)));
    }
    doc.objects.insert(
        root,
        Object::Dictionary(Dictionary::from_iter(vec![
            ("Type", name("Pages")),
            ("Count", Object::Integer(kids.len() as i64)),
            ("Kids", Object::Array(kids)),
        ])),
    );
    let cat = doc.add_object(Dictionary::from_iter(vec![
        ("Type", name("Catalog")),
        ("Pages", Object::Reference(root)),
    ]));
    doc.trailer.set("Root", Object::Reference(cat));
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pdf/mixed-sizes-rotations.pdf");
    doc.save(&out).expect("write pdf");
    println!("wrote {}", out.display());
}
