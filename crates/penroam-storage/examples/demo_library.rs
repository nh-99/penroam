//! Builds a demo library for the README screenshots: handwriting (drawn
//! from the Hershey script stroke font with pressure and jitter), typed text,
//! highlights, sketches, an annotated PDF, a lab write-up with a formula and a
//! spreadsheet table, and an infinite canvas.
//!
//! ```sh
//! python3 tools/screenshots/make_pdf.py /tmp/reading.pdf
//! cargo run -p penroam-storage --example demo_library -- <library-dir> /tmp/reading.pdf
//! ```
//!
//! `tools/screenshots/capture.sh` runs this and then photographs the app.

use penroam_core::*;
use penroam_storage::Library;
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

const FONT_JSON: &str = include_str!("../../../tools/screenshots/hershey-script.json");

const INK: &str = "#111111";
const BLUE: &str = "#1d4ed8";
const RED: &str = "#b91c1c";
const GREEN: &str = "#15803d";
const PURPLE: &str = "#7c3aed";
const ORANGE: &str = "#c2410c";
const YELLOW: &str = "#facc15";
const MINT: &str = "#4ade80";
const PINK: &str = "#f472b6";

#[derive(Deserialize)]
struct Font {
    glyphs: HashMap<char, Glyph>,
}

#[derive(Deserialize)]
struct Glyph {
    adv: f64,
    strokes: Vec<Vec<[f64; 2]>>,
}

/// Small deterministic PRNG (xorshift64*), so the demo is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform in [-a, a].
    fn jit(&mut self, a: f64) -> f64 {
        (self.next() * 2.0 - 1.0) * a
    }
}

struct Pen<'a> {
    font: &'a Font,
    rng: Rng,
    z: i64,
    ids: u128,
}

