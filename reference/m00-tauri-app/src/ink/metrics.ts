// Benchmark recorder. Event/timing names follow benchmarks/protocol.md and
// match the Flutter recorder (flutter_app/lib/ink/metrics.dart) so result
// files can be compared side by side.
//
// Clock: performance.now() (monotonic). PointerEvent.timeStamp shares the
// same time origin in conforming engines, which is what makes
// event_to_handler_ms meaningful here; this is checked at runtime
// (timestampClockOk) rather than assumed.

export interface SeriesSummary {
  count: number;
  retained: number;
  median?: number;
  p95?: number;
  max?: number;
  min?: number;
  mean?: number;
}

export function summarize(values: ArrayLike<number>, total = values.length): SeriesSummary {
  const out: SeriesSummary = { count: total, retained: values.length };
  if (values.length === 0) return out;
  const sorted = Array.from(values).sort((a, b) => a - b);
  const pct = (p: number) => sorted[Math.round((sorted.length - 1) * p)];
  out.median = pct(0.5);
  out.p95 = pct(0.95);
  out.max = sorted[sorted.length - 1];
  out.min = sorted[0];
  out.mean = sorted.reduce((a, b) => a + b, 0) / sorted.length;
  return out;
}

class Series {
  values: Float64Array;
  count = 0;
  next = 0;
  constructor(cap: number) {
    this.values = new Float64Array(cap);
  }
  add(v: number) {
    this.values[this.next] = v;
    this.next = (this.next + 1) % this.values.length;
    this.count++;
  }
  snapshot(): Float64Array {
    return this.values.slice(0, Math.min(this.count, this.values.length));
  }
}

export interface StrokeStats {
  id: string;
  device: string;
  samples: number;
  coalescedSamples: number;
  predictedSamplesShown: number;
  durationMs: number;
  medianDtMs: number;
  maxDtMs: number;
  timeGaps: number;
  spatialJumps: number;
  commitMs: number;
}

export class Metrics {
  counters: Record<string, number> = {};
  private series = new Map<string, Series>();
  events: { name: string; t: number; [k: string]: unknown }[] = [];
  strokes: StrokeStats[] = [];
  errors: string[] = [];
  info: Record<string, unknown> = {};
  constructor(private cap = 8192) {}

  now(): number {
    return performance.now();
  }
  count(name: string, d = 1) {
    this.counters[name] = (this.counters[name] ?? 0) + d;
  }
  timing(name: string, ms: number) {
    let s = this.series.get(name);
    if (!s) this.series.set(name, (s = new Series(this.cap)));
    s.add(ms);
  }
  since(name: string, t0: number) {
    this.timing(name, performance.now() - t0);
  }
  event(name: string, detail: Record<string, unknown> = {}) {
    this.count(name);
    this.events.push({ name, t: performance.now(), ...detail });
    if (this.events.length > 512) this.events.shift();
  }
  error(msg: string) {
    this.errors.push(msg);
    if (this.errors.length > 64) this.errors.shift();
  }
  stroke(s: StrokeStats) {
    this.strokes.push(s);
    if (this.strokes.length > 512) this.strokes.shift();
  }
  get(name: string): SeriesSummary | undefined {
    const s = this.series.get(name);
    return s ? summarize(s.snapshot(), s.count) : undefined;
  }
  reset() {
    this.counters = {};
    this.series.clear();
    this.events = [];
    this.strokes = [];
    this.errors = [];
  }
  toJSON() {
    const timings: Record<string, SeriesSummary> = {};
    for (const [k, s] of this.series) timings[k] = summarize(s.snapshot(), s.count);
    return {
      clock: "performance.now",
      counters: this.counters,
      timings,
      events: this.events,
      strokes: this.strokes,
      errors: this.errors,
      info: this.info,
    };
  }
}

/** Gap analysis on delivered sample timestamps (same rule as Kotlin/Dart). */
export function analyzeSamples(ts: number[], xs: number[], ys: number[]) {
  const dts: number[] = [];
  for (let i = 1; i < ts.length; i++) dts.push(ts[i] - ts[i - 1]);
  const sorted = [...dts].sort((a, b) => a - b);
  const median = sorted.length ? sorted[Math.floor(sorted.length / 2)] : 0;
  const gapThreshold = Math.max(30, median * 4);
  let gaps = 0, jumps = 0;
  for (let i = 0; i < dts.length; i++) {
    if (dts[i] > gapThreshold) gaps++;
    if (Math.hypot(xs[i + 1] - xs[i], ys[i + 1] - ys[i]) > 40) jumps++;
  }
  return { medianDtMs: median, maxDtMs: sorted.length ? sorted[sorted.length - 1] : 0, timeGaps: gaps, spatialJumps: jumps };
}
