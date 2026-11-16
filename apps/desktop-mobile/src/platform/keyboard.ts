// How much of the bottom of the window the on-screen keyboard covers.
//
// Phones put the editor's toolbar at the bottom of the screen. An on-screen keyboard does not
// shrink the page there, it is drawn over it, so the toolbar (and its text formatting
// controls) would sit under the keyboard while you type. This module publishes the covered
// height as the `--pr-viewport-keyboard` CSS variable; `main.css` combines it with the height
// Android reports natively (`--pr-native-keyboard`, see `insets.ts`) into `--pr-keyboard`, and
// `.safe-bottom-keyboard` pads the toolbar above it.
//
// iOS WKWebView keeps the layout viewport and shrinks (and may pan) the visual viewport, so the
// covered height is what lies below the visual viewport. Android's edge-to-edge WebView sees no
// change at all, which is why the activity measures the keyboard itself. Only phones and
// tablets are measured: on a desktop a pinch zoom also shrinks the visual viewport.

import { isMobile } from "./device";

function measure() {
  const vv = window.visualViewport;
  if (!vv) return;
  // A zoomed visual viewport is smaller without anything covering it.
  const covered = Math.abs(vv.scale - 1) < 0.01 ? window.innerHeight - (vv.offsetTop + vv.height) : 0;
  // Ignore sub-pixel rounding and the few pixels some browsers leave for their own chrome.
  const px = covered > 40 ? Math.round(covered) : 0;
  document.documentElement.style.setProperty("--pr-viewport-keyboard", `${px}px`);
}

/** Tracks the on-screen keyboard on phones and tablets; does nothing elsewhere. */
export function startKeyboardTracking() {
  const vv = window.visualViewport;
  if (!vv || !isMobile()) return;
  vv.addEventListener("resize", measure);
  vv.addEventListener("scroll", measure);
  window.addEventListener("resize", measure);
  measure();
}