impl Pen<'_> {
    fn id(&mut self) -> Uuid {
        self.ids += 1;
        Uuid::from_u128(self.ids)
    }

    fn z(&mut self) -> i64 {
        self.z += 1;
        self.z
    }

    /// A pen stroke through `pts` with natural pressure (ramp in/out + wave).
    fn stroke(&mut self, pts: &[(f64, f64)], color: &str, width: f64) -> Element {
        let n = pts.len().max(2) as f64;
        let phase = self.rng.next() * 6.0;
        let points = pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| {
                let t = i as f64 / (n - 1.0);
                let ramp = (t * 6.0).min(1.0).min((1.0 - t) * 5.0 + 0.35);
                let p = (0.32 + 0.38 * ramp + 0.08 * (t * 9.0 + phase).sin()).clamp(0.05, 1.0);
                InkPoint {
                    x,
                    y,
                    pressure: p,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    time_us: i as i64 * 7000,
                    twist: None,
                    flags: None,
                }
            })
            .collect();
        Element::Ink(InkStroke {
            id: self.id(),
            z: self.z(),
            tool: InkTool::Pen,
            color: color.into(),
            opacity: 1.0,
            width,
            pressure: true,
            source: "stylus".into(),
            input: Extra::new(),
            points,
            extra: Extra::new(),
        })
    }

    /// A straight-ish highlighter stroke from (x0, y) to (x1, y).
    fn highlight(&mut self, x0: f64, x1: f64, y: f64, color: &str, width: f64) -> Element {
        let steps = ((x1 - x0) / 4.0).ceil().max(2.0) as usize;
        let drift = self.rng.jit(1.2);
        let points = (0..=steps)
            .map(|i| {
                let t = i as f64 / steps as f64;
                InkPoint {
                    x: x0 + (x1 - x0) * t,
                    y: y + drift * t + self.rng.jit(0.15),
                    pressure: 0.5,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    time_us: i as i64 * 7000,
                    twist: None,
                    flags: None,
                }
            })
            .collect();
        Element::Ink(InkStroke {
            id: self.id(),
            z: self.z(),
            tool: InkTool::Highlighter,
            color: color.into(),
            opacity: 0.35,
            width,
            pressure: false,
            source: "stylus".into(),
            input: Extra::new(),
            points,
            extra: Extra::new(),
        })
    }

    /// Hand-drawn polyline: densified, with a slow wobble.
    fn line(&mut self, pts: &[(f64, f64)], color: &str, width: f64) -> Element {
        let mut out = Vec::new();
        let (a, b) = (self.rng.next() * 6.0, self.rng.next() * 6.0);
        for w in pts.windows(2) {
            let ((x0, y0), (x1, y1)) = (w[0], w[1]);
            let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
            let steps = (len / 2.0).ceil().max(1.0) as usize;
            for i in 0..steps {
                let t = i as f64 / steps as f64;
                let s = out.len() as f64 * 0.05;
                out.push((
                    x0 + (x1 - x0) * t + 0.6 * (s + a).sin(),
                    y0 + (y1 - y0) * t + 0.6 * (s * 1.3 + b).sin(),
                ));
            }
        }
        out.push(*pts.last().unwrap());
        self.stroke(&out, color, width)
    }

    /// Hand-drawn ellipse (slightly open, overlapping start).
    fn ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64, color: &str, width: f64) -> Element {
        let start = self.rng.next() * std::f64::consts::TAU;
        let pts: Vec<(f64, f64)> = (0..=72)
            .map(|i| {
                let a = start + i as f64 / 64.0 * std::f64::consts::TAU;
                let wob = 1.0 + 0.03 * (a * 3.0).sin();
                (cx + rx * wob * a.cos(), cy + ry * wob * a.sin())
            })
            .collect();
        self.stroke(&pts, color, width)
    }

    /// Handwritten text with its baseline at `y`; `xh` is the x-height in
    /// points. Returns the strokes and the x where the text ends.
    fn write(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        xh: f64,
        color: &str,
        width: f64,
    ) -> (Vec<Element>, f64) {
        let scale = xh / 300.0;
        let mut out = Vec::new();
        let mut cx = x;
        let mut base = y;
        for word in text.split(' ') {
            // each word is written in one movement: glyph strokes whose ends
            // touch are joined into a single pen stroke
            let mut cur: Vec<(f64, f64)> = Vec::new();
            let wx = self.rng.jit(0.12) * xh;
            base += self.rng.jit(0.06) * xh;
            let size = 1.0 + self.rng.jit(0.03);
            for ch in word.chars() {
                let Some(g) = self.font.glyphs.get(&ch) else {
                    cx += 300.0 * scale;
                    continue;
                };
                let (jx, jy) = (self.rng.jit(0.03) * xh, self.rng.jit(0.03) * xh);
                for s in &g.strokes {
                    let pts: Vec<(f64, f64)> = s
                        .iter()
                        .map(|[gx, gy]| {
                            (
                                cx + wx + jx + gx * scale * size,
                                base + jy - gy * scale * size,
                            )
                        })
                        .collect();
                    let joins = cur.last().is_some_and(|&(lx, ly)| {
                        ((lx - pts[0].0).powi(2) + (ly - pts[0].1).powi(2)).sqrt() < xh * 0.25
                    });
                    if !joins && cur.len() > 1 {
                        let done = std::mem::take(&mut cur);
                        out.push(self.stroke(&done, color, width));
                    } else if !joins {
                        cur.clear();
                    }
                    cur.extend(if joins { &pts[1..] } else { &pts[..] });
                }
                cx += g.adv * scale * size;
            }
            if cur.len() > 1 {
                out.push(self.stroke(&cur, color, width));
            }
            cx += self.font.glyphs[&' '].adv * scale * 0.9;
        }
        (out, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        text: &str,
        size: f64,
        color: &str,
    ) -> Element {
        Element::Text(TextBlock {
            id: self.id(),
            z: self.z(),
            x,
            y,
            width: w,
            height: h,
            text: text.into(),
            font_size: size,
            color: color.into(),
            align: TextAlign::Left,
            font: Default::default(),
            row_pitch: None,
            extra: Extra::new(),
        })
    }

    /// A small hand-drawn checkbox at (x, baseline y), optionally ticked.
    fn checkbox(&mut self, x: f64, y: f64, ticked: bool, color: &str) -> Vec<Element> {
        let s = 9.0;
        let mut v = vec![self.line(
            &[(x, y - s), (x + s, y - s), (x + s, y), (x, y), (x, y - s)],
            INK,
            1.2,
        )];
        if ticked {
            v.push(self.line(
                &[(x + 1.5, y - 5.0), (x + 4.0, y - 1.5), (x + 11.0, y - 12.0)],
                color,
                2.0,
            ));
        }
        v
    }
}

