// LaTeX formulas as flat vector outlines. MathJax (loaded on first use, it is
// the app's largest dependency) renders TeX to SVG; `flatten` reduces that to
// one absolute path, in em from the baseline, which the canvas and the PDF
// exporter fill the same way. Nothing here is stored in notebooks: the outlines
// are recomputed for the screen and handed to Rust for each export (see
// RenderedMath in crates/penroam-pdf and docs/formats/model-and-archive.md).
//
// A formula MathJax can't draw with its own glyphs (an error, or characters
// outside its fonts) has no outline, and layout shows its source instead.
import type { MathBox } from "./markdown";
import type { RenderedMath } from "../generated/RenderedMath";

type Matrix = [number, number, number, number, number, number];
type Convert = (tex: string, display: boolean) => RenderedMath | null;

const UNITS_PER_EM = 1000;

const multiply = (a: Matrix, b: Matrix): Matrix => [
  a[0] * b[0] + a[2] * b[1],
  a[1] * b[0] + a[3] * b[1],
  a[0] * b[2] + a[2] * b[3],
  a[1] * b[2] + a[3] * b[3],
  a[0] * b[4] + a[2] * b[5] + a[4],
  a[1] * b[4] + a[3] * b[5] + a[5],
];

/** The SVG `transform` values MathJax writes. Anything else fails the formula. */
export function parseTransform(value: string | undefined, base: Matrix): Matrix | null {
  let m = base;
  for (const [, name, args] of (value ?? "").matchAll(/(\w+)\(([^)]*)\)/g)) {
    const n = args.trim().split(/[\s,]+/).map(Number);
    if (name === "translate") m = multiply(m, [1, 0, 0, 1, n[0], n[1] ?? 0]);
    else if (name === "scale") m = multiply(m, [n[0], 0, 0, n[1] ?? n[0], 0, 0]);
    else if (name === "matrix" && n.length === 6) m = multiply(m, n as Matrix);
    else return null;
  }
  return m;
}

/** Rewrites absolute SVG path data as `M`, `L`, `C` and `Z`; null if it can't. */
export function normalizePath(d: string, m: Matrix): string[] | null {
  const tokens = d.match(/[A-Za-z]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?/g) ?? [];
  const out: string[] = [];
  const pt = (x: number, y: number) => {
    const em = (v: number) => String(Math.round((v / UNITS_PER_EM) * 1e5) / 1e5);
    return `${em(m[0] * x + m[2] * y + m[4])} ${em(m[1] * x + m[3] * y + m[5])}`;
  };
  let i = 0;
  let cx = 0, cy = 0, sx = 0, sy = 0;
  let qx = 0, qy = 0; // last quadratic control point, for T
  let cmd = "";
  const num = () => Number(tokens[i++]);
  const quad = (x1: number, y1: number, x: number, y: number) => {
    const c = (a: number, b: number) => a + (2 / 3) * (b - a);
    out.push(`C ${pt(c(cx, x1), c(cy, y1))} ${pt(c(x, x1), c(y, y1))} ${pt(x, y)}`);
    [qx, qy, cx, cy] = [x1, y1, x, y];
  };
  while (i < tokens.length) {
    if (/[A-Za-z]/.test(tokens[i])) cmd = tokens[i++];
    switch (cmd) {
      case "M":
        cx = sx = num();
        cy = sy = num();
        out.push(`M ${pt(cx, cy)}`);
        cmd = "L"; // further pairs are lines
        break;
      case "L":
      case "H":
      case "V":
        if (cmd !== "V") cx = num();
        if (cmd !== "H") cy = num();
        out.push(`L ${pt(cx, cy)}`);
        [qx, qy] = [cx, cy];
        break;
      case "Q":
        quad(num(), num(), num(), num());
        break;
      case "T":
        quad(2 * cx - qx, 2 * cy - qy, num(), num());
        break;
      case "Z":
        out.push("Z");
        [cx, cy, qx, qy] = [sx, sy, sx, sy];
        cmd = "";
        break;
      default:
        return null;
    }
  }
  return out;
}

/** The lite-DOM calls `flatten` needs, so it can be tested with plain objects. */
export interface Tree<N> {
  kind(n: N): string;
  children(n: N): N[];
  attr(n: N, name: string): string | undefined;
}

/** Collects every shape under `n`; false if one can't be represented. */
export function flatten<N>(t: Tree<N>, n: N, base: Matrix, out: string[]): boolean {
  const kind = t.kind(n);
  const m = parseTransform(t.attr(n, "transform"), base);
  if (!m) return false;
  if (kind === "text") return false;
  if (kind === "path") {
    const p = normalizePath(t.attr(n, "d") ?? "", m);
    if (!p) return false;
    out.push(...p);
  } else if (kind === "rect") {
    const [x, y, w, h] = ["x", "y", "width", "height"].map((a) => Number(t.attr(n, a) ?? 0));
    const p = normalizePath(`M ${x} ${y} L ${x + w} ${y} L ${x + w} ${y + h} L ${x} ${y + h} Z`, m);
    if (p) out.push(...p);
  } else if (kind === "line") {
    return false;
  }
  return t.children(n).every((c) => flatten(t, c, m, out));
}

