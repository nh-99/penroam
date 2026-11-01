// Test environment: happy-dom has no canvas 2D backend, so canvases get a
// recording no-op context. Rendering correctness is covered by the geometry
// golden tests and the Rust/PDF render tests; these tests cover behaviour.
export const drawCalls: string[] = [];

function fakeContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const state: Record<string | symbol, unknown> = { canvas, globalAlpha: 1, lineWidth: 1 };
  return new Proxy(state, {
    get(t, k) {
      if (k in t) return t[k];
      if (k === "getImageData") return () => ({ data: new Uint8ClampedArray(4) });
      if (k === "measureText") return (s: string) => ({ width: s.length * 7 });
      return (..._a: unknown[]) => {
        if (k === "stroke" || k === "fill" || k === "fillText") drawCalls.push(String(k));
      };
    },
    set(t, k, v) {
      t[k] = v;
      return true;
    },
  }) as unknown as CanvasRenderingContext2D;
}

const ctxs = new WeakMap<HTMLCanvasElement, CanvasRenderingContext2D>();
(HTMLCanvasElement.prototype as any).getContext = function (this: HTMLCanvasElement) {
  let c = ctxs.get(this);
  if (!c) ctxs.set(this, (c = fakeContext(this)));
  return c;
};
(HTMLCanvasElement.prototype as any).toBlob = function (cb: (b: Blob | null) => void) {
  cb(new Blob([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], { type: "image/png" }));
};
if (!(globalThis as any).ResizeObserver) {
  (globalThis as any).ResizeObserver = class {
    observe() {}
    disconnect() {}
  };
}