fn add(lib: &mut Library, nb: Uuid, page: Uuid, elements: Vec<Element>) {
    let rev = lib.load_page(nb, page).expect("page").revision;
    let batch = OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: rev,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::AddElements { elements },
        }],
    };
    lib.apply_page_operations(nb, page, &batch).expect("apply");
}

/// Baseline of ruled line `n` (lines at 72 + 24k).
fn ruled(n: usize) -> f64 {
    72.0 + 24.0 * n as f64 - 4.0
}

fn ruled_defaults() -> PageDefaults {
    PageDefaults {
        paper: Paper::Letter,
        template: Template {
            kind: TemplateKind::Ruled,
            spacing: 24.0,
        },
    }
}

fn physics(lib: &mut Library, pen: &mut Pen) {
    let (nb, p1) = lib
        .create_notebook(
            "Physics: mechanics",
            ruled_defaults(),
            Some("#3b5b7a".into()),
        )
        .expect("notebook");
    let (nb, p1) = (nb.id, p1.id);
    let mut e = Vec::new();
    let (t, end) = pen.write(
        "Newton's laws of motion",
        86.0,
        ruled(1) + 2.0,
        9.5,
        BLUE,
        2.2,
    );
    e.extend(t);
    e.push(pen.line(
        &[(84.0, ruled(1) + 9.0), (end - 6.0, ruled(1) + 7.0)],
        BLUE,
        1.6,
    ));
    let lines = [
        (3, "1. Inertia: a body keeps its velocity"),
        (4, "    unless a net force acts on it."),
        (6, "2. Force = mass x acceleration"),
        (9, "3. Every action has an equal and"),
        (10, "    opposite reaction."),
    ];
    for (n, s) in lines {
        e.extend(pen.write(s, 86.0, ruled(n), 6.2, INK, 2.2).0);
    }
    // F = ma, large, highlighted and circled
    e.push(pen.highlight(150.0, 250.0, ruled(7) - 8.0, YELLOW, 16.0));
    e.extend(pen.write("F = ma", 156.0, ruled(7) + 1.0, 9.0, INK, 2.6).0);
    e.extend(pen.write("exam!", 300.0, ruled(7), 6.5, RED, 2.2).0);
    e.push(pen.ellipse(323.0, ruled(7) - 6.0, 34.0, 14.0, RED, 1.6));
    // free-body diagram: block on a surface, applied force and friction
    let (bx, by) = (360.0, ruled(15));
    e.push(pen.line(&[(250.0, by), (540.0, by)], INK, 1.6));
    for i in 0..10 {
        let x = 262.0 + i as f64 * 28.0;
        e.push(pen.line(&[(x, by + 1.0), (x - 9.0, by + 10.0)], INK, 1.0));
    }
    e.push(pen.line(
        &[
            (bx, by),
            (bx, by - 54.0),
            (bx + 72.0, by - 54.0),
            (bx + 72.0, by),
            (bx, by),
        ],
        INK,
        2.2,
    ));
    e.extend(pen.write("m", bx + 30.0, by - 22.0, 7.0, INK, 2.0).0);
    let fy = by - 27.0;
    e.push(pen.line(&[(bx + 74.0, fy), (bx + 150.0, fy)], BLUE, 2.2));
    e.push(pen.line(
        &[
            (bx + 140.0, fy - 7.0),
            (bx + 151.0, fy),
            (bx + 140.0, fy + 7.0),
        ],
        BLUE,
        2.2,
    ));
    e.extend(pen.write("F", bx + 155.0, fy + 5.0, 7.0, BLUE, 2.0).0);
    e.push(pen.line(&[(bx - 2.0, by - 6.0), (bx - 70.0, by - 6.0)], RED, 2.2));
    e.push(pen.line(
        &[
            (bx - 60.0, by - 13.0),
            (bx - 71.0, by - 6.0),
            (bx - 60.0, by + 1.0),
        ],
        RED,
        2.2,
    ));
    e.extend(
        pen.write("friction", bx - 130.0, by - 12.0, 5.5, RED, 1.8)
            .0,
    );
    e.push(pen.line(
        &[(bx + 36.0, by - 56.0), (bx + 36.0, by - 110.0)],
        GREEN,
        2.0,
    ));
    e.push(pen.line(
        &[
            (bx + 29.0, by - 100.0),
            (bx + 36.0, by - 111.0),
            (bx + 43.0, by - 100.0),
        ],
        GREEN,
        2.0,
    ));
    e.extend(pen.write("N", bx + 46.0, by - 98.0, 7.0, GREEN, 2.0).0);
    let mut note = pen.text(
        86.0,
        72.0 + 24.0 * 17.0,
        430.0,
        96.0,
        "**Units:** force in newtons, $N = kg \\cdot m/s^2$. Net force zero means *constant velocity*, not necessarily rest.\n\n- [ ] Review problems 3 to 7 before Thursday",
        12.0,
        INK,
    );
    if let Element::Text(t) = &mut note {
        t.row_pitch = Some(24.0);
    }
    e.push(note);
    e.extend(
        pen.write(
            "Q: why does a book on a table not fall?",
            86.0,
            ruled(23),
            6.0,
            PURPLE,
            2.0,
        )
        .0,
    );
    e.extend(
        pen.write(
            "normal force balances gravity",
            110.0,
            ruled(24),
            6.0,
            PURPLE,
            2.0,
        )
        .0,
    );
    add(lib, nb, p1, e);

    // page 2: worked example on a grid page
    let grid = Template {
        kind: TemplateKind::Grid,
        spacing: 20.0,
    };
    let (_, p2) = lib.create_page(nb, None, Some(grid), None).expect("page");
    let mut e = Vec::new();
    e.extend(pen.write("Worked example", 60.0, 66.0, 8.0, BLUE, 2.2).0);
    e.extend(
        pen.write("m = 2 kg,  F = 10 N", 60.0, 110.0, 6.0, INK, 2.0)
            .0,
    );
    e.extend(pen.write("a = F / m = 5 m/s", 60.0, 140.0, 6.0, INK, 2.0).0);
    e.push(pen.highlight(56.0, 200.0, 135.0, MINT, 14.0));
    // velocity-time graph
    let (ox, oy) = (80.0, 400.0);
    e.push(pen.line(&[(ox, oy - 180.0), (ox, oy), (ox + 260.0, oy)], INK, 1.8));
    e.push(pen.line(&[(ox, oy), (ox + 220.0, oy - 150.0)], RED, 2.4));
    e.extend(pen.write("v", ox - 18.0, oy - 170.0, 6.0, INK, 1.8).0);
    e.extend(pen.write("t", ox + 250.0, oy + 18.0, 6.0, INK, 1.8).0);
    e.extend(
        pen.write("slope = a", ox + 140.0, oy - 120.0, 5.5, RED, 1.8)
            .0,
    );
    add(lib, nb, p2.id, e);

    let (_, p3) = lib.create_page(nb, None, None, None).expect("page");
    let mut e = Vec::new();
    e.extend(pen.write("Friction", 86.0, ruled(1), 8.0, BLUE, 2.2).0);
    e.extend(
        pen.write("static > kinetic", 86.0, ruled(3), 6.0, INK, 2.0)
            .0,
    );
    e.extend(pen.write("f = mu N", 86.0, ruled(4), 6.0, INK, 2.0).0);
    add(lib, nb, p3.id, e);

    // page 4: a lab write-up with a heading, a display formula and a table
    // that works as a spreadsheet, next to a hand-drawn pendulum
    let (_, p4) = lib.create_page(nb, None, None, None).expect("page");
    let mut e = Vec::new();
    e.extend(pen.write("Pendulum lab", 86.0, ruled(1), 8.5, BLUE, 2.2).0);
    let mut lab = pen.text(
        86.0,
        72.0 + 24.0 * 2.0,
        300.0,
        312.0,
        "## Measuring g with a pendulum\n\n\
         $$g = \\frac{4\\pi^2 L}{T^2}$$\n\n\
         | L (m) | T (s) | g (m/s²) | Done |\n\
         |---:|---:|---:|:---:|\n\
         | 0.25 | 1.00 | =ROUND(39.478*A2/B2^2, 2) | [x] |\n\
         | 0.50 | 1.42 | =ROUND(39.478*A3/B3^2, 2) | [x] |\n\
         | 0.75 | 1.74 | =ROUND(39.478*A4/B4^2, 2) | [x] |\n\
         | 1.00 | 2.01 | =ROUND(39.478*A5/B5^2, 2) | [ ] |\n\
         | **Mean** | | =ROUND(AVERAGE(C2:C5), 2) | |\n\n\
         - [x] Time 10 swings, divide by 10\n\
         - [ ] Repeat with a heavier bob",
        12.0,
        INK,
    );
    if let Element::Text(t) = &mut lab {
        t.row_pitch = Some(24.0);
    }
    e.push(lab);
    // the pendulum beside the table: a support, the string at an angle, the
    // bob and the arc it swings along
    let (sx, sy) = (460.0, ruled(8));
    e.push(pen.line(&[(sx - 50.0, sy), (sx + 50.0, sy)], INK, 2.2));
    for i in 0..5 {
        let x = sx - 44.0 + i as f64 * 20.0;
        e.push(pen.line(&[(x, sy), (x + 9.0, sy - 9.0)], INK, 1.0));
    }
    let (bx, by) = (sx + 45.0, sy + 133.0);
    e.push(pen.line(&[(sx, sy), (bx, by)], INK, 1.6));
    e.push(pen.ellipse(bx + 4.0, by + 11.0, 11.0, 11.0, ORANGE, 2.2));
    e.push(pen.line(&[(sx, sy), (sx, sy + 150.0)], "#9ca3af", 1.0));
    let arc: Vec<(f64, f64)> = (0..=24)
        .map(|i| {
            let a = -0.45 + 0.9 * i as f64 / 24.0;
            (sx + 160.0 * a.sin(), sy + 160.0 * a.cos())
        })
        .collect();
    e.push(pen.line(&arc, PURPLE, 1.4));
    e.extend(pen.write("L", sx + 28.0, sy + 72.0, 6.0, INK, 1.8).0);
    e.extend(
        pen.write("swing", sx + 62.0, sy + 172.0, 5.5, PURPLE, 1.8)
            .0,
    );
    e.extend(
        pen.write("close to 9.81!", 262.0, ruled(13), 6.0, GREEN, 2.0)
            .0,
    );
    add(lib, nb, p4.id, e);
}