let convert: Convert | null = null;
let loading: Promise<void> | null = null;
let epoch = 0;
const cache = new Map<string, RenderedMath | null>();
const listeners = new Set<() => void>();

async function load(): Promise<Convert> {
  const [{ mathjax }, { TeX }, { SVG }, { liteAdaptor }, { RegisterHTMLHandler }] = await Promise.all([
    import("mathjax-full/js/mathjax.js"),
    import("mathjax-full/js/input/tex.js"),
    import("mathjax-full/js/output/svg.js"),
    import("mathjax-full/js/adaptors/liteAdaptor.js"),
    import("mathjax-full/js/handlers/html.js"),
    import("mathjax-full/js/input/tex/base/BaseConfiguration.js"),
    import("mathjax-full/js/input/tex/ams/AmsConfiguration.js"),
    import("mathjax-full/js/input/tex/newcommand/NewcommandConfiguration.js"),
    import("mathjax-full/js/input/tex/boldsymbol/BoldsymbolConfiguration.js"),
    import("mathjax-full/js/input/tex/cancel/CancelConfiguration.js"),
  ]);
  const adaptor = liteAdaptor();
  RegisterHTMLHandler(adaptor);
  const doc = mathjax.document("", {
    InputJax: new TeX({ packages: ["base", "ams", "newcommand", "boldsymbol", "cancel"] }),
    OutputJax: new SVG({ fontCache: "none" }),
  });
  // Line-breaking and sizing never read these: the output is in font units.
  const metrics = { em: 16, ex: 8, containerWidth: 1e6 };
  const tree: Tree<any> = {
    kind: (n) => adaptor.kind(n),
    children: (n) => (adaptor.kind(n) === "#text" ? [] : adaptor.childNodes(n)),
    attr: (n, name) => (adaptor.kind(n) === "#text" ? undefined : (adaptor.getAttribute(n, name) ?? undefined)),
  };
  return (tex, display) => {
    const container = doc.convert(tex, { display, ...metrics });
    const svg = adaptor.childNodes(container)[0] as any;
    const box = adaptor.getAttribute(svg, "viewBox").split(" ").map(Number);
    const path: string[] = [];
    // the outermost SVG origin is the left end of the baseline
    if (!flatten(tree, svg, [1, 0, 0, 1, -box[0], 0], path)) return null;
    return {
      tex,
      display,
      width: box[2] / UNITS_PER_EM,
      height: -box[1] / UNITS_PER_EM,
      depth: (box[3] + box[1]) / UNITS_PER_EM,
      path: path.join(" "),
    };
  };
}

/** Starts loading MathJax. Resolves when formulas can be rendered, or never will be. */
export function ensureMath(): Promise<void> {
  loading ??= load()
    .then((c) => {
      convert = c;
      cache.clear();
      epoch++;
      listeners.forEach((f) => f());
    })
    .catch((e) => console.error("math renderer unavailable", e));
  return loading;
}

/** Calls `f` when formulas become drawable. Returns the unsubscribe function. */
export function onMathReady(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

/** Changes when `renderedMath` may answer differently: part of the layout cache key. */
export const mathEpoch = () => epoch;

/** The outline of a formula, or null while MathJax loads and for invalid TeX. */
export function renderedMath(tex: string, display: boolean): RenderedMath | null {
  if (!convert) return null;
  const key = `${display ? "d" : "i"}${tex}`;
  let r = cache.get(key);
  if (r === undefined) {
    try {
      r = convert(tex, display);
    } catch {
      r = null;
    }
    cache.set(key, r);
  }
  return r;
}

/** Layout's view of `renderedMath`. */
export function mathBox(tex: string, display: boolean): MathBox | null {
  return renderedMath(tex, display);
}

/** Fills a formula with the context's current fill style. */
export function drawMath(ctx: CanvasRenderingContext2D, m: RenderedMath, x: number, y: number, size: number): void {
  const t = m.path.split(" ");
  const px = (i: number) => x + Number(t[i]) * size;
  const py = (i: number) => y + Number(t[i]) * size;
  ctx.beginPath();
  for (let i = 0; i < t.length; ) {
    const c = t[i++];
    if (c === "M") {
      ctx.moveTo(px(i), py(i + 1));
      i += 2;
    } else if (c === "L") {
      ctx.lineTo(px(i), py(i + 1));
      i += 2;
    } else if (c === "C") {
      ctx.bezierCurveTo(px(i), py(i + 1), px(i + 2), py(i + 3), px(i + 4), py(i + 5));
      i += 6;
    } else ctx.closePath();
  }
  ctx.fill();
}

/** Renders a notebook's formulas for an export, skipping any that can't be drawn. */
export async function renderMathTable(spans: { tex: string; display: boolean }[]): Promise<RenderedMath[]> {
  if (!spans.length) return [];
  await ensureMath();
  return spans.map((s) => renderedMath(s.tex, s.display)).filter((m): m is RenderedMath => !!m);
}
