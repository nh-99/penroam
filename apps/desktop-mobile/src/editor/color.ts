// Colour values for tools and palettes. Notebooks store colours as `#rrggbb`
// (docs/formats/model-and-archive.md), so everything a user types or drags is reduced to that form
// here before it reaches a tool or a stroke.

/** A colour as the picker's sliders show it: hue 0–360, saturation and brightness 0–100. */
export interface Hsv {
  h: number;
  s: number;
  v: number;
}

/** Reads `#rgb`, `#rrggbb` or either without the `#`. Returns lowercase `#rrggbb`, or null if it isn't a colour. */
export function normalizeHex(input: string): string | null {
  const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(input.trim());
  if (!m) return null;
  const digits = m[1].toLowerCase();
  return `#${digits.length === 3 ? [...digits].map((d) => d + d).join("") : digits}`;
}

/** Converts slider values to `#rrggbb`. A hue of 360 is the same red as 0. */
export function hsvToHex({ h, s, v }: Hsv): string {
  const channel = (n: number) => {
    const k = (n + h / 60) % 6;
    const value = (v / 100) * (1 - (s / 100) * Math.max(0, Math.min(k, 4 - k, 1)));
    return Math.round(value * 255).toString(16).padStart(2, "0");
  };
  return `#${channel(5)}${channel(3)}${channel(1)}`;
}

/**
 * Converts a `#rrggbb` colour to slider values. They are left unrounded so that converting back
 * gives exactly the same colour. Greys and black have no hue and report 0.
 */
export function hexToHsv(hex: string): Hsv {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const max = Math.max(r, g, b);
  const delta = max - Math.min(r, g, b);
  let h = 0;
  if (delta > 0) {
    if (max === r) h = ((g - b) / delta + 6) % 6;
    else if (max === g) h = (b - r) / delta + 2;
    else h = (r - g) / delta + 4;
  }
  return { h: h * 60, s: max === 0 ? 0 : (delta / max) * 100, v: max * 100 };
}
