// Canonical M00 stroke format (docs/architecture.md). Identical to the Dart
// (flutter_app/lib/ink/model.dart) and Kotlin (android/boox-ink InkModel.kt)
// representations.

export interface InkPoint {
  x: number; // document units (CSS px at zoom 1)
  y: number;
  pressure: number; // normalized 0..1 (mouse: 0.5)
  tiltX: number; // degrees, W3C convention
  tiltY: number;
  timeUs: number; // microseconds since first sample of the stroke
}

export interface InkBrush {
  type: "pen";
  width: number;
  color: string;
  pressure: boolean;
}

export interface InkInput {
  api: string; // "web-pointer" | "onyx-touchhelper" | "android-motionevent" | "generated" | ...
  device: string; // "pen" | "mouse" | "touch" | "fixture-stylus" | ...
  [extra: string]: unknown;
}

export interface InkStroke {
  id: string;
  source: string; // "stylus" | "mouse" | "touch" | "fixture"
  input: InkInput;
  brush: InkBrush;
  points: InkPoint[];
}

export interface InkDocument {
  format: "m00-strokes";
  version: 1;
  canvas?: { width: number; height: number };
  meta?: Record<string, unknown>;
  strokes: InkStroke[];
}

export function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `s-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function toDocument(strokes: InkStroke[], meta: Record<string, unknown> = {}): InkDocument {
  return { format: "m00-strokes", version: 1, meta, strokes };
}

/** Parses a document or bare stroke array, filling defaults for missing fields. */
export function parseDocument(text: string): InkStroke[] {
  const raw = JSON.parse(text);
  const arr: unknown[] = Array.isArray(raw) ? raw : (raw?.strokes ?? []);
  return arr.map((s: any) => ({
    id: String(s.id ?? newId()),
    source: String(s.source ?? "unknown"),
    input: { api: "unknown", device: "unknown", ...(s.input ?? {}) },
    brush: {
      type: "pen",
      width: Number(s.brush?.width ?? 3),
      color: String(s.brush?.color ?? "#000000"),
      pressure: s.brush?.pressure !== false,
    },
    points: (s.points ?? []).map((p: any) => ({
      x: Number(p.x),
      y: Number(p.y),
      pressure: Number(p.pressure ?? 0.5),
      tiltX: Number(p.tiltX ?? 0),
      tiltY: Number(p.tiltY ?? 0),
      timeUs: Number(p.timeUs ?? 0),
    })),
  }));
}