/// An infinite canvas with a mind map that spreads well beyond one sheet.
fn ideas(lib: &mut Library, pen: &mut Pen) {
    let defaults = PageDefaults {
        paper: Paper::Letter,
        template: Template {
            kind: TemplateKind::Dotted,
            spacing: 20.0,
        },
    };
    let (nb, p) = lib
        .create_notebook("Science fair ideas", defaults, Some("#2f6f73".into()))
        .expect("notebook");
    lib.set_infinite_canvas(nb.id, true).expect("canvas");
    let mut e = Vec::new();
    let (cx, cy) = (520.0, 420.0);
    e.push(pen.ellipse(cx, cy, 120.0, 52.0, BLUE, 2.6));
    e.extend(
        pen.write("Science fair", cx - 84.0, cy + 8.0, 9.0, BLUE, 2.4)
            .0,
    );
    let branches: [(f64, f64, &str, &str); 5] = [
        (-380.0, -230.0, "solar oven", ORANGE),
        (360.0, -250.0, "water filter", BLUE),
        (440.0, 120.0, "bridge loads", RED),
        (-60.0, 300.0, "plant growth + light", GREEN),
        (-460.0, 140.0, "paper planes", PURPLE),
    ];
    for (dx, dy, label, color) in branches {
        let (x, y) = (cx + dx, cy + dy);
        let w = label.len() as f64 * 8.0 + 40.0;
        // connect the edge of the centre bubble to the branch
        let len = (dx * dx + dy * dy).sqrt();
        e.push(pen.line(
            &[
                (cx + dx / len * 120.0, cy + dy / len * 52.0),
                (x - dx / len * w * 0.55, y - dy / len * 26.0),
            ],
            INK,
            1.6,
        ));
        e.push(pen.ellipse(x, y, w * 0.58, 30.0, color, 2.2));
        e.extend(pen.write(label, x - w * 0.4, y + 5.0, 6.5, color, 2.2).0);
    }
    e.push(pen.text(
        cx + 270.0,
        cy - 180.0,
        230.0,
        110.0,
        "**Water filter**\n- sand, gravel, charcoal\n- test cloudiness before and after\n- *cheap to build*",
        12.0,
        INK,
    ));
    e.push(pen.text(
        cx - 560.0,
        cy - 150.0,
        220.0,
        90.0,
        "**Solar oven**\nPizza box, foil and cling film. Measure the inside temperature every 5 min.",
        12.0,
        INK,
    ));
    e.extend(
        pen.write("favourite!", cx + 330.0, cy - 300.0, 6.0, RED, 2.0)
            .0,
    );
    e.push(pen.highlight(cx + 326.0, cx + 400.0, cy - 304.0, YELLOW, 14.0));
    e.extend(
        pen.write("due Dec 12", cx - 100.0, cy + 420.0, 7.0, ORANGE, 2.2)
            .0,
    );
    e.push(pen.line(
        &[(cx - 104.0, cy + 428.0), (cx + 30.0, cy + 426.0)],
        ORANGE,
        1.6,
    ));
    add(lib, nb.id, p.id, e);
}

