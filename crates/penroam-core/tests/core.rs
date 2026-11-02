use penroam_core::geometry::{stroke_pieces, width_for, Piece};
use penroam_core::migrate::{
    elements_to_m00, m00_strokes_to_elements, page_from_slice, page_from_value,
};
use penroam_core::ops::*;
use penroam_core::*;
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

fn fixture(p: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(p);
    std::fs::read_to_string(&root).unwrap_or_else(|e| panic!("{}: {e}", root.display()))
}

fn sample_elements() -> Vec<Element> {
    let doc: Value = serde_json::from_str(&fixture("sample-strokes.json")).unwrap();
    m00_strokes_to_elements(&doc, 1).unwrap()
}

fn ink(e: &Element) -> &InkStroke {
    match e {
        Element::Ink(s) => s,
        _ => panic!("not ink"),
    }
}

#[test]
fn geometry_matches_m00_golden() {
    let golden: Value = serde_json::from_str(&fixture("geometry-golden.json")).unwrap();
    for c in golden["widthCases"].as_array().unwrap() {
        let w = width_for(
            c["base"].as_f64().unwrap(),
            c["pressure"].as_f64().unwrap(),
            c["pressureEnabled"].as_bool().unwrap(),
        );
        assert!((w - c["width"].as_f64().unwrap()).abs() < 1e-12);
    }
    let els = sample_elements();
    for g in golden["strokes"].as_array().unwrap() {
        let id = g["id"].as_str().unwrap();
        let s = els
            .iter()
            .map(ink)
            .find(|s| s.input["m00Id"] == id)
            .unwrap();
        let pieces = stroke_pieces(&s.points, s.width, s.pressure);
        let gp = g["pieces"].as_array().unwrap();
        assert_eq!(pieces.len(), gp.len(), "{id}");
        for (p, e) in pieces.iter().zip(gp) {
            let close =
                |k: &str, v: f64| assert!((e[k].as_f64().unwrap() - v).abs() < 1e-9, "{id} {k}");
            match *p {
                Piece::Dot { x, y, width } => {
                    assert_eq!(e["kind"], "dot");
                    close("x", x);
                    close("y", y);
                    close("width", width);
                }
                Piece::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    width,
                } => {
                    assert_eq!(e["kind"], "line");
                    for (k, v) in [
                        ("x0", x0),
                        ("y0", y0),
                        ("x1", x1),
                        ("y1", y1),
                        ("width", width),
                    ] {
                        close(k, v);
                    }
                }
                Piece::Quad {
                    x0,
                    y0,
                    cx,
                    cy,
                    x1,
                    y1,
                    width,
                } => {
                    assert_eq!(e["kind"], "quad");
                    for (k, v) in [
                        ("x0", x0),
                        ("y0", y0),
                        ("cx", cx),
                        ("cy", cy),
                        ("x1", x1),
                        ("y1", y1),
                        ("width", width),
                    ] {
                        close(k, v);
                    }
                }
            }
        }
    }
}

#[test]
fn m00_import_is_lossless() {
    let doc: Value = serde_json::from_str(&fixture("sample-strokes.json")).unwrap();
    let els = sample_elements();
    let strokes: Vec<&InkStroke> = els.iter().map(ink).collect();
    let back = elements_to_m00(&strokes);
    let orig = doc["strokes"].as_array().unwrap();
    let back = back["strokes"].as_array().unwrap();
    assert_eq!(orig.len(), back.len());
    for (o, b) in orig.iter().zip(back) {
        assert_eq!(o["source"], b["source"]);
        assert_eq!(o["brush"]["width"].as_f64(), b["brush"]["width"].as_f64());
        assert_eq!(o["brush"]["pressure"], b["brush"]["pressure"]);
        for (k, v) in o["input"].as_object().unwrap() {
            assert_eq!(&b["input"][k], v, "input.{k}");
        }
        let (op, bp) = (
            o["points"].as_array().unwrap(),
            b["points"].as_array().unwrap(),
        );
        assert_eq!(op.len(), bp.len());
        for (p, q) in op.iter().zip(bp) {
            for k in ["x", "y", "pressure", "tiltX", "tiltY"] {
                assert_eq!(p[k].as_f64(), q[k].as_f64(), "{k}");
            }
            assert_eq!(p["timeUs"].as_i64(), q["timeUs"].as_i64());
        }
    }
}

