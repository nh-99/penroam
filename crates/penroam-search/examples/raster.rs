//! Renders strokes with the search rasterizer and writes a binary PGM. Used to produce the
//! golden images that keep other implementations of `raster.rs` (the model-training pipeline)
//! byte-identical.
//!
//! ```text
//! cargo run -p penroam-search --example raster -- <strokes.json> <out.pgm> [spec.json]
//! ```
//!
//! `strokes.json` is an array of strokes, each an array of `[x, y]` points in page points;
//! `spec.json` is a `RasterSpec` (camelCase), default `RasterSpec::default()`.

use penroam_core::{InkPoint, InkStroke, InkTool};
use penroam_search::raster::{render_strokes, RasterSpec};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        eprintln!("usage: raster <strokes.json> <out.pgm> [spec.json]");
        std::process::exit(2);
    }
    let pts: Vec<Vec<[f64; 2]>> =
        serde_json::from_slice(&std::fs::read(&a[0]).expect("read strokes")).expect("strokes json");
    let spec: RasterSpec = match a.get(2) {
        Some(p) => {
            serde_json::from_slice(&std::fs::read(p).expect("read spec")).expect("spec json")
        }
        None => RasterSpec::default(),
    };
    let strokes: Vec<InkStroke> = pts
        .iter()
        .map(|s| InkStroke {
            id: uuid::Uuid::nil(),
            z: 0,
            tool: InkTool::Pen,
            color: "#000000".into(),
            opacity: 1.0,
            width: 1.0,
            pressure: false,
            source: "raster-example".into(),
            input: Default::default(),
            points: s
                .iter()
                .map(|p| InkPoint {
                    x: p[0],
                    y: p[1],
                    pressure: 0.5,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    time_us: 0,
                    twist: None,
                    flags: None,
                })
                .collect(),
            extra: Default::default(),
        })
        .collect();
    let refs: Vec<&InkStroke> = strokes.iter().collect();
    std::fs::write(&a[1], render_strokes(&refs, &spec).to_pgm()).expect("write pgm");
}