fn trip(lib: &mut Library, pen: &mut Pen) {
    let defaults = PageDefaults {
        paper: Paper::Letter,
        template: Template {
            kind: TemplateKind::Dotted,
            spacing: 20.0,
        },
    };
    let (nb, p) = lib
        .create_notebook("Trip to Lisbon", defaults, Some("#3b7a52".into()))
        .expect("notebook");
    let mut e = Vec::new();
    e.extend(
        pen.write("Lisbon - packing list", 60.0, 80.0, 9.0, GREEN, 2.4)
            .0,
    );
    let items = [
        ("passport + tickets", true),
        ("adapter (type F)", true),
        ("comfortable shoes", true),
        ("rain jacket", false),
        ("camera charger", false),
        ("phrasebook", false),
    ];
    for (i, (s, done)) in items.iter().enumerate() {
        let y = 140.0 + i as f64 * 40.0;
        e.extend(pen.checkbox(62.0, y, *done, GREEN));
        e.extend(pen.write(s, 84.0, y, 6.2, INK, 2.0).0);
    }
    e.push(pen.highlight(80.0, 200.0, 337.0, PINK, 14.0));
    // the typed itinerary is handwritten, the other typed notes formatted
    let mut itinerary = pen.text(
        330.0,
        110.0,
        230.0,
        150.0,
        "Day 1: Alfama, castle, tram 28\nDay 2: Belém tower + pastries\nDay 3: Sintra (train from Rossio)\nDay 4: LX Factory, sunset at the miradouro",
        12.0,
        INK,
    );
    if let Element::Text(t) = &mut itinerary {
        t.font = TextFont::Handwritten;
    }
    e.push(itinerary);
    e.extend(
        pen.write("book Sintra tickets!", 330.0, 300.0, 6.0, ORANGE, 2.0)
            .0,
    );
    e.push(pen.line(&[(326.0, 305.0), (470.0, 303.0)], ORANGE, 1.4));
    // a tiny tram sketch
    let (x, y) = (360.0, 470.0);
    e.push(pen.line(
        &[
            (x, y),
            (x + 120.0, y),
            (x + 120.0, y - 60.0),
            (x, y - 60.0),
            (x, y),
        ],
        ORANGE,
        2.2,
    ));
    for i in 0..4 {
        let wx = x + 10.0 + i as f64 * 27.0;
        e.push(pen.line(
            &[
                (wx, y - 50.0),
                (wx + 20.0, y - 50.0),
                (wx + 20.0, y - 32.0),
                (wx, y - 32.0),
                (wx, y - 50.0),
            ],
            INK,
            1.4,
        ));
    }
    e.push(pen.ellipse(x + 25.0, y + 6.0, 7.0, 7.0, INK, 1.6));
    e.push(pen.ellipse(x + 95.0, y + 6.0, 7.0, 7.0, INK, 1.6));
    e.push(pen.line(&[(x + 60.0, y - 60.0), (x + 60.0, y - 85.0)], INK, 1.4));
    e.extend(pen.write("28", x + 48.0, y - 12.0, 6.0, INK, 1.8).0);
    add(lib, nb.id, p.id, e);
}