fn page_with(elements: Vec<Element>) -> Page {
    let mut p = Page::new(
        Uuid::new_v4(),
        Paper::Letter,
        Paper::Letter.size().unwrap(),
        Template::default(),
        1,
    );
    p.elements = elements;
    p
}

fn text(id: Uuid, z: i64, s: &str) -> Element {
    Element::Text(TextBlock {
        id,
        z,
        x: 72.0,
        y: 72.0,
        width: 200.0,
        height: 20.0,
        text: s.into(),
        font_size: 14.0,
        color: "#202020".into(),
        align: TextAlign::Left,
        extra: Default::default(),
    })
}

fn batch(base: u64, ops: Vec<OpKind>) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: ops
            .into_iter()
            .map(|kind| PageOperation {
                op_id: Uuid::new_v4(),
                kind,
            })
            .collect(),
    }
}

#[test]
fn page_serialization_round_trips_and_keeps_unknown_fields() {
    let mut p = page_with(sample_elements());
    p.elements.push(text(Uuid::new_v4(), 99, "Hello\nworld"));
    let mut v = serde_json::to_value(&p).unwrap();
    v["futureField"] = json!({"a": 1});
    v["elements"][0]["futureStrokeField"] = json!(true);
    let back = page_from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
    // byte-level: re-serialising is stable
    let s1 = serde_json::to_vec(&back).unwrap();
    let s2 = serde_json::to_vec(&page_from_slice(&s1).unwrap()).unwrap();
    assert_eq!(s1, s2);
}

#[test]
fn schema_version_guard() {
    let p = page_with(vec![]);
    let mut v = serde_json::to_value(&p).unwrap();
    v["schemaVersion"] = json!(2);
    assert!(matches!(
        page_from_value(v.clone()),
        Err(migrate::MigrateError::TooNew { found: 2, .. })
    ));
    v.as_object_mut().unwrap().remove("schemaVersion");
    assert!(page_from_value(v).is_err());
}

#[test]
fn operations_apply_in_order_with_stable_ids_and_are_atomic() {
    let mut p = page_with(vec![]);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let rev = apply_batch(
        &mut p,
        &batch(
            0,
            vec![OpKind::AddElements {
                elements: vec![text(a, 1, "a"), text(b, 2, "b")],
            }],
        ),
        5,
    )
    .unwrap();
    assert_eq!(rev, 1);
    assert_eq!(
        p.elements.iter().map(Element::id).collect::<Vec<_>>(),
        vec![a, b]
    );

    // stale base revision
    assert_eq!(
        apply_batch(
            &mut p,
            &batch(0, vec![OpKind::RemoveElements { ids: vec![a] }]),
            6
        ),
        Err(ApplyError::RevisionConflict {
            expected: 0,
            actual: 1
        })
    );
    // second op fails -> first op must not be applied
    let before = p.clone();
    let r = apply_batch(
        &mut p,
        &batch(
            1,
            vec![
                OpKind::RemoveElements { ids: vec![a] },
                OpKind::RemoveElements {
                    ids: vec![Uuid::new_v4()],
                },
            ],
        ),
        7,
    );
    assert!(matches!(r, Err(ApplyError::MissingElement(_))));
    assert_eq!(p, before);
    // duplicates refused
    assert!(matches!(
        apply_batch(
            &mut p,
            &batch(
                1,
                vec![OpKind::AddElements {
                    elements: vec![text(a, 3, "dup")]
                }]
            ),
            8
        ),
        Err(ApplyError::DuplicateElement(_))
    ));
    // replace + z reorder
    apply_batch(
        &mut p,
        &batch(
            1,
            vec![OpKind::ReplaceElements {
                elements: vec![text(a, 10, "a2")],
            }],
        ),
        9,
    )
    .unwrap();
    assert_eq!(
        p.elements.iter().map(Element::id).collect::<Vec<_>>(),
        vec![b, a]
    );
    // invalid data refused
    let mut bad = text(Uuid::new_v4(), 1, "x");
    if let Element::Text(t) = &mut bad {
        t.color = "red".into();
    }
    assert!(matches!(
        apply_batch(
            &mut p,
            &batch(
                2,
                vec![OpKind::AddElements {
                    elements: vec![bad]
                }]
            ),
            10
        ),
        Err(ApplyError::Invalid(_))
    ));
    assert_eq!(p.revision, 2);
}

