// System insets measured by the Android activity (`MainActivity.kt`).
//
// Android WebView does not reliably report the system bars through `env(safe-area-inset-*)`
// and never reports the bottom gesture area, so the activity measures them and passes them in
// as `--pr-native-inset-*` CSS variables. `main.css` uses the larger of those and `env()`.
// Elsewhere there is no bridge and the variables stay unset (0px).

type Insets = { top: number; right: number; bottom: number; left: number };

declare global {
  interface Window {
    PenroamInsets?: { get(): string };
    __penroamApplyInsets?: (insets: Insets) => void;
  }
}

function apply(insets: Insets) {
  const style = document.documentElement.style;
  for (const side of ["top", "right", "bottom", "left"] as const) {
    const v = Number(insets[side]);
    style.setProperty(`--pr-native-inset-${side}`, `${Number.isFinite(v) ? v : 0}px`);
  }
}

/** Applies the native insets now, and whenever the activity reports a change. */
export function startNativeInsets() {
  // Install the callback first so a change reported during startup isn't missed.
  window.__penroamApplyInsets = apply;
  try {
    const raw = window.PenroamInsets?.get();
    if (raw) apply(JSON.parse(raw) as Insets);
  } catch {
    /* no bridge, or a malformed value: keep env() alone */
  }
}