fn sketchbook(lib: &mut Library, pen: &mut Pen) {
    let defaults = PageDefaults {
        paper: Paper::Letter,
        template: Template {
            kind: TemplateKind::Blank,
            spacing: 24.0,
        },
    };
    let (nb, p) = lib
        .create_notebook("Sketchbook", defaults, Some("#6b4f8a".into()))
        .expect("notebook");
    let mut e = Vec::new();
    // mountains, sun, birds, lake
    e.push(pen.line(
        &[
            (40.0, 420.0),
            (150.0, 260.0),
            (210.0, 330.0),
            (300.0, 200.0),
            (420.0, 360.0),
            (480.0, 300.0),
            (570.0, 420.0),
        ],
        INK,
        2.6,
    ));
    e.push(pen.line(&[(275.0, 236.0), (300.0, 250.0), (322.0, 232.0)], INK, 1.6));
    e.push(pen.ellipse(470.0, 170.0, 34.0, 34.0, ORANGE, 2.4));
    for i in 0..8 {
        let a = i as f64 / 8.0 * std::f64::consts::TAU;
        e.push(pen.line(
            &[
                (470.0 + 44.0 * a.cos(), 170.0 + 44.0 * a.sin()),
                (470.0 + 60.0 * a.cos(), 170.0 + 60.0 * a.sin()),
            ],
            ORANGE,
            2.0,
        ));
    }
    for (bx, by) in [(120.0, 160.0), (150.0, 140.0), (185.0, 170.0)] {
        e.push(pen.line(
            &[
                (bx, by),
                (bx + 10.0, by - 7.0),
                (bx + 20.0, by),
                (bx + 30.0, by - 7.0),
                (bx + 40.0, by),
            ],
            INK,
            1.6,
        ));
    }
    for i in 0..5 {
        let y = 450.0 + i as f64 * 14.0;
        e.push(pen.line(
            &[(80.0 + i as f64 * 20.0, y), (520.0 - i as f64 * 30.0, y)],
            BLUE,
            1.4,
        ));
    }
    e.push(pen.highlight(60.0, 550.0, 470.0, "#60a5fa", 30.0));
    e.extend(pen.write("lake at dawn", 380.0, 560.0, 7.0, PURPLE, 2.0).0);
    add(lib, nb.id, p.id, e);
}

