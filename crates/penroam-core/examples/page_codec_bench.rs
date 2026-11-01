//! Size and speed of the binary page encoding vs JSON on an M00 benchmark scene.
//!
//!     pnpm fixtures   # generates scene-5000/10000 if missing
//!     cargo run --release -p penroam-core --example page_codec_bench [-- 10000]
use penroam_core::*;
use std::time::Instant;

fn main() {
    let n = std::env::args().nth(1).unwrap_or_else(|| "10000".into());
    let path = format!(
        "{}/../../tests/fixtures/benchmark-scenes/scene-{n}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("scene file")).unwrap();
    let mut page = Page::new(
        uuid::Uuid::new_v4(),
        Paper::Custom,
        Size {
            width: 1600.0,
            height: 1200.0,
        },
        Template::default(),
        0,
    );
    page.elements = migrate::m00_strokes_to_elements(&v, 1).unwrap();
    let points: usize = page
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
    let best = |f: &mut dyn FnMut()| {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e3
            })
            .fold(f64::MAX, f64::min)
    };
    let json = serde_json::to_vec(&page).unwrap();
    let bin = binary::encode_page(&page);
    let je = best(&mut || {
        serde_json::to_vec(&page).unwrap();
    });
    let jd = best(&mut || {
        migrate::page_from_slice(&json).unwrap();
    });
    let be = best(&mut || {
        binary::encode_page(&page);
    });
    let bd = best(&mut || {
        binary::decode_page(&bin).unwrap();
    });
    assert_eq!(binary::decode_page(&bin).unwrap(), page);
    println!(
        "scene-{n}: {} strokes, {points} points",
        page.elements.len()
    );
    println!(
        "JSON  : {:>10} B ({:5.1} B/pt)  encode {je:6.1} ms  decode {jd:6.1} ms",
        json.len(),
        json.len() as f64 / points as f64
    );
    println!(
        "binary: {:>10} B ({:5.1} B/pt)  encode {be:6.1} ms  decode {bd:6.1} ms",
        bin.len(),
        bin.len() as f64 / points as f64
    );
}