#[test]
fn undo_by_inverse_batch_restores_exact_state() {
    let mut p = page_with(sample_elements());
    let original = p.elements.clone();
    let victim = original[2].clone();
    apply_batch(
        &mut p,
        &batch(
            0,
            vec![OpKind::RemoveElements {
                ids: vec![victim.id()],
            }],
        ),
        1,
    )
    .unwrap();
    apply_batch(
        &mut p,
        &batch(
            1,
            vec![OpKind::AddElements {
                elements: vec![victim],
            }],
        ),
        2,
    )
    .unwrap();
    assert_eq!(p.elements, original);
}

#[test]
fn pdf_matrix_maps_display_corners() {
    let apply =
        |m: [f64; 6], x: f64, y: f64| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
    let crop = [10.0, 20.0, 210.0, 320.0]; // 200 x 300
    for (rot, (w, h), expect_tl) in [
        (0, (200.0, 300.0), (10.0, 320.0)),
        (90, (300.0, 200.0), (10.0, 20.0)),
        (180, (200.0, 300.0), (210.0, 20.0)),
        (270, (300.0, 200.0), (210.0, 320.0)),
    ] {
        let bg = PdfBackgroundRef {
            asset_hash: "0".repeat(64),
            page_index: 0,
            media_box: crop,
            crop_box: crop,
            rotation: rot,
        };
        assert_eq!((bg.display_size().width, bg.display_size().height), (w, h));
        let m = bg.doc_to_pdf_matrix();
        assert_eq!(apply(m, 0.0, 0.0), expect_tl, "rot {rot}");
        // every display corner lands on a crop box corner
        for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
            let (px, py) = apply(m, x, y);
            assert!(
                [10.0, 210.0].contains(&px) && [20.0, 320.0].contains(&py),
                "rot {rot} corner {x},{y} -> {px},{py}"
            );
        }
    }
}

#[test]
fn text_layout_matches_golden() {
    let cases: &[(&str, f64, f64)] = &[
        ("", 14.0, 200.0),
        ("Hello world", 14.0, 200.0),
        ("Hello world", 14.0, 40.0),
        ("The quick brown fox jumps over the lazy dog", 12.0, 120.0),
        ("Line one\nLine two\n\nAfter blank", 16.0, 300.0),
        ("Supercalifragilisticexpialidocious", 18.0, 80.0),
        ("trailing spaces   \nand  double  spaces", 10.0, 60.0),
        ("Ünïcödé ‘quotes’ – dash € 日本語", 14.0, 100.0),
        ("\ttab", 12.0, 100.0),
    ];
    let actual: Vec<Value> = cases
        .iter()
        .map(|(t, fs, w)| {
            let lines = penroam_core::text::layout(t, *fs, *w);
            json!({ "text": t, "fontSize": fs, "width": w, "lines": lines })
        })
        .collect();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/text-layout-golden.json");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !path.exists() {
        std::fs::write(&path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
    }
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(golden, Value::Array(actual));
}

#[test]
fn text_layout_rules() {
    use penroam_core::text::*;
    let l = layout("aaa bbb", 10.0, measure("aaa b", 10.0));
    assert_eq!(
        l.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        vec!["aaa ", "bbb"]
    );
    assert!((l[0].width - measure("aaa", 10.0)).abs() < 1e-9);
    let l = layout("abcdef", 10.0, measure("abc", 10.0) + 0.01);
    assert_eq!(
        l.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        vec!["abc", "def"]
    );
    assert_eq!(layout("a\n\nb", 10.0, 100.0).len(), 3);
    // narrower than one glyph still makes progress
    assert_eq!(layout("abc", 10.0, 0.5).len(), 3);
}

#[test]
fn validation_rejects_bad_numbers() {
    let mut s = ink(&sample_elements()[0]).clone();
    s.points[0].x = f64::NAN;
    assert!(validate::validate_element(&Element::Ink(s.clone())).is_err());
    s.points[0].x = 1.0;
    s.points[0].pressure = 1.5;
    assert!(validate::validate_element(&Element::Ink(s.clone())).is_err());
    s.points.clear();
    assert!(validate::validate_element(&Element::Ink(s)).is_err());
}