fn reading(lib: &mut Library, pen: &mut Pen, pdf: &[u8]) {
    let (nb, cover) = lib
        .create_notebook(
            "Reading: spaced repetition",
            ruled_defaults(),
            Some("#8a6f3b".into()),
        )
        .expect("notebook");
    let nb = nb.id;
    let asset = lib
        .add_asset(
            nb,
            pdf,
            "application/pdf",
            Some("spaced-repetition.pdf".into()),
        )
        .expect("asset");
    let info = penroam_pdf::inspect(pdf).expect("pdf");
    let pages = info
        .iter()
        .map(|i| {
            let bg = i.background(&asset.sha256);
            (
                Paper::Pdf,
                bg.display_size(),
                Template {
                    kind: TemplateKind::Blank,
                    spacing: 24.0,
                },
                Some(bg),
            )
        })
        .collect();
    // the PDF pages go first; the notebook's ruled first page follows for notes
    let (_, pages) = lib.create_pages(nb, Some(0), pages).expect("pdf pages");
    let mut e = Vec::new();
    e.extend(
        pen.write("Seminar questions", 86.0, ruled(1), 8.0, BLUE, 2.2)
            .0,
    );
    e.extend(
        pen.write(
            "- how long should the first gap be?",
            86.0,
            ruled(3),
            6.0,
            INK,
            2.0,
        )
        .0,
    );
    e.extend(
        pen.write(
            "- does this work for languages?",
            86.0,
            ruled(4),
            6.0,
            INK,
            2.0,
        )
        .0,
    );
    add(lib, nb, cover.id, e);
    let p = pages[0].id;
    // baselines in page coordinates (y down) = 792 - PDF y
    let w = |s: &str| s.len() as f64 * 4.45;
    let mut e = vec![pen.highlight(
        72.0,
        72.0 + w("review just before you would forget, and let the gaps grow."),
        270.5,
        YELLOW,
        12.0,
    )];
    e.push(pen.highlight(
        72.0 + w("Rereading feels productive because the material looks familiar. "),
        72.0 + w("Rereading feels productive because the material looks familiar. Familiarity is"),
        324.5,
        YELLOW,
        12.0,
    ));
    e.push(pen.highlight(
        72.0,
        72.0 + w("not the same as recall") + 2.0,
        340.5,
        YELLOW,
        12.0,
    ));
    e.push(pen.line(
        &[
            (72.0, 395.0),
            (
                72.0 + w(
                    "A practical routine: after each class, write three questions from memory.",
                ),
                394.0,
            ),
        ],
        RED,
        1.4,
    ));
    e.push(pen.highlight(
        72.0 + w("twenty of the same kind in a row, but it trains you to "),
        72.0 + w("twenty of the same kind in a row, but it trains you to recognise which method a"),
        490.5,
        MINT,
        12.0,
    ));
    // margin notes
    e.extend(pen.write("key idea!", 476.0, 262.0, 5.5, RED, 1.8).0);
    e.push(pen.line(&[(472.0, 264.0), (350.0, 268.0)], RED, 1.4));
    e.push(pen.line(&[(358.0, 262.0), (349.0, 268.0), (358.0, 274.0)], RED, 1.4));
    e.extend(pen.write("try this", 476.0, 396.0, 5.5, BLUE, 1.8).0);
    e.extend(pen.write("every week", 476.0, 412.0, 5.5, BLUE, 1.8).0);
    e.extend(pen.write("so true", 476.0, 340.0, 5.5, PURPLE, 1.8).0);
    e.extend(
        pen.write("ask about this in class", 300.0, 560.0, 6.0, BLUE, 2.0)
            .0,
    );
    e.push(pen.line(&[(296.0, 548.0), (250.0, 510.0)], BLUE, 1.4));
    e.push(pen.line(&[(255.0, 520.0), (250.0, 510.0), (261.0, 512.0)], BLUE, 1.4));
    e.push(pen.text(
        72.0,
        610.0,
        440.0,
        80.0,
        "My plan: 3 questions after each lecture, reviewed on day 1, 3 and 7. Mix problem sets from different chapters on Sundays.",
        12.0,
        "#1d4ed8",
    ));
    add(lib, nb, p, e);
}

fn meeting(lib: &mut Library, pen: &mut Pen) {
    let (nb, p) = lib
        .create_notebook(
            "Club meeting notes",
            ruled_defaults(),
            Some("#7a3b3b".into()),
        )
        .expect("notebook");
    let mut e = Vec::new();
    e.extend(
        pen.write("Robotics club - Oct 28", 86.0, ruled(1), 8.5, RED, 2.2)
            .0,
    );
    for (n, s) in [
        (3, "- motor driver arrived, test Friday"),
        (4, "- need two more volunteers for demo day"),
        (5, "- budget: 120 left for sensors"),
        (7, "Action items"),
        (8, "  Sam: order wheels"),
        (9, "  Priya: poster draft"),
    ] {
        e.extend(pen.write(s, 86.0, ruled(n), 6.0, INK, 2.0).0);
    }
    e.push(pen.highlight(84.0, 330.0, ruled(4) - 5.0, YELLOW, 14.0));
    add(lib, nb.id, p.id, e);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: demo_library <library-dir> <reading.pdf>");
    let pdf = std::fs::read(args.next().expect("pdf path")).expect("read pdf");
    if std::path::Path::new(&dir).join("library.json").exists() {
        panic!("{dir} already holds a library; pass an empty directory");
    }
    let font: Font = serde_json::from_str(FONT_JSON).expect("font");
    let mut lib = Library::open(&dir).expect("open library");
    let mut pen = Pen {
        font: &font,
        rng: Rng(0x9e37_79b9_7f4a_7c15),
        z: 0,
        ids: 0xdeed_0000_0000_0000_0000_0000_0000_0000,
    };
    // created oldest first: the library lists the most recent first, so the
    // canvas notebook sits after the five that the screenshot session opens
    ideas(&mut lib, &mut pen);
    meeting(&mut lib, &mut pen);
    sketchbook(&mut lib, &mut pen);
    trip(&mut lib, &mut pen);
    reading(&mut lib, &mut pen, &pdf);
    physics(&mut lib, &mut pen);
    lib.flush_all().expect("flush");
    println!("demo library written to {dir}");
}
